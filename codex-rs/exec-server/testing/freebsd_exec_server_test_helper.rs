//! Cargo integration-test dispatcher for targets without pre-main Rust arguments.

#[cfg(target_os = "freebsd")]
#[allow(dead_code)]
#[path = "../tests/common/mod.rs"]
mod common;

fn main() {
    #[cfg(target_os = "freebsd")]
    let _guard = std::sync::LazyLock::force(&common::TEST_BINARY_DISPATCH_GUARD);
}
