//! Silences LNK4099 (missing `ossl_static.pdb`) for the artifacts of this crate that link the
//! vendored OpenSSL (feature `wireless`) with the MSVC linker. Only that one warning is ignored.
//! A binary in another crate that enables the feature has to pass the same flag itself.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let wireless = std::env::var_os("CARGO_FEATURE_WIRELESS").is_some();
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if wireless && msvc {
        println!("cargo:rustc-link-arg-examples=/ignore:4099");
        println!("cargo:rustc-link-arg-tests=/ignore:4099");
    }
}
