//! `cellglide status | head -1`: a reader that goes away early ends the CLI quietly.

#![cfg(unix)]

use std::process::{Command, Stdio};

#[test]
fn closed_stdout_exits_quietly() {
    let dir = tempfile::tempdir().unwrap();
    // The reading end is closed before the CLI starts, so its first write gets EPIPE.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    // `doctor` prints without a service: here the socket doesn't exist.
    let output = Command::new(env!("CARGO_BIN_EXE_cellglide"))
        .arg("--socket")
        .arg(dir.path().join("none.sock"))
        .arg("doctor")
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(output.status.code(), Some(0), "{stderr}");
}
