//! Stands in for the diagnostics bundle until the Windows service builds one (Task 13), with
//! the Linux module's shape so the server is the same on both.

use cg_ipc::{Check, Contact, DiagRange, Reason, Snapshot};
use jiff::Timestamp;

/// Where the bundle's facts will come from.
pub struct Sources;

pub async fn build(
    _src: &Sources,
    _range: DiagRange,
    _contact: Option<Contact>,
    _snapshot: &Snapshot,
    _checks: &[Check],
    _requester: Option<u32>,
    _now: Timestamp,
) -> Result<String, Reason> {
    Err(Reason::Internal {
        detail: "diagnostics".into(),
    })
}
