//! Who serves a named pipe: the client trusts `\\.\pipe\cellglide-control` only when the pipe
//! is owned by LocalSystem and served from session 0, so a program that grabs the name while
//! the service is stopped can't pose as the service. Both facts are readable by any client,
//! elevated or not (opening the server process is not: LocalSystem processes refuse
//! non-elevated callers, measured in Task 7).

use std::io;
use std::os::windows::io::AsRawHandle;
use tokio::net::windows::named_pipe::NamedPipeClient;
use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows::Win32::Security::{
    IsWellKnownSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, WinLocalSystemSid,
};
use windows::Win32::System::Pipes::GetNamedPipeServerSessionId;

/// The decision, apart from the system calls: the service's pipe is owned by LocalSystem
/// (`O:SY` in [`crate::PIPE_SDDL`], which no other account can set without
/// SeRestorePrivilege) and is served from session 0, where only services run.
pub(crate) fn verdict(owner_is_system: bool, session_id: u32) -> bool {
    owner_is_system && session_id == 0
}

/// Whether the pipe's server is the CellGlide service: see [`verdict`].
pub fn server_is_cellglide_service(pipe: &NamedPipeClient) -> io::Result<bool> {
    let handle = HANDLE(pipe.as_raw_handle());
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the handle belongs to `pipe`, which outlives the call; `owner` points into
    // `descriptor`, which we free below.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    };
    if status.is_err() {
        return Err(io::Error::from_raw_os_error(status.0 as i32));
    }
    // SAFETY: `owner` is valid while `descriptor` lives.
    let owner_is_system = unsafe { IsWellKnownSid(owner, WinLocalSystemSid) }.as_bool();
    // SAFETY: GetSecurityInfo allocated the descriptor with LocalAlloc; freed exactly once.
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    let mut session = u32::MAX;
    // SAFETY: the handle belongs to `pipe`.
    unsafe { GetNamedPipeServerSessionId(handle, &mut session) }
        .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
    Ok(verdict(owner_is_system, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_system_owned_pipe_in_session_zero_is_the_service() {
        assert!(verdict(true, 0));
        assert!(!verdict(true, 1));
        assert!(!verdict(false, 0));
        assert!(!verdict(false, 1));
    }
}
