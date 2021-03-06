/*
 * Copyright 2019 The Project Oak Authors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#include "oak/server/storage/storage_node.h"

#include "absl/memory/memory.h"
#include "absl/strings/escaping.h"
#include "grpcpp/create_channel.h"
#include "oak/common/logging.h"
#include "oak/proto/grpc_encap.pb.h"
#include "oak/proto/storage_service.pb.h"
#include "oak/server/invocation.h"
#include "third_party/asylo/cleansing_types.h"
#include "third_party/asylo/status_macros.h"

using ::oak_abi::OakStatus;

namespace oak {

StorageNode::StorageNode(const std::string& name, NodeId node_id,
                         const std::string& storage_address)
    : OakNode(name, node_id), storage_processor_(storage_address) {}

void StorageNode::Run(Handle invocation_handle) {
  std::vector<std::unique_ptr<ChannelStatus>> channel_status;
  channel_status.push_back(absl::make_unique<ChannelStatus>(invocation_handle));
  while (true) {
    if (!WaitOnChannels(&channel_status)) {
      OAK_LOG(WARNING) << "Node termination requested";
      return;
    }

    std::unique_ptr<Invocation> invocation(Invocation::ReceiveFromChannel(this, invocation_handle));
    if (invocation == nullptr) {
      OAK_LOG(ERROR) << "Failed to create invocation";
      return;
    }

    // Expect to read a single request out of the request channel.
    NodeReadResult req_result = ChannelRead(invocation->req_handle.get());
    if (req_result.status != OakStatus::OK) {
      OAK_LOG(ERROR) << "Failed to read message: " << req_result.status;
      return;
    }
    if (req_result.msg->handles.size() != 0) {
      OAK_LOG(ERROR) << "Unexpectedly received channel handles in request channel";
      return;
    }
    oak::encap::GrpcRequest grpc_req;
    grpc_req.ParseFromString(std::string(req_result.msg->data.data(), req_result.msg->data.size()));

    std::unique_ptr<oak::encap::GrpcResponse> grpc_rsp;
    oak::StatusOr<std::unique_ptr<oak::encap::GrpcResponse>> rsp_or = ProcessMethod(&grpc_req);
    if (!rsp_or.ok()) {
      OAK_LOG(ERROR) << "Failed to perform " << grpc_req.method_name() << ": "
                     << rsp_or.status().code() << ", '" << rsp_or.status().message() << "'";
      grpc_rsp = absl::make_unique<oak::encap::GrpcResponse>();
      grpc_rsp->mutable_status()->set_code(static_cast<int>(rsp_or.status().code()));
      grpc_rsp->mutable_status()->set_message(std::string(rsp_or.status().message()));
    } else {
      grpc_rsp = std::move(rsp_or).ValueOrDie();
    }

    grpc_rsp->set_last(true);
    auto rsp_msg = absl::make_unique<NodeMessage>();
    size_t serialized_size = grpc_rsp->ByteSizeLong();
    rsp_msg->data.resize(serialized_size);
    grpc_rsp->SerializeToArray(rsp_msg->data.data(), rsp_msg->data.size());
    ChannelWrite(invocation->rsp_handle.get(), std::move(rsp_msg));

    // The response channel reference is dropped here.
  }
}

oak::StatusOr<std::unique_ptr<oak::encap::GrpcResponse>> StorageNode::ProcessMethod(
    oak::encap::GrpcRequest* grpc_req) {
  auto grpc_rsp = absl::make_unique<oak::encap::GrpcResponse>();
  grpc::Status status;
  std::string method_name = grpc_req->method_name();

  if (method_name == "/oak.storage.StorageService/Read") {
    oak::storage::StorageChannelReadRequest read_req;
    if (!read_req.ParseFromString(grpc_req->req_msg())) {
      return absl::Status(absl::StatusCode::kInvalidArgument, "Failed to parse request");
    }
    CleansingBytes item_name(read_req.item().name().begin(), read_req.item().name().end());
    CleansingBytes value;
    OAK_ASSIGN_OR_RETURN(value, storage_processor_.Read(read_req.storage_name(), item_name,
                                                        read_req.transaction_id()));
    oak::storage::StorageChannelReadResponse read_rsp;
    read_rsp.mutable_item()->ParseFromArray(value.data(), value.size());
    // TODO(#449): Check security policy for item.
    read_rsp.SerializeToString(grpc_rsp->mutable_rsp_msg());

  } else if (method_name == "/oak.storage.StorageService/Write") {
    oak::storage::StorageChannelWriteRequest write_req;
    if (!write_req.ParseFromString(grpc_req->req_msg())) {
      return absl::Status(absl::StatusCode::kInvalidArgument, "Failed to parse request");
    }
    // TODO(#449): Check integrity policy for item.
    CleansingBytes item(write_req.item().ByteSizeLong());
    if (!write_req.item().SerializeToArray(reinterpret_cast<void*>(item.data()), item.size())) {
      return absl::Status(absl::StatusCode::kInvalidArgument, "Failed to serialize item");
    }

    CleansingBytes item_name(write_req.item().name().begin(), write_req.item().name().end());
    OAK_RETURN_IF_ERROR(storage_processor_.Write(write_req.storage_name(), item_name, item,
                                                 write_req.transaction_id()));

  } else if (method_name == "/oak.storage.StorageService/Delete") {
    oak::storage::StorageChannelDeleteRequest delete_req;
    if (!delete_req.ParseFromString(grpc_req->req_msg())) {
      return absl::Status(absl::StatusCode::kInvalidArgument, "Failed to parse request");
    }
    // TODO(#449): Check integrity policy for item.
    CleansingBytes item_name(delete_req.item().name().begin(), delete_req.item().name().end());
    OAK_RETURN_IF_ERROR(storage_processor_.Delete(delete_req.storage_name(), item_name,
                                                  delete_req.transaction_id()));

  } else if (method_name == "/oak.storage.StorageService/Begin") {
    oak::storage::StorageChannelBeginRequest begin_req;
    if (!begin_req.ParseFromString(grpc_req->req_msg())) {
      return absl::Status(absl::StatusCode::kInvalidArgument, "Failed to parse request");
    }
    oak::storage::StorageChannelBeginResponse begin_rsp;
    std::string transaction_id;
    OAK_ASSIGN_OR_RETURN(transaction_id, storage_processor_.Begin(begin_req.storage_name()));
    begin_rsp.set_transaction_id(transaction_id);
    begin_rsp.SerializeToString(grpc_rsp->mutable_rsp_msg());

  } else if (method_name == "/oak.storage.StorageService/Commit") {
    oak::storage::StorageChannelCommitRequest commit_req;
    if (!commit_req.ParseFromString(grpc_req->req_msg())) {
      return absl::Status(absl::StatusCode::kInvalidArgument, "Failed to parse request");
    }
    OAK_RETURN_IF_ERROR(
        storage_processor_.Commit(commit_req.storage_name(), commit_req.transaction_id()));

  } else if (method_name == "/oak.storage.StorageService/Rollback") {
    oak::storage::StorageChannelRollbackRequest rollback_req;
    if (!rollback_req.ParseFromString(grpc_req->req_msg())) {
      return absl::Status(absl::StatusCode::kInvalidArgument, "Failed to parse request");
    }
    OAK_RETURN_IF_ERROR(
        storage_processor_.Rollback(rollback_req.storage_name(), rollback_req.transaction_id()));
  } else {
    OAK_LOG(ERROR) << "unknown operation " << method_name;
    return absl::Status(absl::StatusCode::kInvalidArgument, "Unknown operation request method.");
  }
  return std::move(grpc_rsp);
}

}  // namespace oak
