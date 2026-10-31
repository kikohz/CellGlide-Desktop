//! The messages between the DLL and the app, as JSON. Every message type the C# side reads is
//! here (or a `cg_ipc::describe::ui` type it refers to); fields are snake_case, an `Option` is
//! `null`, and nothing is a tuple or a fixed-size array.

use crate::views::{ChromeView, HistoryView, LeakView, OverviewView, SpeedView};
use cg_ipc::DiagRange;
use cg_ipc::describe::ui::CheckRow;
use cg_tools::prefs::Prefs;
use serde::{Deserialize, Serialize};

#[cfg(any(test, feature = "testing"))]
mod samples;

#[doc(hidden)]
#[cfg(any(test, feature = "testing"))]
pub use samples::{command_samples, event_samples};

/// What the app asks for. `cg_send` answers each with a number; the result comes back as an
/// [`Event::Reply`] with that number.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Keep sending [`Event::State`] (and [`Event::ServiceGone`] while the service is away).
    Subscribe,
    /// One request to the service, in the pipe's own JSON (see [`request_json`]).
    Request {
        #[serde(with = "request_json")]
        request: cg_ipc::Request,
    },
    /// Connect or disconnect, whichever the last snapshot calls for.
    Toggle,
    SpeedStart {
        include_wifi: bool,
        /// The user accepted the data warning for this test.
        confirmed: bool,
        /// Smaller amounts than the default test (development and acceptance only).
        caps: Option<SpeedCaps>,
    },
    SpeedCancel,
    SpeedHistoryClear,
    LeakStart,
    LeakCancel,
    /// Ask the service for the bundle, for the user to review.
    DiagnosticsPrepare {
        range: DiagRange,
        email: String,
        note: String,
    },
    /// Upload the reviewed bundle.
    DiagnosticsSend {
        json: String,
    },
    /// Save the bundle to `path`, or to the Downloads folder (never over a file there). An
    /// existing file at `path` is replaced only with `overwrite`, which the app sets once the
    /// user has confirmed replacing it.
    DiagnosticsSave {
        json: String,
        path: Option<String>,
        #[serde(default)]
        overwrite: bool,
    },
    /// `zh-CN`, `en`, …; empty for the system's language.
    SetLang {
        lang: String,
    },
    GetPrefs,
    SetPrefs {
        prefs: Prefs,
    },
}

/// Limits for a small speed test.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SpeedCaps {
    pub download_bytes: u64,
    pub upload_bytes: u64,
    pub seconds: u32,
}

/// What the DLL tells the app, in order, on its own thread.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The result of command `id`.
    Reply {
        id: u64,
        result: ReplyResult,
    },
    /// Boxed only to keep `Event` small; the JSON is the view itself.
    State {
        view: Box<OverviewView>,
    },
    /// No state for now, and why; the last state no longer holds.
    ServiceGone {
        text: String,
        not_in_group: bool,
        /// The tray, its menu, the sidebar and its dots while there is no state.
        chrome: ChromeView,
    },
    PhonePlugged {
        serial: String,
    },
    SpeedProgress {
        view: SpeedView,
    },
    LeakProgress {
        view: LeakView,
    },
    /// The preferences changed.
    Prefs {
        prefs: Prefs,
    },
    /// Something inside the DLL panicked; the DLL carries on.
    Fault {
        message: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReplyResult {
    Ok {
        value: ReplyValue,
    },
    /// `message` is a sentence for the user, in the current language.
    Err {
        message: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyValue {
    Done,
    Checks {
        rows: Vec<CheckRow>,
    },
    History {
        view: HistoryView,
    },
    Diagnostics {
        json: String,
        file_name: String,
    },
    /// The report code the server gave back.
    Sent {
        code: String,
    },
    Saved {
        path: String,
    },
    Prefs {
        prefs: Prefs,
    },
}

impl ReplyResult {
    pub fn done() -> ReplyResult {
        ReplyResult::Ok {
            value: ReplyValue::Done,
        }
    }

    pub fn err(message: impl Into<String>) -> ReplyResult {
        ReplyResult::Err {
            message: message.into(),
        }
    }
}

/// `cg_ipc::Request` inside [`Command::Request`]: the pipe's JSON, except that a contact's
/// missing email or note is `null` rather than left out, as every `Option` the app sees.
///
/// `cg_ipc::Contact` itself keeps leaving them out: it is also the `contact` of the
/// diagnostics bundle, which mirrors the Mac's (Swift leaves out nil, and so does
/// `cg_diag::DiagnosticsBundle`), so changing it would change what the support server
/// receives from Linux and Windows. Reading needs nothing special: a `null` and a missing
/// field both become `None`.
mod request_json {
    use cg_ipc::{Contact, DiagRange, Request};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(request: &Request, serializer: S) -> Result<S::Ok, S::Error> {
        match request {
            Request::Diagnostics { range, contact } => Diagnostics {
                range: *range,
                contact: contact.as_ref().map(ContactFields::from),
            }
            .serialize(serializer),
            other => other.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Request, D::Error> {
        Request::deserialize(deserializer)
    }

    /// `Request::Diagnostics`, field for field.
    #[derive(Serialize)]
    #[serde(tag = "cmd", rename = "diagnostics")]
    struct Diagnostics<'a> {
        range: DiagRange,
        contact: Option<ContactFields<'a>>,
    }

    /// `Contact` with both fields always there.
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ContactFields<'a> {
        email: Option<&'a str>,
        note: Option<&'a str>,
    }

    impl<'a> From<&'a Contact> for ContactFields<'a> {
        fn from(contact: &'a Contact) -> Self {
            // Every field named, so a new one fails to compile here until it is carried over.
            let Contact { email, note } = contact;
            ContactFields {
                email: email.as_deref(),
                note: note.as_deref(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::{Contact, Request};
    use serde_json::{Value, json};

    fn diagnostics(email: Option<&str>, note: Option<&str>) -> Command {
        Command::Request {
            request: Request::Diagnostics {
                range: DiagRange::Week,
                contact: Some(Contact {
                    email: email.map(str::to_string),
                    note: note.map(str::to_string),
                }),
            },
        }
    }

    #[test]
    fn a_contact_without_a_note_says_null_and_reads_back() {
        let command = diagnostics(Some("a@b.co"), None);
        let text = serde_json::to_string(&command).unwrap();
        assert_eq!(
            text,
            r#"{"type":"request","request":{"cmd":"diagnostics","range":"week","contact":{"email":"a@b.co","note":null}}}"#
        );
        assert_eq!(serde_json::from_str::<Command>(&text).unwrap(), command);
        // The pipe to the service is unchanged: there the missing note stays left out.
        let Command::Request { request } = &command else {
            unreachable!()
        };
        assert_eq!(
            String::from_utf8(cg_ipc::to_line(request)).unwrap(),
            "{\"cmd\":\"diagnostics\",\"range\":\"week\",\"contact\":{\"email\":\"a@b.co\"}}\n"
        );
        // And what the app sends with the field left out still reads.
        let sparse = json!({"type":"request","request":{"cmd":"diagnostics","range":"week","contact":{"email":"a@b.co"}}});
        assert_eq!(serde_json::from_value::<Command>(sparse).unwrap(), command);
    }

    #[test]
    fn apart_from_nulls_the_request_is_the_pipes_json() {
        let requests = [
            diagnostics(Some("a@b.co"), Some("hi")),
            diagnostics(None, Some("hi")),
            Command::Request {
                request: Request::Diagnostics {
                    range: DiagRange::Day,
                    contact: None,
                },
            },
            Command::Request {
                request: Request::SetSettings {
                    auto_connect: None,
                    phone: None,
                    strict_dns: Some(true),
                },
            },
            Command::Request {
                request: Request::History { day: None },
            },
        ];
        for command in requests {
            let Command::Request { request } = &command else {
                unreachable!()
            };
            let mut ours = serde_json::to_value(&command).unwrap()["request"].take();
            let pipe: Value = serde_json::from_slice(&cg_ipc::to_line(request)).unwrap();
            if let Some(contact) = ours.get_mut("contact").and_then(Value::as_object_mut) {
                contact.retain(|_, v| !v.is_null());
            }
            assert_eq!(ours, pipe);
        }
    }
}
