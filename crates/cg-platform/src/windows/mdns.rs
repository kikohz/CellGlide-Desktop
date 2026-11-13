//! Finding the phone's `_cellglide-p2p._tcp` service on the access point, two ways:
//!
//! - [`Method::System`]: the DNS-SD API Windows has had since 1809 (`DnsServiceBrowse`, then
//!   `DnsServiceResolve` per instance), restricted to the access point's interface index.
//! - [`Method::Raw`]: our own mDNS queries from `interface_local_ip:5353` (SO_REUSEADDR, joined to
//!   224.0.0.251 on that interface only, multicast sent out of it), reading the PTR/SRV/A answers.
//!   It only ever asks; it never answers anyone's query.
//!
//! Either way an address outside the access point's /24 is ignored. Both calls block for the
//! whole `timeout` (run them on a blocking thread).

use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use cg_core::wireless::SERVICE_TYPE;
use socket2::{Domain, Protocol, Socket, Type};
use windows::Win32::Foundation::{DNS_REQUEST_PENDING, NO_ERROR};
use windows::Win32::NetworkManagement::Dns::{
    DNS_QUERY_REQUEST_VERSION1, DNS_RECORDW, DNS_SERVICE_BROWSE_REQUEST,
    DNS_SERVICE_BROWSE_REQUEST_0, DNS_SERVICE_CANCEL, DNS_SERVICE_INSTANCE,
    DNS_SERVICE_RESOLVE_REQUEST, DNS_TYPE_PTR, DnsFree, DnsFreeRecordList, DnsServiceBrowse,
    DnsServiceBrowseCancel, DnsServiceFreeInstance, DnsServiceResolve, DnsServiceResolveCancel,
};
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetUnicastIpAddressTable, MIB_UNICASTIPADDRESS_TABLE,
};
use windows::Win32::Networking::WinSock::AF_INET;
use windows::core::{PCWSTR, PWSTR};

pub use crate::mdns_parse::FoundPhone;
use crate::mdns_parse::{
    MDNS_GROUP, MDNS_PORT, TYPE_PTR, assemble, build_query, in_access_point_subnet, instance_of,
    merge, missing_questions, parse_response, service_name,
};

/// How to look for the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// `DnsServiceBrowse` / `DnsServiceResolve` on the access point's interface.
    System,
    /// Our own mDNS queries on `interface_local_ip:5353`.
    Raw,
}

/// The phones advertising the service on the access point whose address is
/// `interface_local_ip`: the system API first, our own queries if that fails.
pub fn browse(interface_local_ip: IpAddr, timeout: Duration) -> io::Result<Vec<FoundPhone>> {
    match browse_with(Method::System, interface_local_ip, timeout) {
        Ok(found) => Ok(found),
        Err(error) => {
            tracing::warn!("mDNS: system browse failed ({error}), using raw queries");
            browse_with(Method::Raw, interface_local_ip, timeout)
        }
    }
}

pub fn browse_with(
    method: Method,
    interface_local_ip: IpAddr,
    timeout: Duration,
) -> io::Result<Vec<FoundPhone>> {
    let IpAddr::V4(local) = interface_local_ip else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the access point's address is IPv4",
        ));
    };
    match method {
        Method::System => browse_system(local, timeout),
        Method::Raw => browse_raw(local, timeout),
    }
}

// ---- Raw ----------------------------------------------------------------------------------

/// Time between repeated queries (RFC 6762 asks for at least one second).
const REQUERY: Duration = Duration::from_millis(1500);

fn browse_raw(local: Ipv4Addr, timeout: Duration) -> io::Result<Vec<FoundPhone>> {
    let service = service_name(SERVICE_TYPE);
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // The system's own mDNS responder holds 0.0.0.0:5353; sharing the port only to receive the
    // answers sent to the group on this interface.
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::V4(SocketAddrV4::new(local, MDNS_PORT)).into())?;
    socket.join_multicast_v4(&MDNS_GROUP, &local)?;
    socket.set_multicast_if_v4(&local)?;
    socket.set_multicast_ttl_v4(255)?;
    socket.set_multicast_loop_v4(false)?;
    let socket: UdpSocket = socket.into();
    let group = SocketAddr::V4(SocketAddrV4::new(MDNS_GROUP, MDNS_PORT));

    let deadline = Instant::now() + timeout;
    let mut known = Vec::new();
    let mut next_query = Instant::now();
    let mut buffer = vec![0u8; 9000];
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if now >= next_query {
            let mut questions = vec![(service.clone(), TYPE_PTR)];
            questions.extend(missing_questions(&known, &service));
            let questions: Vec<(&str, u16)> = questions
                .iter()
                .map(|(name, kind)| (name.as_str(), *kind))
                .collect();
            if let Err(error) = socket.send_to(&build_query(&questions), group) {
                tracing::debug!("mDNS: query not sent: {error}");
            }
            next_query = now + REQUERY;
        }
        let wait = deadline.min(next_query).saturating_duration_since(now);
        socket.set_read_timeout(Some(wait.max(Duration::from_millis(10))))?;
        match socket.recv_from(&mut buffer) {
            Ok((length, from)) => {
                if !in_access_point_subnet(IpAddr::V4(local), from.ip()) {
                    continue;
                }
                merge(&mut known, parse_response(&buffer[..length]));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::ConnectionReset
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(assemble(&known, &service, IpAddr::V4(local)))
}

// ---- System -------------------------------------------------------------------------------

/// What the callbacks of one `browse_system` call report. The system calls back on its own
/// threads with a context pointer; the pointer is only a key into `CONTEXTS`, so a callback that
/// arrives after we cancelled and returned finds nothing and touches no freed memory.
#[derive(Default)]
struct Shared {
    /// Full instance names (`<instance>._cellglide-p2p._tcp.local`) from the browse.
    instances: Mutex<Vec<String>>,
    /// (full instance name, IPv4 address, port, interface index) from the resolves.
    resolved: Mutex<Vec<(String, Option<Ipv4Addr>, u16)>>,
}

/// One request's context: the call's shared results and whether this request has called back
/// (a resolve calls back once; a completed one is not cancelled).
#[derive(Clone)]
struct Context {
    shared: Arc<Shared>,
    done: Arc<AtomicBool>,
}

static CONTEXTS: LazyLock<Mutex<HashMap<usize, Context>>> = LazyLock::new(Default::default);
static NEXT_CONTEXT: AtomicUsize = AtomicUsize::new(1);

fn lookup(context: *const c_void) -> Option<Context> {
    CONTEXTS.lock().ok()?.get(&(context as usize)).cloned()
}

/// A request's context key, registered while the request may call back and removed on drop
/// (after the request was cancelled).
struct Registration {
    key: usize,
    done: Arc<AtomicBool>,
}

impl Registration {
    fn new(shared: &Arc<Shared>) -> Registration {
        let key = NEXT_CONTEXT.fetch_add(1, Ordering::Relaxed);
        let done = Arc::new(AtomicBool::new(false));
        if let Ok(mut contexts) = CONTEXTS.lock() {
            contexts.insert(
                key,
                Context {
                    shared: shared.clone(),
                    done: done.clone(),
                },
            );
        }
        Registration { key, done }
    }

    fn context(&self) -> *mut c_void {
        self.key as *mut c_void
    }

    fn done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut contexts) = CONTEXTS.lock() {
            contexts.remove(&self.key);
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// # Safety
/// `text` is null or a NUL-terminated UTF-16 string.
unsafe fn from_wide(text: *const u16) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: per the contract, a valid NUL-terminated string.
    unsafe { PCWSTR(text).to_string().ok() }
}

unsafe extern "system" fn browse_callback(
    _status: u32,
    context: *const c_void,
    records: *const DNS_RECORDW,
) {
    let shared = lookup(context).map(|context| context.shared);
    let mut record = records;
    while !record.is_null() {
        // SAFETY: the list the system handed us; valid until we free it below.
        let current = unsafe { &*record };
        if current.wType == DNS_TYPE_PTR.0
            && let Some(shared) = &shared
        {
            // SAFETY: a PTR record's data is the PTR variant, a NUL-terminated name.
            let target = unsafe { from_wide(current.Data.PTR.pNameHost.0) };
            if let (Some(target), Ok(mut instances)) = (target, shared.instances.lock())
                && !instances
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&target))
            {
                instances.push(target);
            }
        }
        record = current.pNext;
    }
    if !records.is_null() {
        // SAFETY: the browse callback owns the record list and frees it as a list.
        unsafe { DnsFree(Some(records.cast()), DnsFreeRecordList) };
    }
}

unsafe extern "system" fn resolve_callback(
    status: u32,
    context: *const c_void,
    instance: *const DNS_SERVICE_INSTANCE,
) {
    if let Some(Context { shared, done }) = lookup(context) {
        done.store(true, Ordering::SeqCst);
        if status == NO_ERROR.0 && !instance.is_null() {
            // SAFETY: a valid instance from the system, freed below.
            let found = unsafe { &*instance };
            // SAFETY: NUL-terminated string or null.
            let name = unsafe { from_wide(found.pszInstanceName.0) };
            let address = (!found.ip4Address.is_null()).then(|| {
                // SAFETY: non-null; IP4_ADDRESS holds the address in network byte order.
                Ipv4Addr::from(unsafe { *found.ip4Address }.to_ne_bytes())
            });
            if let (Some(name), Ok(mut resolved)) = (name, shared.resolved.lock()) {
                resolved.push((name, address, found.wPort));
            }
        }
    }
    if !instance.is_null() {
        // SAFETY: the resolve callback owns the instance.
        unsafe { DnsServiceFreeInstance(instance) };
    }
}

/// The interface index that has `local` as one of its IPv4 addresses.
fn interface_index(local: Ipv4Addr) -> io::Result<u32> {
    let mut table: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
    // SAFETY: on success the function stores a table we free with FreeMibTable below.
    let result = unsafe { GetUnicastIpAddressTable(AF_INET, &mut table) };
    if result != NO_ERROR {
        return Err(io::Error::from_raw_os_error(result.0 as i32));
    }
    // SAFETY: the table holds NumEntries rows and is freed only after this block.
    let index = unsafe {
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let index = rows
            .iter()
            .find(|row| {
                Ipv4Addr::from(row.Address.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()) == local
            })
            .map(|row| row.InterfaceIndex);
        FreeMibTable(table.cast());
        index
    };
    index.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no interface has the address {local}"),
        )
    })
}

/// One running `DnsServiceResolve`: its name buffer and cancel handle stay alive (and its
/// context registered) until it is cancelled or has called back.
struct Resolve {
    _name: Vec<u16>,
    cancel: Box<DNS_SERVICE_CANCEL>,
    registration: Registration,
}

fn browse_system(local: Ipv4Addr, timeout: Duration) -> io::Result<Vec<FoundPhone>> {
    let service = service_name(SERVICE_TYPE);
    let index = interface_index(local)?;
    let shared = Arc::new(Shared::default());
    let registration = Registration::new(&shared);
    let query = wide(&service);
    let request = DNS_SERVICE_BROWSE_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1.0,
        InterfaceIndex: index,
        QueryName: PCWSTR(query.as_ptr()),
        Anonymous: DNS_SERVICE_BROWSE_REQUEST_0 {
            pBrowseCallback: Some(browse_callback),
        },
        pQueryContext: registration.context(),
    };
    let mut browse_cancel = Box::new(DNS_SERVICE_CANCEL::default());
    // SAFETY: the request, its name and the cancel handle outlive the browse (cancelled below
    // before they drop); the context is a key into CONTEXTS, not a pointer.
    let status = unsafe { DnsServiceBrowse(&request, &mut *browse_cancel) };
    if status != DNS_REQUEST_PENDING {
        return Err(io::Error::other(format!(
            "DnsServiceBrowse failed: {status}"
        )));
    }

    let deadline = Instant::now() + timeout;
    let mut resolves: Vec<Resolve> = Vec::new();
    let mut started: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        let instances = shared
            .instances
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default();
        for full in instances {
            if started.contains(&full) || instance_of(&full, &service).is_none() {
                continue;
            }
            started.push(full.clone());
            let mut name = wide(&full);
            let resolve_registration = Registration::new(&shared);
            let request = DNS_SERVICE_RESOLVE_REQUEST {
                Version: DNS_QUERY_REQUEST_VERSION1.0,
                InterfaceIndex: index,
                QueryName: PWSTR(name.as_mut_ptr()),
                pResolveCompletionCallback: Some(resolve_callback),
                pQueryContext: resolve_registration.context(),
            };
            let mut cancel = Box::new(DNS_SERVICE_CANCEL::default());
            // SAFETY: the name buffer and cancel handle move into `resolves` (heap buffers, so
            // their addresses stay put) and live until the resolve called back or was cancelled;
            // the context is a key.
            let status = unsafe { DnsServiceResolve(&request, &mut *cancel) };
            if status == DNS_REQUEST_PENDING {
                resolves.push(Resolve {
                    _name: name,
                    cancel,
                    registration: resolve_registration,
                });
            } else {
                tracing::debug!("mDNS: DnsServiceResolve({full}) failed: {status}");
            }
        }
        std::thread::sleep(
            Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
        );
    }

    // SAFETY: the handle DnsServiceBrowse filled in; the browse is still running.
    unsafe { DnsServiceBrowseCancel(&*browse_cancel) };
    for resolve in &resolves {
        if !resolve.registration.done() {
            // SAFETY: the handle DnsServiceResolve filled in, for a resolve that has not called
            // back yet.
            unsafe { DnsServiceResolveCancel(&*resolve.cancel) };
        }
    }
    // Late callbacks now find no context and only free what they were given.
    drop(resolves);
    drop(registration);

    let resolved = shared
        .resolved
        .lock()
        .map(|list| list.clone())
        .unwrap_or_default();
    let mut found: Vec<FoundPhone> = Vec::new();
    for (name, address, port) in resolved {
        let Some(instance) = instance_of(&name, &service) else {
            continue;
        };
        let Some(address) = address.map(IpAddr::V4) else {
            continue;
        };
        if !in_access_point_subnet(IpAddr::V4(local), address)
            || found.iter().any(|phone| phone.instance == instance)
        {
            continue;
        }
        found.push(FoundPhone {
            instance,
            address,
            port,
        });
    }
    Ok(found)
}
