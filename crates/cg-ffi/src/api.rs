//! The messages between the DLL and the app, as JSON. Every message type the C# side reads is
//! here (or a `cg_ipc::describe::ui` type it refers to); fields are snake_case, an `Option` is
//! `null`, and nothing is a tuple or a fixed-size array.

use crate::views::{ChromeView, HistoryView, LeakView, OverviewView, SpeedView};
use cg_ipc::DiagRange;
use cg_ipc::describe::ui::CheckRow;
use cg_tools::prefs::Prefs;
use serde::{Deserialize, Serialize};

/// What the app asks for. `cg_send` answers each with a number; the result comes back as an
/// [`Event::Reply`] with that number.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Keep sending [`Event::State`] (and [`Event::ServiceGone`] while the service is away).
    Subscribe,
    /// One request to the service.
    Request {
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
    /// Save the bundle to `path`, or to the Downloads folder.
    DiagnosticsSave {
        json: String,
        path: Option<String>,
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
