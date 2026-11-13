//! For the vendored OpenSSL (feature `wireless`) with the MSVC toolchain:
//!
//! - Silences LNK4099 (missing `ossl_static.pdb`) for the artifacts of this crate that link it.
//!   Only that one warning is ignored. `rustc-link-arg` (not `-tests`/`-examples`) because the
//!   lib's own unit-test binary is neither an integration test nor an example; for the rlib itself
//!   the flag is unused. A binary in another crate that enables the feature has to pass the same
//!   flag itself.
//! - On aarch64, refuses to build unless OpenSSL is compiled with clang-cl through
//!   `scripts/openssl-clang-cl.cmd`. Built by cl (MSVC 14.51, VS 18, /O2, OpenSSL 3.6.3 C-only),
//!   every TLS handshake crashed with an access violation, client and server alike: CI on
//!   windows-11-arm, our client against `openssl s_server` and the loopback tests. The fault is
//!   in `tls_parse_all_extensions`, reached with the connection pointer equal to 1. The same
//!   sources pass when built with clang-cl, or with cl at /Od. openssl-src has no setting for the
//!   compiler; OpenSSL's Configure takes it from `CC`.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CC");
    let wireless = env::var_os("CARGO_FEATURE_WIRELESS").is_some();
    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|v| v == "msvc");
    let aarch64 = env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|v| v == "aarch64");
    if !(wireless && msvc) {
        return;
    }
    println!("cargo:rustc-link-arg=/ignore:4099");
    let cc = env::var("CC").unwrap_or_default();
    if aarch64 && !cc.to_ascii_lowercase().ends_with("openssl-clang-cl.cmd") {
        panic!(
            "feature `wireless` on aarch64-pc-windows-msvc: MSVC's ARM64 compiler builds an OpenSSL \
             whose TLS handshake crashes. Build OpenSSL with clang-cl: set CC to the full path of \
             scripts\\openssl-clang-cl.cmd and CC_aarch64_pc_windows_msvc=clang-cl (clang-cl on \
             PATH), then build again (CC is now {cc:?})."
        );
    }
}
