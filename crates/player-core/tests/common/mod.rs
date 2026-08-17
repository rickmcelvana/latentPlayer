//! A tiny HTTP server the tests can misbehave with.
//!
//! Phase 2 needs this (tasks/phase-2.md, "What verified has to mean here"):
//! Phase 1's checks could synthesise a complete fixture -- a WAV with a ramp in
//! it is a whole test input -- and a network protocol cannot be checked that way
//! alone. The failure paths that matter here are the ones nobody can produce on
//! cue against a real server: an error envelope carried by an HTTP 200, a
//! response with no `Accept-Ranges`, a connection that stops mid-body, a 500
//! from a reverse proxy. This serves all of them, deterministically, over real
//! TCP with a real HTTP client on the other end.
//!
//! Shared by more than one test file on purpose -- M2's remote source has to
//! misbehave against the same server this one does.

// A `tests/common` module is compiled into every test binary that includes it,
// so whatever one binary does not call is dead code in that binary. The alloweds
// here are that, not unused helpers: `Hang` and `request_count` are M2's.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// What the server should do when a path is asked for.
#[derive(Clone)]
pub enum Reply {
    /// A normal reply: status, content type, body.
    Body {
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
    },
    /// Headers promising more than the body delivers, then a closed socket --
    /// what a server going away mid-transfer looks like to a client.
    Truncated { promised: usize, sent: Vec<u8> },
    /// Accept the connection and never answer, so the client's own timeout is
    /// the only thing that ends it.
    Hang,
    /// A body served the way a file server serves one: `Accept-Ranges: bytes`,
    /// and a `Range` request answered with 206 and a `Content-Range`. This is
    /// what M2's remote source is built against, and `Body` is then the *other*
    /// case it has to survive -- a server that ignores the range and sends
    /// everything (a transcoding endpoint, per phase-2 decision 7).
    Ranged { body: Vec<u8> },
}

impl Reply {
    pub fn json(body: &str) -> Self {
        Self::Body {
            status: 200,
            content_type: "application/json",
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn status(status: u16, body: &str) -> Self {
        Self::Body {
            status,
            content_type: "text/plain",
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn text(body: &str) -> Self {
        Self::Body {
            status: 200,
            content_type: "text/html",
            body: body.as_bytes().to_vec(),
        }
    }
}

/// One request as the server saw it, so a test can assert on what the client
/// actually sent rather than on what it meant to send.
#[derive(Clone, Debug)]
pub struct Seen {
    pub path: String,
    pub query: HashMap<String, String>,
    /// The `Range` header as sent, so a test can assert on what was actually
    /// asked for rather than on how much came back.
    pub range: Option<String>,
}

impl Seen {
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query.get(key).map(String::as_str)
    }
}

pub struct FakeServer {
    port: u16,
    replies: Arc<Mutex<HashMap<String, Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeServer {
    /// Binds an ephemeral port on loopback and starts serving.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let port = listener.local_addr().expect("read the bound port").port();
        // So the accept loop wakes up often enough to notice `stop` rather than
        // blocking forever on a server nobody is talking to any more.
        listener
            .set_nonblocking(true)
            .expect("make the listener non-blocking");

        let replies: Arc<Mutex<HashMap<String, Reply>>> = Arc::new(Mutex::new(HashMap::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let replies = Arc::clone(&replies);
            let seen = Arc::clone(&seen);
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // The listener is non-blocking so the accept loop can
                            // notice `stop`; on Windows an accepted socket
                            // *inherits* that, and then every read on it can
                            // return WouldBlock before the request has arrived.
                            // This server treated that as "the client hung up"
                            // and closed the connection mid-request, which the
                            // client saw as an aborted connection -- intermittent
                            // under parallel tests, invisible when run alone, and
                            // entirely this side's fault.
                            let _ = stream.set_nonblocking(false);
                            let replies = Arc::clone(&replies);
                            let seen = Arc::clone(&seen);
                            let requests = Arc::clone(&requests);
                            let stop = Arc::clone(&stop);
                            // One thread per connection: a `Hang` reply holds
                            // its connection for the length of the client's
                            // timeout, and must not hold up the next test.
                            std::thread::spawn(move || {
                                serve(stream, &replies, &seen, &requests, &stop);
                            });
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };

        Self {
            port,
            replies,
            seen,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    /// The base URL to hand a client, in the shape a user would have typed.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// What to answer for one Subsonic endpoint (`ping`, `getOpenSubsonicExtensions`, ...).
    pub fn on(&self, endpoint: &str, reply: Reply) {
        self.replies
            .lock()
            .expect("the reply table is not poisoned")
            .insert(format!("/rest/{endpoint}"), reply);
    }

    /// Every request served so far, in order.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .expect("the request log is not poisoned")
            .clone()
    }

    /// The most recent request, for the common case.
    pub fn last(&self) -> Option<Seen> {
        self.seen().last().cloned()
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Acquire)
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    mut stream: TcpStream,
    replies: &Mutex<HashMap<String, Reply>>,
    seen: &Mutex<Vec<Seen>>,
    requests: &AtomicUsize,
    stop: &AtomicBool,
) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    // One connection, as many requests as the client sends down it. Serving one
    // and hanging up would be simpler and is *not* how any real server behaves:
    // a client that pools connections -- which ureq does, and which is the whole
    // reason a chunked read of a track is not a TCP handshake per 512KB -- then
    // reuses a connection this side has already closed, and the second range
    // request of every track dies with "an established connection was aborted by
    // the software in your host machine". That looked exactly like a bug in the
    // client and was a bug in this test double.
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if !serve_one(&mut stream, &mut reader, replies, seen, requests, stop) {
            return;
        }
    }
}

/// Serves one request. Returns whether the connection may carry another.
fn serve_one(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    replies: &Mutex<HashMap<String, Reply>>,
    seen: &Mutex<Vec<Seen>>,
    requests: &AtomicUsize,
    stop: &AtomicBool,
) -> bool {
    let mut request_line = String::new();
    match reader.read_line(&mut request_line) {
        // The client closed the connection, which is how this normally ends.
        Ok(0) => return false,
        Ok(_) => {}
        Err(_) => return false,
    }
    // Headers have to be drained either way -- leaving them in the socket makes
    // some clients wait on the write side -- and `Range` is kept, since serving
    // it is the whole point of `Reply::Ranged`.
    let mut range: Option<String> = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => {
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("range") {
                        range = Some(value.trim().to_string());
                    }
                }
                continue;
            }
            Err(_) => return false,
        }
    }

    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), parse_query(query)),
        None => (target.to_string(), HashMap::new()),
    };

    requests.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut log) = seen.lock() {
        log.push(Seen {
            path: path.clone(),
            query,
            range: range.clone(),
        });
    }

    let reply = replies
        .lock()
        .ok()
        .and_then(|table| table.get(&path).cloned())
        .unwrap_or_else(|| Reply::status(404, "no reply configured for this path"));

    match reply {
        Reply::Body {
            status,
            content_type,
            body,
        } => {
            let head = format!(
                "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
                reason(status),
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
            true
        }
        Reply::Truncated { promised, sent } => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {promised}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&sent);
            let _ = stream.flush();
            // Ending the connection here is the point: the client is still owed
            // `promised - sent.len()` bytes that will never come.
            false
        }
        Reply::Ranged { body } => {
            // Headers built line by line rather than as one long literal: the
            // separator is CRLF and getting that wrong produces a header name
            // no client will parse, which is exactly the bug this arm had.
            let total = body.len() as u64;
            let (status, mut headers, slice): (&str, Vec<String>, &[u8]) =
                match range.as_deref().and_then(parse_range) {
                    Some((first, _)) if first >= total => {
                        // What a real server answers for a range past the end.
                        let head = [
                            "HTTP/1.1 416 Range Not Satisfiable",
                            "Content-Length: 0",
                            "",
                            "",
                        ]
                        .join("\r\n");
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.flush();
                        return true;
                    }
                    Some((first, last)) => {
                        let last = last.unwrap_or(total - 1).min(total.saturating_sub(1));
                        (
                            "HTTP/1.1 206 Partial Content",
                            vec![format!("Content-Range: bytes {first}-{last}/{total}")],
                            &body[first as usize..=last as usize],
                        )
                    }
                    // No range asked for: the whole thing, the way any file
                    // server answers a plain GET.
                    None => ("HTTP/1.1 200 OK", Vec::new(), &body[..]),
                };

            headers.push("Content-Type: application/octet-stream".to_string());
            headers.push("Accept-Ranges: bytes".to_string());
            headers.push(format!("Content-Length: {}", slice.len()));

            let mut head = String::from(status);
            for header in &headers {
                head.push_str("\r\n");
                head.push_str(header);
            }
            head.push_str("\r\n\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(slice);
            let _ = stream.flush();
            true
        }
        Reply::Hang => {
            // This one *wants* to poll rather than block, so it opts back out of
            // the blocking mode set at accept.
            let _ = stream.set_nonblocking(true);
            while !stop.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(20));
                // Notice a client that gave up, so this thread does not outlive
                // the test that created it.
                let mut probe = [0u8; 1];
                match stream.read(&mut probe) {
                    Ok(0) => break,
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(_) => break,
                    Ok(_) => continue,
                }
            }
            false
        }
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
        .collect()
}

/// Percent-decoding, so a test can assert on what the client *meant* rather
/// than on its escaping. Deliberately a separate implementation from the
/// encoder under test: a decoder that shared its bugs would agree with it.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Status",
    }
}

/// `bytes=0-511` / `bytes=512-` -> (first, last).
fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    let (first, last) = spec.split_once('-')?;
    let first = first.trim().parse::<u64>().ok()?;
    let last = last.trim();
    let last = if last.is_empty() {
        None
    } else {
        Some(last.parse::<u64>().ok()?)
    };
    Some((first, last))
}

/// A 16-bit stereo WAV in memory: left a 220Hz tone, right a ramp from 0 to 1
/// across the track, so any decoded sample says where in the track it came
/// from. The same fixture shape `transport_smoke` uses, as bytes rather than as
/// a file, because a server serves bytes.
pub fn wav_bytes(sample_rate: u32, seconds: f32) -> Vec<u8> {
    let frames = (sample_rate as f32 * seconds) as usize;
    let (channels, bits) = (2u16, 16u16);
    let block_align = channels * bits / 8;
    let data_len = frames * block_align as usize;

    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for i in 0..frames {
        let t = 2.0 * std::f32::consts::PI * 220.0 * i as f32 / sample_rate as f32;
        let left = (t.sin() * 0.7 * 32_767.0) as i16;
        let right = ((i as f32 / frames as f32) * 32_767.0) as i16;
        out.extend_from_slice(&left.to_le_bytes());
        out.extend_from_slice(&right.to_le_bytes());
    }
    out
}
