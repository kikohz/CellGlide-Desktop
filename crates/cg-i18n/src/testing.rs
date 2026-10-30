//! For tests in other crates: every key a crate's code names is listed in [`crate::KEYS`] and
//! has both languages. Keys must be written as literals (`tr("settings.proxy", …)`), never
//! built with `format!`.

use std::path::{Path, PathBuf};

/// The namespaces of the window's keys; a literal in one of them is taken for a key.
pub const NAMESPACES: &[&str] = &[
    "nav", "gui", "panel", "overview", "speed", "leak", "history", "diag", "settings", "about",
    "common", "tray", "phone",
];

/// File and host names that look like keys (`gui.json`, `speed.cloudflare.com`) are not keys.
const FILE_SUFFIXES: &[&str] = &[".json", ".desktop", ".svg", ".rs", ".txt", ".yml", ".com"];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
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

/// Scans the `.rs` files under `dirs` (recursively) for string literals in [`NAMESPACES`] and
/// returns each, with its file, that is not in [`crate::KEYS`] or lacks a language. Panics when
/// a directory can't be read or the scan finds almost no keys, so a wrong path never passes.
pub fn unlisted_keys(dirs: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let _guard = crate::lock_for_tests();
    let mut files = Vec::new();
    for dir in dirs {
        sources(dir, &mut files);
    }
    assert!(!files.is_empty(), "no sources found in {dirs:?}");
    let mut seen = 0;
    let mut unlisted = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        for literal in literals(&text).into_iter().filter(|l| is_key(l)) {
            seen += 1;
            let listed = crate::KEYS.contains(&literal)
                && [crate::Lang::ZhCn, crate::Lang::En]
                    .into_iter()
                    .all(|lang| crate::tr_in(lang, literal, &[]) != literal);
            if !listed {
                unlisted.push((path.clone(), literal.to_string()));
            }
        }
    }
    assert!(seen > 10, "the scan found only {seen} keys in {dirs:?}");
    unlisted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_skips_file_names_and_other_strings() {
        assert!(is_key("nav.overview"));
        assert!(is_key("gui.state.connected.title"));
        assert!(!is_key("gui.json"));
        assert!(!is_key("cellglide.desktop"));
        assert!(!is_key("speed.cloudflare.com"));
        assert!(!is_key("leak.cellglide.com"));
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
    fn a_missing_key_is_reported_with_its_file() {
        let dir = std::env::temp_dir().join(format!("cg-i18n-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let listed: Vec<String> = crate::KEYS
            .iter()
            .filter(|k| is_key(k))
            .take(11)
            .map(|k| format!("tr(\"{k}\", &[]);\n"))
            .collect();
        std::fs::write(dir.join("a.rs"), listed.concat()).unwrap();
        let file = dir.join("sub").join("b.rs");
        std::fs::write(&file, "tr(\"nav.no_such_key\", &[]); \"gui.json\";").unwrap();
        let found = unlisted_keys(std::slice::from_ref(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found, [(file, "nav.no_such_key".to_string())]);
    }
}
