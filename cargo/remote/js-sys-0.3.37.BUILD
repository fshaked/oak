"""
cargo-raze crate build file.

DO NOT EDIT! Replaced on runs of cargo-raze
"""
package(default_visibility = [
  # Public for visibility by "@raze__crate__version//" targets.
  #
  # Prefer access through "//cargo", which limits external
  # visibility to explicit Cargo.toml dependencies.
  "//visibility:public",
])

licenses([
  "notice", # "MIT,Apache-2.0"
])

load(
    "@io_bazel_rules_rust//rust:rust.bzl",
    "rust_library",
    "rust_binary",
    "rust_test",
)


# Unsupported target "headless" with type "test" omitted

rust_library(
    name = "js_sys",
    crate_root = "src/lib.rs",
    crate_type = "lib",
    edition = "2018",
    srcs = glob(["**/*.rs"]),
    deps = [
        "@raze__wasm_bindgen__0_2_60//:wasm_bindgen",
    ],
    rustc_flags = [
        "--cap-lints=allow",
    ],
    version = "0.3.37",
    crate_features = [
    ],
)

# Unsupported target "wasm" with type "test" omitted
