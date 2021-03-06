//
// Copyright 2020 The Project Oak Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

use crate::{
    node::{grpc::codec::VecCodec, Node},
    runtime::RuntimeProxy,
};
use hyper::service::Service;
use log::{debug, error, info};
use oak_abi::{
    label::Label,
    proto::oak::encap::{GrpcRequest, GrpcResponse},
    ChannelReadStatus, OakStatus,
};
use prost::Message;
use std::{
    net::SocketAddr,
    task::{Context, Poll},
};
use tokio::sync::oneshot;
use tonic::{
    codegen::BoxFuture,
    server::{Grpc, UnaryService},
    transport::{Identity, NamedService},
};

/// Struct that represents a gRPC server pseudo-Node.
#[derive(Clone)]
pub struct GrpcServerNode {
    /// Pseudo-Node name.
    node_name: String,
    /// Server address to listen client requests on.
    address: SocketAddr,
    /// Loaded files containing a server TLS key and certificates.
    tls_identity: Identity,
}

impl GrpcServerNode {
    /// Creates a new [`GrpcServerNode`] instance, but does not start it.
    pub fn new(node_name: &str, address: SocketAddr, tls_identity: Identity) -> Self {
        Self {
            node_name: node_name.to_string(),
            address,
            tls_identity,
        }
    }

    /// Reads an [`oak_abi::Handle`] from a channel specified by `handle`.
    /// Returns an error if couldn't read from the channel or if received a wrong number of handles
    /// (not equal to 1).
    fn get_channel_writer(
        runtime: &RuntimeProxy,
        handle: oak_abi::Handle,
    ) -> Result<oak_abi::Handle, OakStatus> {
        let read_status = runtime.wait_on_channels(&[handle]).map_err(|error| {
            error!("Couldn't wait on the initial reader handle: {:?}", error);
            OakStatus::ErrInternal
        })?;

        let channel_writer = if read_status[0] == ChannelReadStatus::ReadReady {
            runtime
                .channel_read(handle)
                .map_err(|error| {
                    error!("Couldn't read from the initial reader handle {:?}", error);
                    OakStatus::ErrInternal
                })
                .and_then(|message| {
                    message
                        .ok_or_else(|| {
                            error!("Empty message");
                            OakStatus::ErrInternal
                        })
                        .and_then(|m| {
                            if m.handles.len() == 1 {
                                Ok(m.handles[0])
                            } else {
                                error!(
                                    "gRPC server pseudo-node should receive a single writer handle, found {}",
                                    m.handles.len()
                                );
                                Err(OakStatus::ErrInternal)
                            }
                        })
                })
        } else {
            error!("Couldn't read channel: {:?}", read_status[0]);
            Err(OakStatus::ErrInternal)
        }?;

        info!("Channel writer received: {:?}", channel_writer);
        Ok(channel_writer)
    }
}

/// Oak Node implementation for the gRPC server.
impl Node for GrpcServerNode {
    fn run(
        self: Box<Self>,
        runtime: RuntimeProxy,
        handle: oak_abi::Handle,
        notify_receiver: oneshot::Receiver<()>,
    ) {
        // Receive a `channel_writer` handle used to pass handles for temporary channels.
        info!("{}: Waiting for a channel writer", self.node_name);
        let channel_writer = GrpcServerNode::get_channel_writer(&runtime, handle)
            .expect("Couldn't initialize a channel writer");

        let handler = HttpRequestHandler {
            runtime,
            writer: channel_writer,
        };

        // Handles incoming TLS connections, unpacks HTTP/2 requests and forwards them to
        // [`HttpRequestHandler::handle`].
        let server = tonic::transport::Server::builder()
            .tls_config(tonic::transport::ServerTlsConfig::new().identity(self.tls_identity))
            .add_service(handler)
            .serve_with_shutdown(self.address, async {
                // Treat notification failure the same as a notification.
                let _ = notify_receiver.await;
            });

        // Create an Async runtime for executing futures.
        // https://docs.rs/tokio/
        let mut async_runtime = tokio::runtime::Builder::new()
            // Use simple scheduler that runs all tasks on the current-thread.
            // https://docs.rs/tokio/0.2.16/tokio/runtime/index.html#basic-scheduler
            .basic_scheduler()
            // Enables the I/O driver.
            // Necessary for using net, process, signal, and I/O types on the Tokio runtime.
            .enable_io()
            // Enables the time driver.
            // Necessary for creating a Tokio Runtime.
            .enable_time()
            .build()
            .expect("Couldn't create an Async runtime");

        // Start a gRPC server.
        info!(
            "{}: Starting a gRPC server pseudo-Node on: {}",
            self.node_name, self.address
        );
        let result = async_runtime.block_on(server);
        info!(
            "{}: Exiting gRPC server pseudo-Node thread {:?}",
            self.node_name, result
        );
        info!("{}: Exiting gRPC server pseudo-Node thread", self.node_name);
    }
}

/// [`HttpRequestHandler`] handles HTTP/2 requests from a client and sends HTTP/2 responses back.
#[derive(Clone)]
struct HttpRequestHandler {
    /// Reference to a Runtime that corresponds to a node that created a gRPC server pseudo-Node.
    runtime: RuntimeProxy,
    /// Channel handle used for writing gRPC invocations.
    writer: oak_abi::Handle,
}

/// Set a mandatory prefix for all gRPC requests processed by a gRPC pseudo-Node.
impl NamedService for HttpRequestHandler {
    const NAME: &'static str = "oak";
}

impl Service<http::Request<hyper::Body>> for HttpRequestHandler {
    type Response = http::Response<tonic::body::BoxBody>;
    type Error = http::Error;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    /// Decodes an unary gRPC request using a [`VecCodec`] and processes it with
    /// [`tonic::server::Grpc::unary`] and a [`GrpcRequestHandler`].
    fn call(&mut self, request: http::Request<hyper::Body>) -> Self::Future {
        let grpc_handler = GrpcRequestHandler::new(
            self.runtime.clone(),
            self.writer,
            request.uri().path().to_string(),
        );

        let method_name = request.uri().path().to_string();
        let metrics_data = self.runtime.metrics_data();
        let future = async move {
            debug!("Processing an HTTP/2 request: {:?}", request);
            let mut grpc_service = Grpc::new(VecCodec::default());
            let response = grpc_service.unary(grpc_handler, request).await;
            debug!("Sending an HTTP/2 response: {:?}", response);
            let stc = format!("{}", response.status());
            metrics_data
                .grpc_server_metrics
                .grpc_server_handled_total
                .with_label_values(&[&method_name, &stc])
                .inc();
            Ok(response)
        };

        Box::pin(future)
    }
}

/// [`GrpcRequestHandler`] handles gRPC requests and generates gRPC responses.
#[derive(Clone)]
struct GrpcRequestHandler {
    /// Reference to a Runtime that corresponds to the Node that created a gRPC server pseudo-Node.
    runtime: RuntimeProxy,
    /// Channel handle used for writing gRPC invocations.
    writer: oak_abi::Handle,
    /// Name of the gRPC method that should be invoked.
    method_name: String,
}

impl UnaryService<Vec<u8>> for GrpcRequestHandler {
    type Response = Vec<u8>;
    type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

    fn call(&mut self, request: tonic::Request<Vec<u8>>) -> Self::Future {
        let handler = self.clone();
        let metrics_data = handler.runtime.metrics_data();
        let future = async move {
            debug!("Processing a gRPC request: {:?}", request);
            metrics_data
                .grpc_server_metrics
                .grpc_server_started_total
                .with_label_values(&[&handler.method_name])
                .inc();
            let timer = metrics_data
                .grpc_server_metrics
                .grpc_server_handled_latency_seconds
                .with_label_values(&[&handler.method_name])
                .start_timer();

            // Create a gRPC request.
            // TODO(#953): Add streaming support.
            let grpc_request = GrpcRequest {
                method_name: handler.method_name.to_string(),
                req_msg: request.into_inner(),
                last: true,
            };

            let response = handler
                // Handle a gRPC request and send it into the Runtime.
                .handle_grpc_request(grpc_request)
                // Read a gRPC response from the Runtime.
                .and_then(|response_reader| handler.handle_grpc_response(response_reader))
                // Convert an error to a gRPC error status without sending clients descriptions for
                // internal errors.
                // Errors are logged inside inside [`GrpcRequestHandler::handle_grpc_request`] and
                // [`GrpcRequestHandler::handle_grpc_response`].
                .map_err(|_| tonic::Status::new(tonic::Code::Internal, ""))?;

            // Send a gRPC response back to the client.
            debug!("Sending a gRPC response: {:?}", response);
            timer.observe_duration();
            Ok(tonic::Response::new(response))
        };

        Box::pin(future)
    }
}

impl GrpcRequestHandler {
    fn new(runtime: RuntimeProxy, writer: oak_abi::Handle, method_name: String) -> Self {
        Self {
            runtime,
            writer,
            method_name,
        }
    }

    /// Handles a gRPC request, forwards it to a temporary channel and sends handles for this
    /// channel to the [`GrpcRequestHandler::writer`].
    /// Returns an [`oak_abi::Handle`] for reading a gRPC response from.
    fn handle_grpc_request(&self, request: GrpcRequest) -> Result<oak_abi::Handle, OakStatus> {
        // Create a pair of temporary channels to pass the gRPC request and to receive the response.
        let (request_writer, request_reader) =
            self.runtime.channel_create(&Label::public_trusted())?;
        let (response_writer, response_reader) =
            self.runtime.channel_create(&Label::public_trusted())?;

        // Create an invocation message and attach the method-invocation specific channels to it.
        //
        // This message should be in sync with the [`oak::grpc::Invocation`] from the Oak SDK:
        // the order of the `request_reader` and `response_writer` must be consistent.
        let invocation = crate::NodeMessage {
            data: vec![],
            handles: vec![request_reader, response_writer],
        };

        // Serialize gRPC request into a message.
        let mut message = crate::NodeMessage {
            data: vec![],
            handles: vec![],
        };
        request.encode(&mut message.data).map_err(|error| {
            error!("Couldn't serialize a GrpcRequest message: {}", error);
            OakStatus::ErrInternal
        })?;

        // Send a message to the temporary channel.
        self.runtime
            .channel_write(request_writer, message)
            .map_err(|error| {
                error!(
                    "Couldn't write a message to the temporary channel: {:?}",
                    error
                );
                error
            })?;

        // Send an invocation message (with attached handles) to the Oak Node.
        self.runtime
            .channel_write(self.writer, invocation)
            .map_err(|error| {
                error!("Couldn't write a gRPC invocation message: {:?}", error);
                error
            })?;

        Ok(response_reader)
    }

    /// Processes a gRPC response from a channel represented by `response_reader` and returns an
    /// HTTP response body.
    fn handle_grpc_response(&self, response_reader: oak_abi::Handle) -> Result<Vec<u8>, OakStatus> {
        let read_status = self
            .runtime
            .wait_on_channels(&[response_reader])
            .map_err(|error| {
                error!("Couldn't wait on the temporary gRPC channel: {:?}", error);
                error
            })?;

        if read_status[0] == ChannelReadStatus::ReadReady {
            self.runtime
                .channel_read(response_reader)
                .map_err(|error| {
                    error!("Couldn't read from the temporary gRPC channel: {:?}", error);
                    error
                })
                .map(|message| {
                    // Return an empty HTTP body if the `message` is None.
                    message.map_or(vec![], |m| {
                        self.runtime
                            .metrics_data()
                            .grpc_server_metrics
                            .grpc_response_size_bytes
                            .with_label_values(&[&self.method_name])
                            .observe(m.data.len() as f64);
                        m.data
                    })
                })
                .and_then(|response| {
                    // Return the serialized message body.
                    GrpcResponse::decode(response.as_slice())
                        .map_err(|error| {
                            error!("Couldn't parse the GrpcResponse message: {}", error);
                            OakStatus::ErrInternal
                        })
                        .map(|message| message.rsp_msg)
                })
        } else {
            error!(
                "Couldn't read from the temporary gRPC channel: {:?}",
                read_status[0]
            );
            Err(OakStatus::ErrInternal)
        }
    }
}
