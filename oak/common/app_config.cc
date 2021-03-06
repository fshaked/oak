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

#include "oak/common/app_config.h"

#include <set>
#include <utility>

#include "absl/memory/memory.h"
#include "oak/common/logging.h"
#include "oak/common/utils.h"

using ::oak::application::ApplicationConfiguration;
using ::oak::application::NodeConfiguration;

namespace oak {

namespace {

constexpr int16_t kDefaultGrpcPort = 8080;

// Conventional names for the configuration of Nodes.
constexpr char kAppConfigName[] = "app";
constexpr char kAppEntrypointName[] = "oak_main";
constexpr char kLogConfigName[] = "log";
constexpr char kStorageConfigName[] = "storage";
constexpr char kGrpcClientConfigName[] = "grpc-client";

}  // namespace

std::unique_ptr<ApplicationConfiguration> DefaultConfig(const std::string& module_bytes) {
  auto config = absl::make_unique<ApplicationConfiguration>();
  config->set_grpc_port(kDefaultGrpcPort);

  config->set_initial_node_config_name(kAppConfigName);
  NodeConfiguration* node_config = config->add_node_configs();
  node_config->set_name(kAppConfigName);
  application::WebAssemblyConfiguration* code = node_config->mutable_wasm_config();
  code->set_module_bytes(module_bytes);
  config->set_initial_entrypoint_name(kAppEntrypointName);

  return config;
}

std::unique_ptr<ApplicationConfiguration> ReadConfigFromFile(const std::string& filename) {
  auto config = absl::make_unique<ApplicationConfiguration>();

  std::string data = utils::read_file(filename);
  config->ParseFromString(data);

  return config;
}

void WriteConfigToFile(const ApplicationConfiguration* config, const std::string& filename) {
  std::string data = config->SerializeAsString();
  utils::write_file(data, filename);
}

void AddLoggingToConfig(ApplicationConfiguration* config) {
  NodeConfiguration* node_config = config->add_node_configs();
  node_config->set_name(kLogConfigName);
  node_config->mutable_log_config();
}

void AddStorageToConfig(ApplicationConfiguration* config, const std::string& storage_address) {
  NodeConfiguration* node_config = config->add_node_configs();
  node_config->set_name(kStorageConfigName);
  application::StorageProxyConfiguration* storage = node_config->mutable_storage_config();
  storage->set_address(storage_address);
}

void AddGrpcClientToConfig(ApplicationConfiguration* config, const std::string& grpc_address) {
  NodeConfiguration* node_config = config->add_node_configs();
  node_config->set_name(kGrpcClientConfigName);
  application::GrpcClientConfiguration* grpc_config = node_config->mutable_grpc_client_config();
  grpc_config->set_address(grpc_address);
}

void SetGrpcPortInConfig(ApplicationConfiguration* config, const int16_t grpc_port) {
  config->set_grpc_port(grpc_port);
}

bool ValidApplicationConfig(const ApplicationConfiguration& config) {
  // Check for valid port.
  if (config.grpc_port() <= 1023) {
    OAK_LOG(ERROR) << "Invalid gRPC port";
    return false;
  }

  // Check name uniqueness for NodeConfiguration.
  std::set<std::string> config_names;
  std::set<std::string> wasm_names;
  for (const auto& node_config : config.node_configs()) {
    if (config_names.count(node_config.name()) > 0) {
      OAK_LOG(ERROR) << "duplicate node config name " << node_config.name();
      return false;
    }
    config_names.insert(node_config.name());
    if (node_config.has_wasm_config()) {
      wasm_names.insert(node_config.name());
    }
  }

  // Check name for the config of the initial node is present and is a Web
  // Assembly variant.
  if (wasm_names.count(config.initial_node_config_name()) == 0) {
    OAK_LOG(ERROR) << "config of the initial node is not present in Wasm";
    return false;
  }
  if (config.initial_entrypoint_name().empty()) {
    OAK_LOG(ERROR) << "missing entrypoint name";
    return false;
  }
  return true;
}

}  // namespace oak
