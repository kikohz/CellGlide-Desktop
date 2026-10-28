//! What a page asks the app to do. Pages never do I/O themselves: `update` returns an `Effect`,
//! tests assert on it, and `App::run_effect` is the one place that turns it into work.

use crate::pages::speed::SpeedJob;
use crate::prefs::{PrefChange, Prefs};
use cg_tools::speed::Record;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    None,
    Batch(Vec<Effect>),
    /// Ask the service; the reply goes where `Reply` says.
    Request(cg_ipc::Request, Reply),
    Copy(String),
    /// Opened with `xdg-open`; anything but an `https://` address is ignored.
    OpenUrl(String),
    OpenFolder(PathBuf),
    /// Close the panel and exit the GUI (the tray's Quit).
    Quit,
    /// Applied to the preferences, which are then saved.
    Prefs(PrefChange),
    /// Write these preferences to disk, off the UI thread. `Prefs` queues one after applying
    /// its change; a failure is logged.
    SavePrefs(Prefs),
    /// Change the speed test history file, off the UI thread; the page gets
    /// `speed::Msg::HistorySaved` with the records the file holds afterwards.
    SpeedHistory(HistoryOp),
    SetAutostart(bool),
    /// Run this speed test in the background; its progress comes back as page messages.
    StartSpeed(SpeedJob),
    /// Run the DNS leak test in the background; `sharing` is the connection's DNS mode, `None`
    /// when this computer isn't online through the phone. The result comes back as `leak::Msg::Done`.
    StartLeak {
        sharing: Option<cg_ipc::DnsMode>,
    },
    /// Stop the running speed test.
    CancelSpeed,
    /// Send this diagnostics bundle (the text the user previewed) to CellGlide; the report code
    /// comes back as `diagnostics::Msg::Sent`. Only the diagnostics page's Send makes it.
    Upload(String),
    /// Write this diagnostics bundle into the download directory; `diagnostics::Msg::Saved`.
    SaveDiagnostics(String),
    /// `Message::After` once the time has passed (e.g. "Copied" turning back into "Copy").
    After(Duration, AfterMsg),
}

/// A change to the speed test history file.
#[derive(Clone, Debug, PartialEq)]
pub enum HistoryOp {
    Add(Record),
    Clear,
}

/// The page a service reply belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    History,
    Diagnostics,
    /// `doctor` for a page; the number is the ask's token (`checks::Run`).
    Doctor(DoctorFor, u64),
    Settings(SettingField),
}

/// Which control a settings change came from, so a failure shows beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingField {
    AutoConnect,
    Phone,
}

/// Which page ran `doctor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoctorFor {
    Settings,
    Leak,
}

// Every timer so far is a "Copied" turning back into "Copy".
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterMsg {
    SettingsCopied,
    DiagnosticsCopied,
    AboutCopied,
}
