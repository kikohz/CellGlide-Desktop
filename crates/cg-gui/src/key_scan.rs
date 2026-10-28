//! Every key the window's code names is listed in `cg_i18n::KEYS` and has both languages.
//! Keys must be written as literals (`tr("settings.proxy", …)`), never built with `format!`.

use std::path::{Path, PathBuf};

const NAMESPACES: &[&str] = &[
    "nav", "gui", "panel", "overview", "speed", "leak", "history", "diag", "settings", "about",
    "common", "tray", "phone",
];

/// File names that look like keys (`gui.json`) are not keys.
const FILE_SUFFIXES: &[&str] = &[".json", ".desktop", ".svg", ".rs", ".txt", ".yml"];

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

/// The contents of every string literal, plain (`"…"`, escapes respected) or raw (`r#"…"#`).
/// Char literals such as `'"'` are skipped so they can't pair with a later quote; lifetimes
/// (`'a`) are left alone.
fn literals(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let after_ident = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        match bytes[i] {
            b'\'' => {
                if bytes.get(i + 1) == Some(&b'\\') {
                    // '\n', '\'', '\u{..}': up to the quote after the escaped character.
                    let from = (i + 3).min(bytes.len());
                    i = source[from..]
                        .find('\'')
                        .map_or(bytes.len(), |p| from + p + 1);
                } else if let Some(c) = source[i + 1..].chars().next()
                    && source[i + 1 + c.len_utf8()..].starts_with('\'')
                {
                    i += 2 + c.len_utf8();
                } else {
                    // A lifetime or label.
                    i += 1;
                }
            }
            b'r' if !after_ident && matches!(bytes.get(i + 1), Some(b'"' | b'#')) => {
                let hashes = bytes[i + 1..].iter().take_while(|&&b| b == b'#').count();
                let open = i + 1 + hashes;
                if bytes.get(open) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let closing = format!("\"{}", "#".repeat(hashes));
                let Some(len) = source[open + 1..].find(&closing) else {
                    break;
                };
                found.push(&source[open + 1..open + 1 + len]);
                i = open + 1 + len + closing.len();
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                if j >= bytes.len() {
                    break;
                }
                found.push(&source[start..j]);
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    found
}

fn is_key(literal: &str) -> bool {
    let Some((ns, rest)) = literal.split_once('.') else {
        return false;
    };
    NAMESPACES.contains(&ns)
        && !rest.is_empty()
        && literal.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
        && !FILE_SUFFIXES.iter().any(|s| literal.ends_with(s))
}

fn all_sources() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() > 3, "no sources found");
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            (p, text)
        })
        .collect()
}

#[test]
fn every_key_the_gui_uses_is_listed() {
    let _guard = cg_i18n::lock_for_tests();
    let mut seen = 0;
    // This file names keys only as examples.
    let this = Path::new(file!()).file_name().unwrap();
    for (path, text) in all_sources() {
        if path.file_name() == Some(this) {
            continue;
        }
        for literal in literals(&text).into_iter().filter(|l| is_key(l)) {
            seen += 1;
            let where_ = path.display();
            assert!(
                cg_i18n::KEYS.contains(&literal),
                "{literal} in {where_} is not in KEYS"
            );
            for lang in [cg_i18n::Lang::ZhCn, cg_i18n::Lang::En] {
                assert_ne!(
                    cg_i18n::tr_in(lang, literal, &[]),
                    literal,
                    "{literal} has no {lang:?} text"
                );
            }
        }
    }
    assert!(seen > 10, "the scan found only {seen} keys");
}

#[test]
fn the_scan_skips_file_names_and_other_strings() {
    assert!(is_key("nav.overview"));
    assert!(is_key("gui.state.connected.title"));
    assert!(!is_key("gui.json"));
    assert!(!is_key("cellglide.desktop"));
    assert!(!is_key("cli.unexpected_reply"));
    assert!(!is_key("CellGlide"));
    assert!(!is_key("nav."));
    assert_eq!(literals(r#"tr("a.b", &[]) + "c""#), ["a.b", "c"]);
    assert_eq!(literals(r#""x\"y" "nav.about""#), [r#"x\"y"#, "nav.about"]);
}

#[test]
fn keys_after_char_literals_and_raw_strings_are_still_found() {
    let source = r##"
        let q = '"'; let e = '\''; let b = b'"'; fn f<'a>(x: &'a str) {}
        let r = r#"say "hi" here"#; let s = r"plain";
        tr("nav.history", &[]);
    "##;
    assert_eq!(
        literals(source),
        [r#"say "hi" here"#, "plain", "nav.history"]
    );
}

#[test]
fn gui_never_names_the_unbound_route() {
    let word = ["Un", "bound"].concat();
    for (path, text) in all_sources() {
        assert!(!text.contains(&word), "{} names {word}", path.display());
    }
}
