//! Who serves a named pipe: the client trusts `\\.\pipe\cellglide-control` only when its server
//! process runs as LocalSystem, so a program that grabs the name while the service is stopped
//! can't pose as the service.

use std::io;
use std::os::windows::io::AsRawHandle;
use tokio::net::windows::named_pipe::NamedPipeClient;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, TOKEN_QUERY, TOKEN_USER, TokenUser, WinLocalSystemSid,
};
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Closes the handle when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us and is closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn os_error(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

/// Whether the pipe's server process runs as LocalSystem: `GetNamedPipeServerProcessId`, then
/// that process's token user compared with the well-known LocalSystem SID.
pub fn server_is_local_system(pipe: &NamedPipeClient) -> io::Result<bool> {
    let mut pid = 0u32;
    // SAFETY: the handle belongs to `pipe`, which outlives the call.
    unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
        .map_err(os_error)?;
    // SAFETY: plain call; the returned handle is owned and closed by `Owned`.
    let process = Owned(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(os_error)?,
    );
    let mut token = HANDLE::default();
    // SAFETY: `process` is a valid process handle; `token` receives a handle we then own.
    unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) }.map_err(os_error)?;
    let token = Owned(token);
    let mut len = 0u32;
    // SAFETY: a null buffer asks only for the size, written to `len`; the call fails by design.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 cells keep the TOKEN_USER (pointer-sized fields) aligned.
    let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `len` bytes.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            len,
            &mut len,
        )
    }
    .map_err(os_error)?;
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points into the buffer.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    // SAFETY: the SID is valid while `buffer` lives.
    Ok(unsafe { IsWellKnownSid(user.User.Sid, WinLocalSystemSid) }.as_bool())
}
