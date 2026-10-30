//! Small read-only questions to Windows: registry values and keys, the computer's name,
//! `%ProgramData%`, an account's login name and a process's file name.

use super::auth::Owned;
use std::path::PathBuf;
use windows::Win32::Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows::Win32::Security::{LookupAccountSidW, PSID, SID_NAME_USE};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{
    HKEY, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows::Win32::System::SystemInformation::{
    COMPUTER_NAME_FORMAT, ComputerNameNetBIOS, ComputerNamePhysicalDnsHostname, GetComputerNameExW,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::{HSTRING, PWSTR};

pub use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

/// A REG_SZ value; `None` when the key or value is missing or of another type.
pub fn reg_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
    let (subkey, value) = (HSTRING::from(subkey), HSTRING::from(value));
    let mut bytes = 0u32;
    // SAFETY: a size query: no buffer, the length in bytes comes back in `bytes`.
    let sized = unsafe {
        RegGetValueW(
            root,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if sized != ERROR_SUCCESS || bytes == 0 {
        return None;
    }
    let mut buffer = vec![0u16; (bytes as usize).div_ceil(2)];
    let mut bytes = (buffer.len() * 2) as u32;
    // SAFETY: `buffer` holds `bytes` bytes.
    let read = unsafe {
        RegGetValueW(
            root,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    (read == ERROR_SUCCESS).then(|| from_wide(&buffer))
}

/// An open registry key, closed when dropped.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: opened by `open`, closed exactly once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn open(root: HKEY, subkey: &str) -> Option<Key> {
    let mut key = HKEY::default();
    // SAFETY: `key` receives an open handle on success, owned by the `Key`.
    let opened = unsafe { RegOpenKeyExW(root, &HSTRING::from(subkey), None, KEY_READ, &mut key) };
    (opened == ERROR_SUCCESS).then_some(Key(key))
}

pub fn reg_key_exists(root: HKEY, subkey: &str) -> bool {
    open(root, subkey).is_some()
}

/// The names of a key's subkeys; none when it can't be opened.
pub fn reg_subkeys(root: HKEY, subkey: &str) -> Vec<String> {
    let Some(key) = open(root, subkey) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    // Key names are at most 255 characters.
    let mut buffer = [0u16; 256];
    for index in 0.. {
        let mut length = buffer.len() as u32;
        // SAFETY: `buffer` holds `length` characters.
        let found = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                Some(PWSTR(buffer.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if found != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&buffer[..length as usize]));
    }
    names
}

/// The computer's DNS host name (what `hostname` prints), not the NetBIOS name.
pub fn computer_name() -> Option<String> {
    computer_name_as(ComputerNamePhysicalDnsHostname)
}

/// The computer's NetBIOS name (`%COMPUTERNAME%`), which may differ from the host name:
/// cut to 15 characters, in capitals, or set apart.
pub fn netbios_name() -> Option<String> {
    computer_name_as(ComputerNameNetBIOS)
}

fn computer_name_as(format: COMPUTER_NAME_FORMAT) -> Option<String> {
    let mut length = 0u32;
    // SAFETY: a size query: fails with ERROR_MORE_DATA and sets `length`.
    let _ = unsafe { GetComputerNameExW(format, None, &mut length) };
    let mut buffer = vec![0u16; length.max(1) as usize];
    let mut length = buffer.len() as u32;
    // SAFETY: `buffer` holds `length` characters.
    unsafe { GetComputerNameExW(format, Some(PWSTR(buffer.as_mut_ptr())), &mut length) }.ok()?;
    Some(String::from_utf16_lossy(&buffer[..length as usize])).filter(|n| !n.is_empty())
}

/// `%ProgramData%` (normally `C:\ProgramData`), from the shell's known folders.
pub fn program_data() -> Option<PathBuf> {
    // SAFETY: on success the path is CoTaskMemAlloc'd and freed below.
    let path =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: a NUL-terminated string from the call above.
    let text = unsafe { path.to_string() };
    // SAFETY: freed exactly once.
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    text.ok().map(PathBuf::from)
}

/// The login name of the account with string SID `sid` (`S-1-5-21-…`), without its domain.
pub fn account_name(sid: &str) -> Option<String> {
    let mut psid = PSID::default();
    // SAFETY: `psid` is LocalAlloc'd on success and freed below.
    unsafe { ConvertStringSidToSidW(&HSTRING::from(sid), &mut psid) }.ok()?;
    let name = lookup(psid);
    // SAFETY: allocated by the conversion above; freed exactly once.
    let _ = unsafe { LocalFree(Some(HLOCAL(psid.0))) };
    name
}

fn lookup(sid: PSID) -> Option<String> {
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
    let mut kind = SID_NAME_USE::default();
    // SAFETY: both buffers hold the lengths passed.
    unsafe {
        LookupAccountSidW(
            None,
            sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_len,
            &mut kind,
        )
    }
    .ok()?;
    Some(String::from_utf16_lossy(&name[..name_len as usize])).filter(|n| !n.is_empty())
}

/// The file name of process `pid`'s executable (`svchost.exe`); `None` when it can't be
/// opened (the System process, or a protected one).
pub fn process_name(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed by `Owned`.
    let process =
        Owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: `buffer` holds `length` characters.
    unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .ok()?;
    let path = String::from_utf16_lossy(&buffer[..length as usize]);
    path.rsplit('\\').next().map(str::to_string)
}

/// A UTF-16 buffer up to its first NUL.
fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_windows_version() {
        let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let build = reg_string(HKEY_LOCAL_MACHINE, key, "CurrentBuild").unwrap();
        assert!(build.parse::<u32>().unwrap() >= 10240, "{build}");
        assert!(reg_key_exists(HKEY_LOCAL_MACHINE, key));
        assert!(!reg_key_exists(
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\CellGlide-no-such-key"
        ));
        assert_eq!(reg_string(HKEY_LOCAL_MACHINE, key, "NoSuchValueHere"), None);
        // A DWORD isn't a string.
        assert_eq!(
            reg_string(HKEY_LOCAL_MACHINE, key, "CurrentMajorVersionNumber"),
            None
        );
        let services = reg_subkeys(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Services");
        assert!(services.iter().any(|s| s.eq_ignore_ascii_case("Tcpip")));
    }

    #[test]
    fn names_this_computer_and_user() {
        let host = computer_name().unwrap();
        // COMPUTERNAME is the NetBIOS name: the host name's first 15 characters, upper case.
        let netbios = std::env::var("COMPUTERNAME").unwrap();
        assert!(
            host.to_uppercase().starts_with(&netbios),
            "{host} {netbios}"
        );
        assert_eq!(netbios_name().as_deref(), Some(netbios.as_str()));
        let me = account_name(&super::super::auth::testing::my_sid()).unwrap();
        assert!(
            me.eq_ignore_ascii_case(&std::env::var("USERNAME").unwrap()),
            "{me}"
        );
        assert_eq!(account_name("not a sid"), None);
    }

    #[test]
    fn finds_program_data_and_this_process() {
        let dir = program_data().unwrap();
        assert!(dir.is_absolute() && dir.is_dir(), "{}", dir.display());
        let exe = std::env::current_exe().unwrap();
        let name = process_name(std::process::id()).unwrap();
        assert!(
            name.eq_ignore_ascii_case(exe.file_name().unwrap().to_str().unwrap()),
            "{name}"
        );
        assert_eq!(process_name(0xFFFF_FFF0), None);
    }
}
