//! The window's text keys, read from the sources (no window is built, so this runs everywhere).

use std::path::{Path, PathBuf};

#[test]
fn every_key_the_window_names_is_listed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dirs = [
        root.join("src"),
        root.join("../cg-ipc/src/describe"),
        root.join("../cg-tools/src"),
        root.join("../cg-diag/src"),
    ];
    assert_eq!(cg_i18n::testing::unlisted_keys(&dirs), Vec::new());
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn gui_never_names_the_unbound_route() {
    let word = ["Un", "bound"].concat();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in [root.join("src"), root.join("../cg-ipc/src/describe")] {
        sources(&dir, &mut files);
    }
    assert!(files.len() > 3, "no sources found");
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&word), "{} names {word}", path.display());
    }
}
