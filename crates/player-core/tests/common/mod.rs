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
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Headers are read and discarded -- nothing here depends on them yet, but
    // leaving them in the socket makes some clients wait on the write side.
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => continue,
            Err(_) => return,
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
                "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                reason(status),
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
        Reply::Truncated { promised, sent } => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {promised}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&sent);
            let _ = stream.flush();
            // Dropping the stream here is the point: the client is still owed
            // `promised - sent.len()` bytes that will never come.
        }
        Reply::Hang => {
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
