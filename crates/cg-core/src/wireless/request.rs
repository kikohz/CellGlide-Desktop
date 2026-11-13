//! First line of every wireless connection: `CG1 DATA <port>\n` or `CG1 PAIR\n`.

use super::ALLOWED_PORTS;

/// Longest request line accepted, in bytes, including the trailing newline.
pub const MAX_REQUEST_LENGTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Data(u16),
    Pair,
}

/// No valid request comes near the limit (the longest is 13 bytes); the check only bounds the
/// work done on hostile input.
fn too_long(line: &str) -> bool {
    line.len() >= MAX_REQUEST_LENGTH
}

impl Request {
    /// What the sender writes, including the trailing newline.
    pub fn line(&self) -> String {
        match self {
            Request::Data(port) => format!("CG1 DATA {port}\n"),
            Request::Pair => "CG1 PAIR\n".to_string(),
        }
    }

    /// `line` is the request without its newline (the reader strips it).
    pub fn parse(line: &str) -> Option<Request> {
        if too_long(line) {
            return None;
        }
        if line == "CG1 PAIR" {
            return Some(Request::Pair);
        }
        let digits = line.strip_prefix("CG1 DATA ")?;
        // Canonical decimal only: `u16::from_str` would also accept a leading "+".
        if digits.is_empty()
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
        {
            return None;
        }
        let port: u16 = digits.parse().ok()?;
        ALLOWED_PORTS.contains(&port).then_some(Request::Data(port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wireless::CABLE_PAIRING_PORT;

    #[test]
    fn parses_data_and_pair() {
        assert_eq!(Request::parse("CG1 PAIR"), Some(Request::Pair));
        for port in ALLOWED_PORTS {
            assert_eq!(
                Request::parse(&format!("CG1 DATA {port}")),
                Some(Request::Data(port))
            );
        }
        assert_eq!(Request::Pair.line(), "CG1 PAIR\n");
        assert_eq!(Request::Data(9876).line(), "CG1 DATA 9876\n");
        let request = Request::Data(8089);
        let line = request.line();
        assert_eq!(Request::parse(line.trim_end_matches('\n')), Some(request));
    }

    #[test]
    fn rejects_port_outside_allowlist() {
        for line in [
            "CG1 DATA 22".to_string(),
            format!("CG1 DATA {CABLE_PAIRING_PORT}"),
            "CG1 DATA 0".into(),
            "CG1 DATA 65535".into(),
            "CG1 DATA 70000".into(),
        ] {
            assert_eq!(Request::parse(&line), None, "{line:?}");
        }
    }

    #[test]
    fn rejects_malformed() {
        let long_port = format!("CG1 DATA 9876{}", "0".repeat(40));
        let long_junk = "A".repeat(33);
        let bad = [
            "",
            "CG1",
            "CG1 DATA",
            "CG1 DATA ",
            "CG1  DATA 9876",
            "CG1 DATA  9876",
            " CG1 PAIR",
            "CG1 PAIR ",
            "CG1 DATA 9876 ",
            "CG1 PAIR\n",
            "CG1 DATA 9876\n",
            "CG1 DATA 9876\r",
            "CG1 DATA\t9876",
            "CG1 DATA abc",
            "CG1 DATA -1",
            "CG1 DATA +9876",
            "CG1 DATA 09876",
            "CG1 DATA 98.76",
            "cg1 pair",
            "CG2 PAIR",
            "CG1 PAIR 9876",
            "CG1 DATA 9876 9876",
            &long_port,
            &long_junk,
        ];
        for line in bad {
            assert_eq!(Request::parse(line), None, "{line:?}");
        }
        assert!(Request::Data(9876).line().len() <= MAX_REQUEST_LENGTH);
    }

    #[test]
    fn the_length_limit_is_exclusive_of_the_newline_budget() {
        // `parse` gets the line without its newline, so 31 bytes plus it is the longest allowed.
        assert!(!too_long(&"A".repeat(MAX_REQUEST_LENGTH - 1)));
        assert!(too_long(&"A".repeat(MAX_REQUEST_LENGTH)));
    }
}
