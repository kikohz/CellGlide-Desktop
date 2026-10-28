//! Everything the CLI prints to stdout goes through `outln!` / `out!`. `println!` panics
//! when the reader has gone away (`cellglide status | head -1`); these end the process
//! quietly instead. SIGPIPE stays ignored, as Rust sets it.

use std::fmt;
use std::io::{self, ErrorKind, Write};

/// Prints a line to stdout; a closed pipe exits 0.
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::out::finish($crate::out::emit(
            &mut ::std::io::stdout().lock(),
            format_args!($($arg)*),
            true,
        ))
    };
}

/// Prints to stdout without a newline; a closed pipe exits 0.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::out::finish($crate::out::emit(
            &mut ::std::io::stdout().lock(),
            format_args!($($arg)*),
            false,
        ))
    };
}

pub(crate) use {out, outln};

/// Writes `args`, a newline if asked, and flushes text left without one.
pub fn emit(w: &mut impl Write, args: fmt::Arguments, newline: bool) -> io::Result<()> {
    if newline {
        writeln!(w, "{args}")
    } else {
        write!(w, "{args}")?;
        w.flush()
    }
}

/// The exit code a failed write leads to: 0 when the reader closed the pipe, 1 otherwise.
pub fn exit_code(result: &io::Result<()>) -> Option<i32> {
    match result {
        Ok(()) => None,
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Some(0),
        Err(_) => Some(1),
    }
}

pub fn finish(result: io::Result<()>) {
    if let Some(code) = exit_code(&result) {
        if let Err(error) = &result
            && code != 0
        {
            eprintln!("stdout: {error}");
        }
        std::process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn a_closed_pipe_exits_zero() {
        for newline in [true, false] {
            let result = emit(&mut Closed, format_args!("line {}", 1), newline);
            assert_eq!(exit_code(&result), Some(0));
        }
    }

    #[test]
    fn other_write_errors_exit_one() {
        let result: io::Result<()> = Err(io::Error::from(ErrorKind::StorageFull));
        assert_eq!(exit_code(&result), Some(1));
    }

    #[test]
    fn written_text_continues() {
        let mut buf = Vec::new();
        let result = emit(&mut buf, format_args!("a{}", "b"), true);
        assert_eq!(exit_code(&result), None);
        emit(&mut buf, format_args!("c"), false).unwrap();
        assert_eq!(buf, b"ab\nc");
    }
}
