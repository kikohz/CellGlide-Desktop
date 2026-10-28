//! Our TUN device. It exists only while this descriptor is open: closing it (or the
//! process dying) makes the kernel delete the interface and its routes.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const TUNSETIFF: libc::c_ulong = 0x4004_54ca;

pub fn open(name: &str) -> io::Result<OwnedFd> {
    // ifr_name must keep its terminating NUL.
    assert!(name.len() < libc::IFNAMSIZ, "interface name too long");
    let path = CString::new("/dev/net/tun").unwrap();
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request.ifr_name.iter_mut().zip(name.bytes()) {
        *slot = byte as libc::c_char;
    }
    request.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;
    // TUNSETIFF writes the final name back into the request.
    let result = unsafe { libc::ioctl(fd.as_raw_fd(), TUNSETIFF as _, &mut request) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

pub fn index(name: &str) -> io::Result<i32> {
    let name = CString::new(name).unwrap();
    match unsafe { libc::if_nametoindex(name.as_ptr()) } {
        0 => Err(io::Error::last_os_error()),
        index => Ok(index as i32),
    }
}
