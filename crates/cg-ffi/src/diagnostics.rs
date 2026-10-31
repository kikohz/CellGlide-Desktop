//! `DiagnosticsSend` and `DiagnosticsSave`: the bundle the user reviewed goes to CellGlide, or
//! into a file. The words are the Linux window's.

use crate::api::{ReplyResult, ReplyValue};
use crate::runtime::Context;
use cg_diag::upload::UploadError;
use cg_i18n::tr;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::path::{Path, PathBuf};

/// `Command::DiagnosticsSend`: the report code, or why it wasn't sent.
pub(crate) async fn send(context: &Context, json: &str) -> ReplyResult {
    match cg_diag::upload::upload(json.as_bytes(), &context.config.diag_target).await {
        Ok(code) => ReplyResult::Ok {
            value: ReplyValue::Sent { code },
        },
        Err(UploadError::TooFrequent) => ReplyResult::err(tr("diag.too_frequent", &[])),
        Err(_) => ReplyResult::err(tr("diag.failed_gui", &[])),
    }
}

/// `Command::DiagnosticsSave`: to `path` (over an existing file only with `overwrite`, which the
/// user confirmed), or into the Downloads folder under a name of its own; the full path
/// written, or why not.
pub(crate) async fn save(
    context: &Context,
    json: String,
    path: Option<String>,
    overwrite: bool,
) -> ReplyResult {
    let downloads = context.config.download_dir.clone();
    let written = tokio::task::spawn_blocking(move || write(json, path, overwrite, downloads))
        .await
        .unwrap_or_else(|error| Err(tr("diag.save_failed", &[("reason", &error.to_string())])));
    match written {
        Ok(path) => ReplyResult::Ok {
            value: ReplyValue::Saved {
                path: path.to_string_lossy().into_owned(),
            },
        },
        Err(message) => ReplyResult::err(message),
    }
}

fn write(
    json: String,
    path: Option<String>,
    overwrite: bool,
    downloads: Option<PathBuf>,
) -> Result<PathBuf, String> {
    match (path, downloads) {
        (Some(path), _) if overwrite => cg_diag::save::write_over(Path::new(&path), &json),
        (Some(path), _) => cg_diag::save::write_to(Path::new(&path), &json),
        (None, Some(dir)) => {
            cg_diag::save::save_bundle(&dir, Timestamp::now(), &TimeZone::system(), &json)
        }
        (None, None) => Err(tr(
            "diag.save_failed",
            &[("reason", &tr("diag.no_home", &[]))],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_downloads_folder_saving_says_so() {
        let _g = cg_i18n::lock_for_tests();
        let error = write("{}".into(), None, false, None).unwrap_err();
        assert_eq!(
            error,
            tr("diag.save_failed", &[("reason", &tr("diag.no_home", &[]))])
        );
    }

    #[test]
    fn a_chosen_path_is_replaced_only_when_confirmed() {
        let _g = cg_i18n::lock_for_tests();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mine.json");
        let path = target.to_string_lossy().into_owned();
        assert_eq!(
            write("{}".into(), Some(path.clone()), false, None),
            Ok(target.clone())
        );
        assert!(write("{\"b\":1}".into(), Some(path.clone()), false, None).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{}");
        // Once the user confirmed replacing it, it is replaced.
        assert_eq!(
            write("{\"c\":1}".into(), Some(path), true, None),
            Ok(target.clone())
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "{\"c\":1}");
    }
}
