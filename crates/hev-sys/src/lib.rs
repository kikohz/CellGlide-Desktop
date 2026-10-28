//! FFI to hev-socks5-tunnel. One tunnel per process: `quit` stops the running one.

#[cfg(target_os = "linux")]
unsafe extern "C" {
    /// Runs the tunnel on `tun_fd` with a YAML config; blocks until `quit` or an error.
    pub fn hev_socks5_tunnel_main_from_str(config: *const u8, config_len: u32, tun_fd: i32) -> i32;
    pub fn hev_socks5_tunnel_quit();
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn rejects_a_broken_config_without_blocking() {
        let config = b"tunnel: [";
        let result = unsafe {
            super::hev_socks5_tunnel_main_from_str(config.as_ptr(), config.len() as u32, -1)
        };
        assert_ne!(result, 0);
    }
}
