//! A local stand-in for the diagnostics endpoint, for checking the app's Send without reaching
//! production: `cargo run -p cg-diag --example fake_diag_server -- <port>`. It listens on
//! 127.0.0.1 only; every POST is read to its end, its body length printed, and answered with
//! 200 and `{"code":"CG-LOCAL1"}`. Point a debug build of the app at it with
//! `CELLGLIDE_DIAG_TARGET=http://127.0.0.1:<port>/v1/reports`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};

const REPLY: &str = r#"{"code":"CG-LOCAL1"}"#;

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or_else(|| {
            eprintln!("usage: fake_diag_server <port>");
            std::process::exit(2);
        });
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("bind 127.0.0.1");
    println!("listening on 127.0.0.1:{port}");
    for stream in listener.incoming() {
        match stream.map(serve) {
            Ok(Ok(())) => {}
            Ok(Err(e)) | Err(e) => eprintln!("connection failed: {e}"),
        }
    }
}

fn serve(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let method = request_line.split_whitespace().next().unwrap_or("");
    let (status, reply) = if method == "POST" {
        println!("{} {} bytes", request_line.trim_end(), body.len());
        ("200 OK", REPLY)
    } else {
        println!("{} (not a POST)", request_line.trim_end());
        ("405 Method Not Allowed", "")
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    )?;
    stream.flush()
}
