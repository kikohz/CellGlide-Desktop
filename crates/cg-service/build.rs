//! With the MSVC toolchain the service links the vendored OpenSSL (cg-platform's feature
//! `wireless`, on for Windows): silence LNK4099 (missing `ossl_static.pdb`) for its artifacts, the
//! binary and its unit tests. Only that one warning is ignored. cg-platform's own build script
//! refuses an aarch64 build whose OpenSSL is not compiled with clang-cl.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = env::var("CARGO_CFG_TARGET_OS").is_ok_and(|v| v == "windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|v| v == "msvc");
    if windows && msvc {
        println!("cargo:rustc-link-arg=/ignore:4099");
    }
}
