//! Builds hev-socks5-tunnel 2.18.0 (MIT) from the submodule as static libraries.
//! Only Linux links it; other targets build nothing.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=hev");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("hev");
    // Build a fresh copy every run so the submodule stays clean and no stale objects are reused.
    match fs::remove_dir_all(&out) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => panic!("removing old hev copy failed: {e}"),
    }
    let status = Command::new("cp")
        .arg("-a")
        .arg(manifest.join("hev"))
        .arg(&out)
        .status()
        .expect("cp");
    assert!(status.success(), "copying hev sources failed");
    let compiler = cc::Build::new().get_compiler();
    let jobs = env::var("NUM_JOBS").unwrap_or_else(|_| "4".into());
    let status = Command::new("make")
        .arg("-C")
        .arg(&out)
        .arg("static")
        .arg(format!("-j{jobs}"))
        .arg(format!("CC={}", compiler.path().display()))
        .status()
        .expect("make");
    assert!(status.success(), "building hev-socks5-tunnel failed");
    for dir in [
        "bin",
        "third-part/lwip/bin",
        "third-part/yaml/bin",
        "third-part/hev-task-system/bin",
    ] {
        println!("cargo:rustc-link-search=native={}", out.join(dir).display());
    }
    for lib in ["hev-socks5-tunnel", "lwip", "yaml", "hev-task-system"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
}
