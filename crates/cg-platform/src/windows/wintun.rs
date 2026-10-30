//! A thin wrapper over the official signed `wintun.dll` (0.14.1), loaded at run time with
//! `LoadLibraryExW` and `GetProcAddress` as `wintun.h` describes. No wintun crate, so the
//! DLL is only ever searched for next to our executable and in System32.

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_NOT_FOUND, FreeLibrary, HANDLE, HMODULE,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
    LoadLibraryExW,
};
use windows::core::{GUID, PCSTR, PCWSTR};

/// Size of each session ring (send and receive): 4 MiB.
pub const SESSION_CAPACITY: u32 = 0x40_0000;

/// `WINTUN_MAX_IP_PACKET_SIZE`.
const MAX_IP_PACKET_SIZE: usize = 0xFFFF;

/// Debug builds look here first, so tests and examples can run from `target\`.
#[cfg(debug_assertions)]
const DLL_ENV: &str = "CELLGLIDE_WINTUN_DLL";

type AdapterHandle = *mut c_void;
type SessionHandle = *mut c_void;

// Prototypes from wintun.h. NET_LUID is a 64-bit union, written through as a u64.
type CreateAdapterFn =
    unsafe extern "system" fn(*const u16, *const u16, *const GUID) -> AdapterHandle;
type OpenAdapterFn = unsafe extern "system" fn(*const u16) -> AdapterHandle;
type CloseAdapterFn = unsafe extern "system" fn(AdapterHandle);
type GetAdapterLuidFn = unsafe extern "system" fn(AdapterHandle, *mut u64);
type StartSessionFn = unsafe extern "system" fn(AdapterHandle, u32) -> SessionHandle;
type EndSessionFn = unsafe extern "system" fn(SessionHandle);
type GetReadWaitEventFn = unsafe extern "system" fn(SessionHandle) -> HANDLE;
type ReceivePacketFn = unsafe extern "system" fn(SessionHandle, *mut u32) -> *mut u8;
type ReleaseReceivePacketFn = unsafe extern "system" fn(SessionHandle, *const u8);
type AllocateSendPacketFn = unsafe extern "system" fn(SessionHandle, u32) -> *mut u8;
type SendPacketFn = unsafe extern "system" fn(SessionHandle, *const u8);

/// Owns the module handle; frees it last, after every adapter and session is gone
/// (they hold an `Arc<Wintun>`).
struct Library(HMODULE);

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the handle came from LoadLibraryExW and nothing uses the DLL any more.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

/// The loaded `wintun.dll` and its entry points.
pub struct Wintun {
    create_adapter: CreateAdapterFn,
    open_adapter: OpenAdapterFn,
    close_adapter: CloseAdapterFn,
    get_adapter_luid: GetAdapterLuidFn,
    start_session: StartSessionFn,
    end_session: EndSessionFn,
    get_read_wait_event: GetReadWaitEventFn,
    receive_packet: ReceivePacketFn,
    release_receive_packet: ReleaseReceivePacketFn,
    allocate_send_packet: AllocateSendPacketFn,
    send_packet: SendPacketFn,
    _library: Library,
}

// SAFETY: the module handle is process-wide and wintun's functions may be called from any
// thread; the struct holds nothing else.
unsafe impl Send for Wintun {}
// SAFETY: as above; nothing in Wintun is mutated after loading.
unsafe impl Sync for Wintun {}

impl std::fmt::Debug for Wintun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wintun").finish_non_exhaustive()
    }
}

impl Wintun {
    /// Loads `wintun.dll` from our executable's directory (or System32). Debug builds use
    /// the absolute path in `CELLGLIDE_WINTUN_DLL` instead when it is set.
    pub fn load() -> io::Result<Arc<Wintun>> {
        #[cfg(debug_assertions)]
        if let Some(path) = std::env::var_os(DLL_ENV) {
            return Self::load_from(Path::new(&path));
        }
        Self::load_module(OsStr::new("wintun.dll")).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "can't load wintun.dll ({e}); it should be in the same directory as cellglide-service.exe"
                ),
            )
        })
    }

    /// Loads `wintun.dll` from an absolute path.
    pub fn load_from(path: &Path) -> io::Result<Arc<Wintun>> {
        let explain = |e: io::Error| {
            io::Error::new(
                e.kind(),
                format!(
                    "can't load wintun.dll from {} ({e}); it should be in the same directory as cellglide-service.exe",
                    path.display()
                ),
            )
        };
        if !path.is_absolute() {
            return Err(explain(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not an absolute path",
            )));
        }
        Self::load_module(path.as_os_str()).map_err(explain)
    }

    fn load_module(file: &OsStr) -> io::Result<Arc<Wintun>> {
        let wide = wide(file)?;
        // SAFETY: `wide` is NUL-terminated and outlives the call. The search flags keep
        // the loader to our executable's directory and System32.
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR(wide.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map_err(win_error)?;
        let library = Library(module);

        macro_rules! sym {
            ($ty:ty, $name:literal) => {{
                // SAFETY: the name is NUL-terminated; the module is loaded.
                let f = unsafe { GetProcAddress(library.0, PCSTR(concat!($name, "\0").as_ptr())) }
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            concat!("wintun.dll has no ", $name),
                        )
                    })?;
                // SAFETY: the target type is the prototype wintun.h declares for this name.
                unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(f) }
            }};
        }

        Ok(Arc::new(Wintun {
            create_adapter: sym!(CreateAdapterFn, "WintunCreateAdapter"),
            open_adapter: sym!(OpenAdapterFn, "WintunOpenAdapter"),
            close_adapter: sym!(CloseAdapterFn, "WintunCloseAdapter"),
            get_adapter_luid: sym!(GetAdapterLuidFn, "WintunGetAdapterLUID"),
            start_session: sym!(StartSessionFn, "WintunStartSession"),
            end_session: sym!(EndSessionFn, "WintunEndSession"),
            get_read_wait_event: sym!(GetReadWaitEventFn, "WintunGetReadWaitEvent"),
            receive_packet: sym!(ReceivePacketFn, "WintunReceivePacket"),
            release_receive_packet: sym!(ReleaseReceivePacketFn, "WintunReleaseReceivePacket"),
            allocate_send_packet: sym!(AllocateSendPacketFn, "WintunAllocateSendPacket"),
            send_packet: sym!(SendPacketFn, "WintunSendPacket"),
            _library: library,
        }))
    }
}

/// Closes the adapter when the last owner (the `Adapter` or a `Session`) goes away, so a
/// session always ends before its adapter closes.
struct AdapterHandleOwner {
    dll: Arc<Wintun>,
    handle: AdapterHandle,
}

// SAFETY: wintun adapter handles may be used and closed from any thread.
unsafe impl Send for AdapterHandleOwner {}
// SAFETY: the handle is only passed to wintun functions, which are thread-safe.
unsafe impl Sync for AdapterHandleOwner {}

impl Drop for AdapterHandleOwner {
    fn drop(&mut self) {
        // SAFETY: the handle is valid and no session on it remains (sessions hold an Arc).
        // An adapter this process created is removed by this call.
        unsafe { (self.dll.close_adapter)(self.handle) };
    }
}

/// A wintun adapter. Dropping it calls `WintunCloseAdapter` (once any session has ended),
/// which removes the adapter if this process created it.
pub struct Adapter {
    inner: Arc<AdapterHandleOwner>,
}

impl std::fmt::Debug for Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Adapter")
            .field("luid", &self.luid())
            .finish()
    }
}

impl Adapter {
    /// `WintunCreateAdapter(name, tunnel_type, guid)`.
    pub fn create(
        dll: &Arc<Wintun>,
        name: &str,
        tunnel_type: &str,
        guid: u128,
    ) -> io::Result<Adapter> {
        let name = wide(OsStr::new(name))?;
        let tunnel_type = wide(OsStr::new(tunnel_type))?;
        let guid = GUID::from_u128(guid);
        // SAFETY: both strings are NUL-terminated and the GUID lives across the call.
        let handle = unsafe { (dll.create_adapter)(name.as_ptr(), tunnel_type.as_ptr(), &guid) };
        if handle.is_null() {
            return Err(last_error());
        }
        Ok(Adapter::wrap(dll, handle))
    }

    /// `WintunOpenAdapter(name)`: `None` when no adapter by that name exists.
    pub fn open(dll: &Arc<Wintun>, name: &str) -> io::Result<Option<Adapter>> {
        let name = wide(OsStr::new(name))?;
        // SAFETY: the string is NUL-terminated.
        let handle = unsafe { (dll.open_adapter)(name.as_ptr()) };
        if handle.is_null() {
            let error = last_error();
            return match error.raw_os_error() {
                Some(code)
                    if code == ERROR_NOT_FOUND.0 as i32
                        || code == ERROR_FILE_NOT_FOUND.0 as i32 =>
                {
                    Ok(None)
                }
                _ => Err(error),
            };
        }
        Ok(Some(Adapter::wrap(dll, handle)))
    }

    fn wrap(dll: &Arc<Wintun>, handle: AdapterHandle) -> Adapter {
        Adapter {
            inner: Arc::new(AdapterHandleOwner {
                dll: Arc::clone(dll),
                handle,
            }),
        }
    }

    /// The adapter's `NET_LUID`, as the IP Helper functions take it.
    pub fn luid(&self) -> u64 {
        let mut luid = 0u64;
        // SAFETY: the handle is valid; NET_LUID is a 64-bit union.
        unsafe { (self.inner.dll.get_adapter_luid)(self.inner.handle, &mut luid) };
        luid
    }

    /// `WintunStartSession` with rings of `capacity` bytes (a power of two between
    /// 128 KiB and 64 MiB).
    pub fn start_session(&self, capacity: u32) -> io::Result<Session> {
        // SAFETY: the handle is valid.
        let handle = unsafe { (self.inner.dll.start_session)(self.inner.handle, capacity) };
        if handle.is_null() {
            return Err(last_error());
        }
        Ok(Session {
            adapter: Arc::clone(&self.inner),
            handle,
        })
    }
}

/// A running session. Receiving and sending may happen on two threads at once; each of
/// `try_receive` and `send` is thread-safe in wintun, but `try_receive` is meant for one
/// receiving thread (which waits on `read_event` when it returns `None`), so packets come
/// out in order. Dropping it calls `WintunEndSession`.
pub struct Session {
    adapter: Arc<AdapterHandleOwner>,
    handle: SessionHandle,
}

// SAFETY: wintun's receive, release, allocate and send functions are thread-safe
// (wintun.h), and the session handle stays valid until Drop.
unsafe impl Send for Session {}
// SAFETY: as above.
unsafe impl Sync for Session {}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    /// Signalled when packets are waiting. Owned by the session; do not close it.
    pub fn read_event(&self) -> HANDLE {
        // SAFETY: the session handle is valid.
        unsafe { (self.adapter.dll.get_read_wait_event)(self.handle) }
    }

    /// The next received packet, or `None` when the ring is empty (`ERROR_NO_MORE_ITEMS`).
    /// Any other failure, such as `ERROR_HANDLE_EOF` when the adapter is going away, is an
    /// error.
    pub fn try_receive(&self) -> io::Result<Option<Vec<u8>>> {
        let dll = &self.adapter.dll;
        let mut size = 0u32;
        // SAFETY: the session handle is valid and `size` is writable.
        let packet = unsafe { (dll.receive_packet)(self.handle, &mut size) };
        if packet.is_null() {
            let error = last_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_ITEMS.0 as i32) {
                return Ok(None);
            }
            return Err(error);
        }
        // SAFETY: wintun returned `size` readable bytes at `packet`, valid until released.
        let data = unsafe { std::slice::from_raw_parts(packet, size as usize) }.to_vec();
        // SAFETY: `packet` came from WintunReceivePacket on this session and is released once.
        unsafe { (dll.release_receive_packet)(self.handle, packet) };
        Ok(Some(data))
    }

    /// Queues one IP packet for the system to receive. `ERROR_BUFFER_OVERFLOW` means the
    /// ring is full; the packet is dropped.
    pub fn send(&self, packet: &[u8]) -> io::Result<()> {
        if packet.is_empty() || packet.len() > MAX_IP_PACKET_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("packet of {} bytes", packet.len()),
            ));
        }
        let dll = &self.adapter.dll;
        // SAFETY: the session handle is valid; the size is within WINTUN_MAX_IP_PACKET_SIZE.
        let buffer = unsafe { (dll.allocate_send_packet)(self.handle, packet.len() as u32) };
        if buffer.is_null() {
            return Err(last_error());
        }
        // SAFETY: wintun handed us `packet.len()` writable bytes at `buffer`, which cannot
        // overlap `packet`; it is sent exactly once.
        unsafe {
            std::ptr::copy_nonoverlapping(packet.as_ptr(), buffer, packet.len());
            (dll.send_packet)(self.handle, buffer);
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the handle came from WintunStartSession and is ended once; the adapter
        // is still open because we hold its owner.
        unsafe { (self.adapter.dll.end_session)(self.handle) };
    }
}

/// A GUID in registry form, e.g. `{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}`, built from
/// the same `GUID` value that is handed to wintun.
pub fn guid_string(guid: u128) -> String {
    let g = GUID::from_u128(guid);
    let d = g.data4;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

fn wide(s: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = s.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "string contains NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

/// An OS error code as an io::Error. Some wintun paths leave an HRESULT in the last error
/// (`WintunCreateAdapter` on an existing name leaves 0x800700B7); those are unwrapped to
/// the Win32 code so `kind()` and `raw_os_error()` mean what they say.
fn os_error(code: i32) -> io::Error {
    let bits = code as u32;
    if bits & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((bits & 0xFFFF) as i32)
    } else {
        io::Error::from_raw_os_error(code)
    }
}

/// `GetLastError` after a failed wintun call.
fn last_error() -> io::Error {
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) => os_error(code),
        None => error,
    }
}

/// A windows-rs error as an io::Error.
fn win_error(error: windows::core::Error) -> io::Error {
    os_error(error.code().0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::tunnel::WINDOWS_ADAPTER_GUID;
    use std::path::Path;

    #[test]
    fn missing_dll_is_explained() {
        let err = Wintun::load_from(Path::new(r"C:\nonexistent\wintun.dll")).unwrap_err();
        assert!(err.to_string().contains("wintun.dll"), "{err}");
        assert!(
            err.to_string()
                .contains("same directory as cellglide-service.exe"),
            "{err}"
        );
    }

    #[test]
    fn guid_bytes_match_the_registry_form() {
        assert_eq!(
            guid_string(WINDOWS_ADAPTER_GUID),
            "{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}"
        );
    }

    #[test]
    fn hresult_codes_become_win32_codes() {
        // WintunCreateAdapter on a name that exists leaves HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS).
        let error = os_error(0x800700B7_u32 as i32);
        assert_eq!(error.raw_os_error(), Some(183));
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(os_error(259).raw_os_error(), Some(259));
    }
}
