//! What the service clears when it starts. A crash leaves no adapter behind (W0: it goes
//! with the process's wintun handle), and the strict DNS rules go with their dynamic WFP
//! session, so this is a backstop for our GUID's DNS servers.
//!
//! An adapter that still opens belongs to a live process (another CellGlide, or a dev
//! tool): `WintunCloseAdapter` only removes adapters this process created, so we can't
//! close it, and clearing the DNS would pull it out from under a running tunnel. Both are
//! left alone then.

use std::io;
use std::sync::Arc;

use cg_core::tunnel::{INTERFACE_NAME, WINDOWS_ADAPTER_GUID};

use super::dns;
use super::wintun::{Adapter, Wintun};

/// Loads `wintun.dll` and clears leftovers; called once when the service starts.
pub fn cleanup_leftovers() -> io::Result<()> {
    cleanup_leftovers_with(&Wintun::load()?)
}

/// `cleanup_leftovers` with an already loaded `wintun.dll`.
pub fn cleanup_leftovers_with(dll: &Arc<Wintun>) -> io::Result<()> {
    if let Some(adapter) = Adapter::open(dll, INTERFACE_NAME)? {
        // Dropping an opened (not created) adapter only releases our handle.
        drop(adapter);
        tracing::warn!(
            "{INTERFACE_NAME} is in use by another process; leaving it and its DNS servers alone"
        );
        return Ok(());
    }
    dns::clear_dns(WINDOWS_ADAPTER_GUID)
}
