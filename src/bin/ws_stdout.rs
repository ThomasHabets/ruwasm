//! `ws_stdout`: Run a websocket server that when connected on, streams the stdout
//! from a command to the websocket client.
//!
//! This is just a proof of concept while the actual protocol gets designed in
//! `DATA_STREAM.md`.
//!
//! This code is mostly vibe coded, implementing websocket stuff without
//! dependencies. Kudos to LLM, but not something to rely on.
#![allow(clippy::many_single_char_names)]
#![allow(clippy::unreadable_literal)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::single_match_else)]
#![allow(clippy::format_collect)]
#![allow(clippy::cast_possible_truncation)]
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;

const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const WEBSOCKET_MAGIC: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Websocket server for stdout output streaming.
#[derive(Parser)]
#[clap(about, version)]
struct Opt {
    /// Listen address.
    #[arg(long, short, default_value = "localhost:8080")]
    listen: String,

    /// Verbosity level.
    #[clap(short = 'v', default_value = "info")]
    verbose: LogLevel,

    /// Command to run and read stdout from.
    command: Vec<String>,
}

struct HttpRequest {
    method: String,
    headers: Vec<(String, String)>,
}

struct ClientFrame {
    opcode: u8,
    payload: Vec<u8>,
}

fn main() -> Result<()> {
    let opt = Arc::new(Opt::parse());

    stderrlog::new()
        .module(module_path!())
        .module("rustradio")
        .quiet(false)
        .verbosity(opt.verbose as usize)
        .timestamp(stderrlog::Timestamp::Second)
        .init()?;

    let listener =
        TcpListener::bind(&opt.listen).context(format!("failed to bind to {}", opt.listen))?;

    if opt.command.is_empty() {
        return Err(anyhow::anyhow!("missing command"));
    }

    eprintln!(
        "Listening on {}; streaming command: {}",
        opt.listen,
        shellish_command(&opt.command)
    );

    for conn in listener.incoming() {
        let opt = opt.clone();
        match conn {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, opt) {
                        eprintln!("connection failed: {err}");
                    }
                });
            }
            Err(err) => eprintln!("accept failed: {err}"),
        }
    }
    Ok(())
}

fn handle_client(mut stream: TcpStream, opt: Arc<Opt>) -> io::Result<()> {
    let request = match read_http_request(&mut stream) {
        Ok(request) => request,
        Err(err) => {
            let _ = write_http_response(
                &mut stream,
                "400 Bad Request",
                &[],
                b"bad websocket request\n",
            );
            return Err(err);
        }
    };

    let Some(accept_key) = websocket_accept_key(&request) else {
        write_http_response(
            &mut stream,
            "426 Upgrade Required",
            &[("Connection", "close"), ("Upgrade", "websocket")],
            b"websocket upgrade required\n",
        )?;
        return Ok(());
    };

    write_websocket_upgrade(&mut stream, &accept_key)?;

    let mut child = match spawn_command(&opt.command) {
        Ok(child) => child,
        Err(err) => {
            let _ = write_ws_frame(&mut stream, 0x8, &close_payload(1011, "spawn failed"));
            return Err(err);
        }
    };

    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        let _ = write_ws_frame(&mut stream, 0x8, &close_payload(1011, "stdout unavailable"));
        return Err(io::Error::other("child stdout was not piped"));
    };

    let child = Arc::new(Mutex::new(child));
    let disconnected = Arc::new(AtomicBool::new(false));
    let read_stream = stream.try_clone()?;
    let writer = Arc::new(Mutex::new(stream));

    let monitor = spawn_disconnect_monitor(
        read_stream.try_clone()?,
        writer.clone(),
        child.clone(),
        disconnected.clone(),
    );

    let mut buf = [0u8; 16 * 1024];
    loop {
        if disconnected.load(Ordering::SeqCst) {
            break;
        }

        let n = stdout.read(&mut buf)?;
        if n == 0 {
            break;
        }

        let frame_result = {
            let mut writer = writer.lock().unwrap();
            write_ws_frame(&mut writer, 0x2, &buf[..n])
        };

        if frame_result.is_err() {
            disconnected.store(true, Ordering::SeqCst);
            terminate_child(&child);
            break;
        }
    }

    let client_disconnected = disconnected.swap(true, Ordering::SeqCst);
    if client_disconnected {
        terminate_child(&child);
    } else {
        {
            let mut writer = writer.lock().unwrap();
            let _ = write_ws_frame(&mut writer, 0x8, &close_payload(1000, "command finished"));
            let _ = writer.shutdown(Shutdown::Both);
        }
        terminate_child(&child);
    }

    let _ = read_stream.shutdown(Shutdown::Both);
    let _ = monitor.join();

    wait_child(&child);
    Ok(())
}

fn spawn_command(command: &[String]) -> io::Result<Child> {
    Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
}

fn spawn_disconnect_monitor(
    mut stream: TcpStream,
    writer: Arc<Mutex<TcpStream>>,
    child: Arc<Mutex<Child>>,
    disconnected: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        loop {
            match read_client_frame(&mut stream) {
                Ok(frame) => match frame.opcode {
                    0x8 => {
                        disconnected.store(true, Ordering::SeqCst);
                        terminate_child(&child);
                        let mut writer = writer.lock().unwrap();
                        let _ = write_ws_frame(&mut writer, 0x8, &frame.payload);
                        let _ = writer.shutdown(Shutdown::Both);
                        break;
                    }
                    0x9 => {
                        let mut writer = writer.lock().unwrap();
                        if write_ws_frame(&mut writer, 0xA, &frame.payload).is_err() {
                            disconnected.store(true, Ordering::SeqCst);
                            terminate_child(&child);
                            break;
                        }
                    }
                    _ => {}
                },
                Err(_) => {
                    disconnected.store(true, Ordering::SeqCst);
                    terminate_child(&child);
                    let writer = writer.lock().unwrap();
                    let _ = writer.shutdown(Shutdown::Both);
                    break;
                }
            }
        }
    })
}

fn terminate_child(child: &Arc<Mutex<Child>>) {
    let mut child = child.lock().unwrap();
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
}

fn wait_child(child: &Arc<Mutex<Child>>) {
    let mut child = child.lock().unwrap();
    let _ = child.wait();
}

fn read_http_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut raw = Vec::new();

    loop {
        let mut byte = [0u8; 1];
        let n = stream.read(&mut byte)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before headers",
            ));
        }
        raw.push(byte[0]);
        if raw.len() > MAX_HTTP_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP headers too large",
            ));
        }
        if raw.ends_with(b"\r\n\r\n") || raw.ends_with(b"\n\n") {
            break;
        }
    }

    let raw = String::from_utf8(raw)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "headers were not UTF-8"))?;
    let mut lines = raw.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing method"))?
        .to_string();

    let mut headers = Vec::new();
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed HTTP header",
            ));
        };
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }

    Ok(HttpRequest { method, headers })
}

fn websocket_accept_key(request: &HttpRequest) -> Option<String> {
    if !request.method.eq_ignore_ascii_case("GET") {
        return None;
    }
    if !header_eq(request, "Upgrade", "websocket") {
        return None;
    }
    if !header_contains_token(request, "Connection", "upgrade") {
        return None;
    }
    if !header_eq(request, "Sec-WebSocket-Version", "13") {
        return None;
    }

    let key = header_value(request, "Sec-WebSocket-Key")?;
    let mut input = Vec::with_capacity(key.len() + WEBSOCKET_MAGIC.len());
    input.extend_from_slice(key.as_bytes());
    input.extend_from_slice(WEBSOCKET_MAGIC.as_bytes());
    Some(base64_encode(&sha1_digest(&input)))
}

fn header_value<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn header_eq(request: &HttpRequest, name: &str, value: &str) -> bool {
    header_value(request, name).is_some_and(|v| v.eq_ignore_ascii_case(value))
}

fn header_contains_token(request: &HttpRequest, name: &str, token: &str) -> bool {
    header_value(request, name).is_some_and(|value| {
        value
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case(token))
    })
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\n")?;
    write!(stream, "Content-Length: {}\r\n", body.len())?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "\r\n")?;
    stream.write_all(body)
}

fn write_websocket_upgrade(stream: &mut TcpStream, accept_key: &str) -> io::Result<()> {
    write!(
        stream,
        concat!(
            "HTTP/1.1 101 Switching Protocols\r\n",
            "Upgrade: websocket\r\n",
            "Connection: Upgrade\r\n",
            "Sec-WebSocket-Accept: {}\r\n",
            "\r\n"
        ),
        accept_key
    )
}

fn write_ws_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> io::Result<()> {
    stream.write_all(&[0x80 | (opcode & 0x0F)])?;
    match payload.len() {
        len @ 0..=125 => stream.write_all(&[len as u8])?,
        len @ 126..=65535 => {
            stream.write_all(&[126])?;
            stream.write_all(&(len as u16).to_be_bytes())?;
        }
        len => {
            stream.write_all(&[127])?;
            stream.write_all(&(len as u64).to_be_bytes())?;
        }
    }
    stream.write_all(payload)
}

fn read_client_frame(stream: &mut TcpStream) -> io::Result<ClientFrame> {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header)?;

    let opcode = header[0] & 0x0F;
    let masked = header[1] & 0x80 != 0;
    if !masked {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "client frame was not masked",
        ));
    }

    let len = match header[1] & 0x7F {
        len @ 0..=125 => u64::from(len),
        126 => {
            let mut buf = [0u8; 2];
            stream.read_exact(&mut buf)?;
            u64::from(u16::from_be_bytes(buf))
        }
        127 => {
            let mut buf = [0u8; 8];
            stream.read_exact(&mut buf)?;
            u64::from_be_bytes(buf)
        }
        _ => unreachable!(),
    };

    let mut mask = [0u8; 4];
    stream.read_exact(&mut mask)?;

    if matches!(opcode, 0x8..=0xA) && len > 125 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control frame payload too large",
        ));
    }

    let keep_payload = matches!(opcode, 0x8..=0xA);
    let payload = if keep_payload {
        let mut payload = vec![0u8; len as usize];
        stream.read_exact(&mut payload)?;
        for (idx, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[idx % 4];
        }
        payload
    } else {
        drain_masked_payload(stream, len, mask)?;
        Vec::new()
    };

    Ok(ClientFrame { opcode, payload })
}

fn drain_masked_payload(stream: &mut TcpStream, mut len: u64, mask: [u8; 4]) -> io::Result<()> {
    let mut offset = 0usize;
    let mut buf = [0u8; 4096];
    while len > 0 {
        let n = len.min(buf.len() as u64) as usize;
        stream.read_exact(&mut buf[..n])?;
        for byte in &mut buf[..n] {
            *byte ^= mask[offset % 4];
            offset += 1;
        }
        len -= n as u64;
    }
    Ok(())
}

fn close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + reason.len().min(123));
    payload.extend_from_slice(&code.to_be_bytes());
    payload.extend_from_slice(&reason.as_bytes()[..reason.len().min(123)]);
    payload
}

fn shellish_command(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| {
            if arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./:".contains(c))
            {
                arg.clone()
            } else {
                format!("{arg:?}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn sha1_digest(input: &[u8]) -> [u8; 20] {
    let mut h0 = 0x67452301u32;
    let mut h1 = 0xEFCDAB89u32;
    let mut h2 = 0x98BADCFEu32;
    let mut h3 = 0x10325476u32;
    let mut h4 = 0xC3D2E1F0u32;

    let bit_len = (input.len() as u64) * 8;
    let mut msg = input.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (idx, word) in w.iter_mut().take(16).enumerate() {
            let start = idx * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for idx in 16..80 {
            w[idx] = (w[idx - 3] ^ w[idx - 8] ^ w[idx - 14] ^ w[idx - 16]).rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for (idx, word) in w.iter().enumerate() {
            let (f, k) = match idx {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);

    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_sha1() {
        let digest = sha1_digest(b"abc");
        let hex = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn computes_websocket_accept_key() {
        let request = HttpRequest {
            method: "GET".to_string(),
            headers: vec![
                ("Upgrade".to_string(), "websocket".to_string()),
                ("Connection".to_string(), "keep-alive, Upgrade".to_string()),
                ("Sec-WebSocket-Version".to_string(), "13".to_string()),
                (
                    "Sec-WebSocket-Key".to_string(),
                    "dGhlIHNhbXBsZSBub25jZQ==".to_string(),
                ),
            ],
        };
        assert_eq!(
            websocket_accept_key(&request).as_deref(),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
    }
}
