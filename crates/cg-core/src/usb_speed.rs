//! The negotiated USB speed of the iPhone, from sysfs, for the session watch header
//! (the Mac reads the same from IOKit). sysfs lists the iPhone's UDID without the dash
//! as its serial; `speed` is in Mbit/s.

use std::fs;
use std::path::Path;

pub fn link_speed(root: &Path, udid: &str) -> Option<u64> {
    let wanted = udid.replace('-', "").to_ascii_uppercase();
    for entry in fs::read_dir(root.join("sys/bus/usb/devices"))
        .ok()?
        .flatten()
    {
        let dir = entry.path();
        let Ok(serial) = fs::read_to_string(dir.join("serial")) else {
            continue;
        };
        if serial.trim().replace('-', "").to_ascii_uppercase() != wanted {
            continue;
        }
        let mbits: f64 = fs::read_to_string(dir.join("speed"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        // Unknown speeds read as -1 (or nonsense): leave the header out then.
        if !mbits.is_finite() || mbits <= 0.0 {
            return None;
        }
        return Some((mbits * 1_000_000.0).round() as u64);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(root: &Path, name: &str, serial: &str, speed: &str) {
        let dir = root.join("sys/bus/usb/devices").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("serial"), format!("{serial}\n")).unwrap();
        std::fs::write(dir.join("speed"), format!("{speed}\n")).unwrap();
    }

    #[test]
    fn finds_the_phone_by_udid() {
        let root = tempfile::tempdir().unwrap();
        device(root.path(), "1-1", "OTHERDEVICE", "12");
        device(root.path(), "1-2", "00008140000000000000001C", "480");
        assert_eq!(
            link_speed(root.path(), "00008140-000000000000001C"),
            Some(480_000_000)
        );
    }

    #[test]
    fn reads_fractional_and_superspeed() {
        let root = tempfile::tempdir().unwrap();
        device(root.path(), "2-1", "AAAA", "5000");
        device(root.path(), "3-1", "BBBB", "1.5");
        assert_eq!(link_speed(root.path(), "aaaa"), Some(5_000_000_000));
        assert_eq!(link_speed(root.path(), "BBBB"), Some(1_500_000));
        assert_eq!(link_speed(root.path(), "CCCC"), None);
        assert_eq!(link_speed(Path::new("/nonexistent"), "AAAA"), None);
    }

    #[test]
    fn unknown_speed_is_left_out() {
        let root = tempfile::tempdir().unwrap();
        device(root.path(), "1-1", "AAAA", "-1");
        device(root.path(), "1-2", "BBBB", "0");
        device(root.path(), "1-3", "CCCC", "NaN");
        device(root.path(), "1-4", "DDDD", "inf");
        assert_eq!(link_speed(root.path(), "AAAA"), None);
        assert_eq!(link_speed(root.path(), "BBBB"), None);
        assert_eq!(link_speed(root.path(), "CCCC"), None);
        assert_eq!(link_speed(root.path(), "DDDD"), None);
    }
}
