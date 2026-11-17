//! `cellglide wireless forget|regenerate` without a terminal and without `--yes` refuses before
//! it talks to the service (here there is none: a refusal about the service would be a different
//! message).

use std::process::{Command, Stdio};

#[test]
fn forget_and_regenerate_refuse_before_contacting_the_service() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["wireless", "forget", "1"][..],
        &["wireless", "regenerate"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cellglide"))
            .arg("--socket")
            .arg(dir.path().join("none.sock"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(stderr.contains("--yes"), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}
