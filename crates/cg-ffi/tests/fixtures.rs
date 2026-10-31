//! The contract with the C# side: one JSON file per sample command and event in
//! `tests/fixtures/`, which the C# tests read too. The samples must serialize to exactly
//! these bytes and read back as themselves. `CG_FFI_BLESS=1` writes the files instead, only
//! when a field changes on purpose (say why in the commit message).

use cellglide_native::api::{command_samples, event_samples};
use cellglide_native::testing::plain_shape;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::path::{Path, PathBuf};

fn en() -> std::sync::MutexGuard<'static, ()> {
    let guard = cg_i18n::lock_for_tests();
    cg_i18n::set(cg_i18n::Lang::En);
    guard
}

fn dir(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(kind)
}

fn check<T: Serialize + DeserializeOwned + PartialEq + Debug>(kind: &str, samples: Vec<(&str, T)>) {
    let bless = std::env::var_os("CG_FFI_BLESS").is_some();
    if bless {
        std::fs::create_dir_all(dir(kind)).unwrap();
    }
    let names: BTreeSet<String> = samples.iter().map(|(n, _)| format!("{n}.json")).collect();
    for (name, value) in samples {
        let path = dir(kind).join(format!("{name}.json"));
        let text = serde_json::to_string_pretty(&value).unwrap() + "\n";
        if bless {
            std::fs::write(&path, &text).unwrap();
            continue;
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{path:?}");
        assert_eq!(serde_json::from_str::<T>(&text).unwrap(), value, "{path:?}");
    }
    let on_disk: BTreeSet<String> = std::fs::read_dir(dir(kind))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        on_disk, names,
        "fixtures without a sample, or samples without a file"
    );
}

#[test]
fn command_fixtures_match() {
    let _g = en();
    check("commands", command_samples());
}

#[test]
fn event_fixtures_match() {
    let _g = en();
    check("events", event_samples());
}

#[test]
fn samples_are_plain_records() {
    let _g = en();
    for (name, command) in command_samples() {
        assert_eq!(plain_shape(&command), Ok(()), "{name}");
    }
    for (name, event) in event_samples() {
        assert_eq!(plain_shape(&event), Ok(()), "{name}");
    }
}

/// Every place that is `null` in some sample holds a value in another, so the C# side reads
/// and writes every field at least once. A place is the path of keys from the top, with the
/// tags (`type`, `cmd`, `status`, `kind`) of the objects on the way and `[]` for any element.
#[test]
fn every_option_is_filled_somewhere() {
    let _g = en();
    let mut seen: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    let values = command_samples()
        .into_iter()
        .map(|(_, c)| serde_json::to_value(c).unwrap())
        .chain(
            event_samples()
                .into_iter()
                .map(|(_, e)| serde_json::to_value(e).unwrap()),
        );
    for value in values {
        walk(&value, String::new(), &mut seen);
    }
    let never: Vec<&String> = seen
        .iter()
        .filter(|(_, (null, filled))| *null && !*filled)
        .map(|(path, _)| path)
        .collect();
    assert!(never.is_empty(), "always null: {never:#?}");
}

fn walk(value: &Value, path: String, seen: &mut BTreeMap<String, (bool, bool)>) {
    let entry = seen.entry(path.clone()).or_default();
    match value {
        Value::Null => entry.0 = true,
        Value::Array(items) => {
            entry.1 = true;
            for item in items {
                walk(item, format!("{path}[]"), seen);
            }
        }
        Value::Object(fields) => {
            entry.1 = true;
            let mut path = path;
            for tag in ["type", "cmd", "status", "kind"] {
                if let Some(Value::String(name)) = fields.get(tag) {
                    path = format!("{path}<{name}>");
                }
            }
            for (key, field) in fields {
                walk(field, format!("{path}.{key}"), seen);
            }
        }
        _ => entry.1 = true,
    }
}

/// No sample says anything only one system says (`tr_os` picks `<key>_windows` on Windows),
/// so the fixtures are the same bytes on Linux and Windows: neither text of such a key shows.
#[test]
fn fixtures_read_the_same_on_every_system() {
    let _g = en();
    let json: String = command_samples()
        .into_iter()
        .map(|(_, c)| serde_json::to_string(&c).unwrap())
        .chain(
            event_samples()
                .into_iter()
                .map(|(_, e)| serde_json::to_string(&e).unwrap()),
        )
        .collect();
    let variants = cg_i18n::KEYS
        .iter()
        .filter_map(|key| key.strip_suffix("_windows"));
    for base in variants {
        for key in [base.to_string(), format!("{base}_windows")] {
            let text = cg_i18n::tr(&key, &[]);
            let fragment = literal_part(&text);
            let quoted = serde_json::to_string(fragment).unwrap();
            let quoted = &quoted[1..quoted.len() - 1];
            assert!(!json.contains(quoted), "{key}: {fragment}");
        }
    }
}

/// The longest stretch of `text` without a `%{placeholder}`.
fn literal_part(text: &str) -> &str {
    let mut pieces = vec![];
    for (i, piece) in text.split("%{").enumerate() {
        pieces.push(if i == 0 {
            piece
        } else {
            piece.split_once('}').map_or("", |(_, rest)| rest)
        });
    }
    pieces.into_iter().max_by_key(|p| p.len()).unwrap_or(text)
}
