//! Who is on the other end of the control pipe, from the client process's token, and what
//! that lets them do (design §6.3): administrators, LocalSystem and users whose session is
//! active (console or Remote Desktop) get full access; anyone else who could open the pipe
//! may only look.

use crate::access::Access;
use std::io;
use std::os::windows::io::AsRawHandle;
use tokio::net::windows::named_pipe::NamedPipeServer;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_NO_IMPERSONATION_TOKEN, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    CheckTokenMembership, CreateWellKnownSid, DuplicateToken, GetTokenInformation, IsWellKnownSid,
    PSID, SECURITY_MAX_SID_SIZE, SecurityIdentification, TOKEN_DUPLICATE, TOKEN_ELEVATION_TYPE,
    TOKEN_INFORMATION_CLASS, TOKEN_LINKED_TOKEN, TOKEN_QUERY, TOKEN_USER, TokenElevationType,
    TokenElevationTypeLimited, TokenLinkedToken, TokenSessionId, TokenUser,
    WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows::Win32::System::RemoteDesktop::{
    WTS_CONNECTSTATE_CLASS, WTS_CURRENT_SERVER_HANDLE, WTSActive, WTSConnectState, WTSFreeMemory,
    WTSQuerySessionInformationW,
};
use windows::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::PWSTR;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientIdentity {
    pub is_system: bool,
    /// A member of BUILTIN\Administrators, elevated or not (UAC's filtered token counts).
    pub is_admin: bool,
    /// The client's Windows session is the active one at the console or over Remote Desktop.
    pub session_active: bool,
    /// The user's SID, `S-1-5-21-…`.
    pub sid: String,
}

pub fn access(id: &ClientIdentity) -> Access {
    if id.is_system || id.is_admin || id.session_active {
        Access::Full
    } else {
        Access::ReadOnly
    }
}

/// The identity of the process that opened `pipe`'s client end.
pub fn identify(pipe: &NamedPipeServer) -> io::Result<ClientIdentity> {
    let mut pid = 0u32;
    // SAFETY: the handle belongs to `pipe`, which outlives the call.
    unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
        .map_err(os_error)?;
    identify_process(pid)
}

/// The identity of process `pid`, from its primary token. The service runs as LocalSystem,
/// which may query any user's process.
pub(crate) fn identify_process(pid: u32) -> io::Result<ClientIdentity> {
    // SAFETY: plain calls; every handle returned is owned by an `Owned` and closed once.
    let process = Owned(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(os_error)?,
    );
    let mut token = HANDLE::default();
    // SAFETY: as above.
    unsafe { OpenProcessToken(process.0, TOKEN_QUERY | TOKEN_DUPLICATE, &mut token) }
        .map_err(os_error)?;
    let token = Owned(token);

    let user = token_info(token.0, TokenUser)?;
    // SAFETY: GetTokenInformation(TokenUser) fills the buffer with a TOKEN_USER whose SID
    // points into the same buffer, which lives until the end of this function.
    let user_sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let sid = sid_string(user_sid)?;
    // SAFETY: as above.
    let is_system = unsafe { IsWellKnownSid(user_sid, WinLocalSystemSid) }.as_bool();
    let session: u32 = token_value(token.0, TokenSessionId)?;
    let session_active = session_is_active(session);
    Ok(combine(is_system, is_admin(token.0), session_active, sid))
}

/// The identity from its parts. Only failing to read the process, token or SID makes a
/// client unidentified (denied); not being able to tell whether it is an administrator just
/// means "not one", so an unelevated admin in the active session still gets full access.
fn combine(
    is_system: bool,
    admin: io::Result<bool>,
    session_active: bool,
    sid: String,
) -> ClientIdentity {
    let is_admin = admin.unwrap_or_else(|error| {
        tracing::warn!(%error, %sid, "can't tell whether a control pipe client is an administrator");
        false
    });
    ClientIdentity {
        is_system,
        is_admin,
        session_active,
        sid,
    }
}

/// BUILTIN\Administrators enabled in the token, or in its elevated twin when UAC hands the
/// user a filtered token (where the group is only "deny").
pub(crate) fn is_admin(token: HANDLE) -> io::Result<bool> {
    if in_administrators(token)? {
        return Ok(true);
    }
    let elevation: TOKEN_ELEVATION_TYPE = token_value(token, TokenElevationType)?;
    if elevation != TokenElevationTypeLimited {
        return Ok(false);
    }
    // Without SeTcbPrivilege this is an identification-level impersonation token, checked
    // as is; with it (LocalSystem) it is a primary token, which `in_administrators`
    // duplicates. Any failure here reaches `combine` as "not an administrator".
    let linked: TOKEN_LINKED_TOKEN = token_value(token, TokenLinkedToken)?;
    let linked = Owned(linked.LinkedToken);
    in_administrators(linked.0)
}

/// CheckTokenMembership wants an impersonation token: one is used directly, a primary one
/// is duplicated (needing TOKEN_DUPLICATE on the handle) only when the call refuses it.
fn in_administrators(token: HANDLE) -> io::Result<bool> {
    let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = SECURITY_MAX_SID_SIZE;
    let admins = PSID(sid.as_mut_ptr().cast());
    // SAFETY: `sid` holds SECURITY_MAX_SID_SIZE bytes, as `size` says.
    unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(admins), &mut size) }
        .map_err(os_error)?;
    let mut member = false.into();
    // SAFETY: `token` is a valid token handle; `admins` is a valid SID.
    match unsafe { CheckTokenMembership(Some(token), admins, &mut member) } {
        Ok(()) => return Ok(member.as_bool()),
        Err(e) if e.code() == ERROR_NO_IMPERSONATION_TOKEN.to_hresult() => {}
        Err(e) => return Err(os_error(e)),
    }
    let mut duplicate = HANDLE::default();
    // SAFETY: the token was opened with TOKEN_DUPLICATE; the copy is closed by `Owned`.
    unsafe { DuplicateToken(token, SecurityIdentification, &mut duplicate) }.map_err(os_error)?;
    let duplicate = Owned(duplicate);
    // SAFETY: as above.
    unsafe { CheckTokenMembership(Some(duplicate.0), admins, &mut member) }.map_err(os_error)?;
    Ok(member.as_bool())
}

/// Whether session `id` is connected and in use; any failure (no such session) is "no".
fn session_is_active(id: u32) -> bool {
    let mut buffer = PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: on success the buffer is WTS-allocated and freed below.
    let queried = unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            id,
            WTSConnectState,
            &mut buffer,
            &mut bytes,
        )
    };
    if queried.is_err() || buffer.is_null() {
        return false;
    }
    let active = bytes as usize >= size_of::<WTS_CONNECTSTATE_CLASS>()
        // SAFETY: WTSConnectState returns one WTS_CONNECTSTATE_CLASS, as `bytes` confirms.
        && unsafe { buffer.0.cast::<WTS_CONNECTSTATE_CLASS>().read_unaligned() } == WTSActive;
    // SAFETY: allocated by WTSQuerySessionInformationW; freed exactly once.
    unsafe { WTSFreeMemory(buffer.0.cast()) };
    active
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` is valid; `text` is LocalAlloc'd on success and freed below.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(os_error)?;
    // SAFETY: a NUL-terminated string from the call above.
    let string = unsafe { text.to_string() }.map_err(io::Error::other);
    // SAFETY: freed exactly once.
    let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    string
}

/// A variable-size token information class, in a buffer aligned for its structures.
fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut needed = 0u32;
    // SAFETY: a size query: no buffer, the length comes back in `needed`.
    let _ = unsafe { GetTokenInformation(token, class, None, 0, &mut needed) };
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buffer` holds at least `needed` bytes.
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some(buffer.as_mut_ptr().cast()),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    }
    .map_err(os_error)?;
    Ok(buffer)
}

/// A fixed-size token information class: `T` must be the structure Windows writes for it.
fn token_value<T: Copy + Default>(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<T> {
    let mut value = T::default();
    let mut returned = 0u32;
    // SAFETY: `value` is a `T`, the size we pass.
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some((&raw mut value).cast()),
            size_of::<T>() as u32,
            &mut returned,
        )
    }
    .map_err(os_error)?;
    Ok(value)
}

/// A Windows error as the Win32 code `io::Error` expects (not the HRESULT).
pub(crate) fn os_error(error: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(error.code().0 & 0xFFFF)
}

/// A handle closed when dropped.
pub(crate) struct Owned(pub HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle is owned here and closed exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_rules() {
        let base = ClientIdentity {
            is_system: false,
            is_admin: false,
            session_active: false,
            sid: "S-1-5-21-1".into(),
        };
        assert_eq!(access(&base), Access::ReadOnly);
        assert_eq!(
            access(&ClientIdentity {
                session_active: true,
                ..base.clone()
            }),
            Access::Full
        );
        assert_eq!(
            access(&ClientIdentity {
                is_admin: true,
                ..base.clone()
            }),
            Access::Full
        );
        assert_eq!(
            access(&ClientIdentity {
                is_system: true,
                ..base
            }),
            Access::Full
        );
    }

    /// Failing to tell whether a client is an administrator never denies it: the active
    /// session still grants full access, otherwise it may look.
    #[test]
    fn admin_check_failure_is_not_admin() {
        let failed = || Err(io::Error::from_raw_os_error(5));
        let active = combine(false, failed(), true, "S-1-5-21-1".into());
        assert!(!active.is_admin);
        assert_eq!(access(&active), Access::Full);
        let inactive = combine(false, failed(), false, "S-1-5-21-1".into());
        assert_eq!(access(&inactive), Access::ReadOnly);
        let system = combine(true, failed(), false, "S-1-5-18".into());
        assert_eq!(access(&system), Access::Full);
        let admin = combine(false, Ok(true), false, "S-1-5-21-1".into());
        assert_eq!(access(&admin), Access::Full);
    }

    #[test]
    fn no_such_session_is_not_active() {
        assert!(!session_is_active(0xFFFF_FFF0));
    }

    /// This process as identify sees it: its own user and session, never LocalSystem here.
    #[test]
    fn identifies_this_process() {
        let me = identify_process(std::process::id()).unwrap();
        assert_eq!(me.sid, testing::my_sid());
        assert!(!me.is_system);
    }

    /// A token where Administrators is deny-only (as in UAC's filtered token, but with no
    /// linked twin) is not an administrator.
    #[test]
    fn deny_only_administrators_is_not_admin() {
        let restricted = testing::without_administrators();
        assert!(!is_admin(restricted.0).unwrap());
    }
}

/// Helpers the Windows tests share.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use windows::Win32::Security::{
        CREATE_RESTRICTED_TOKEN_FLAGS, CreateRestrictedToken, SID_AND_ATTRIBUTES,
        TOKEN_ASSIGN_PRIMARY,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    /// The test user's SID, as `whoami` reports it (independent of the code under test).
    pub fn my_sid() -> String {
        let whoami = std::path::Path::new(&std::env::var("SystemRoot").unwrap())
            .join(r"System32\whoami.exe");
        let out = std::process::Command::new(whoami)
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        // "domain\user","S-1-5-21-…"
        let sid = text.trim().rsplit(',').next().unwrap().trim_matches('"');
        assert!(sid.starts_with("S-1-"), "{text}");
        sid.to_string()
    }

    /// This process's token with BUILTIN\Administrators turned deny-only.
    pub fn without_administrators() -> Owned {
        let mut token = HANDLE::default();
        // SAFETY: the pseudo handle needs no closing; `token` is owned below.
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY,
                &mut token,
            )
        }
        .unwrap();
        let token = Owned(token);
        let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = SECURITY_MAX_SID_SIZE;
        let admins = PSID(sid.as_mut_ptr().cast());
        // SAFETY: `sid` holds `size` bytes.
        unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(admins), &mut size) }
            .unwrap();
        let disable = [SID_AND_ATTRIBUTES {
            Sid: admins,
            Attributes: 0,
        }];
        let mut restricted = HANDLE::default();
        // SAFETY: valid token and SID; the new token is owned by the result.
        unsafe {
            CreateRestrictedToken(
                token.0,
                CREATE_RESTRICTED_TOKEN_FLAGS(0),
                Some(&disable),
                None,
                None,
                &mut restricted,
            )
        }
        .unwrap();
        Owned(restricted)
    }
}
