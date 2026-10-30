//! What the service clears when it starts. A crash leaves no adapter behind (W0: it goes
//! with the process's wintun handle), so this is a backstop: an adapter that still opens
//! is closed, and our GUID's DNS servers are cleared.

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
        tracing::warn!("closing a leftover {INTERFACE_NAME}");
        drop(adapter);
    }
    dns::clear_dns(WINDOWS_ADAPTER_GUID)
}
