//! Unix-domain-socket MCP server.
//!
//! * socket mode 0600 in a private directory, stale sockets reclaimed,
//!   symlinks and foreign files refused;
//! * every connection's peer UID must equal ours (`getpeereid` / `SO_PEERCRED`);
//! * one thread per client (max [`MAX_CLIENTS`]), newline-delimited JSON-RPC,
//!   request lines capped at [`MAX_LINE_BYTES`], [`RATE_PER_SEC`] requests/s.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use super::protocol::{self, Session};
use super::{Activity, Backend, Outcome, Shared, MAX_CLIENTS, MAX_LINE_BYTES, RATE_PER_SEC};
use crate::ai::chat::json::Json;

/// Token bucket: `rate` tokens/second, bursts up to `rate`.
#[derive(Debug)]
pub struct RateLimiter {
    rate: f64,
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(per_sec: u32, now: Instant) -> Self {
        Self { rate: per_sec as f64, tokens: per_sec as f64, last: now }
    }

    pub fn allow(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.rate);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Result of reading one framed line.
#[derive(Debug, PartialEq)]
pub enum Line {
    Eof,
    TooLong,
    Text(String),
}

/// Read a `\n`-terminated line of at most `max` bytes without buffering more.
pub fn read_line_capped<R: BufRead>(r: &mut R, max: usize) -> io::Result<Line> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(max as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(Line::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        if buf.len() > max {
            return Ok(Line::TooLong);
        }
    } else if buf.len() > max {
        return Ok(Line::TooLong);
    }
    Ok(Line::Text(String::from_utf8_lossy(&buf).into_owned()))
}

// ---- peer credentials -------------------------------------------------------

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"))]
fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
    // SAFETY: the fd is a valid socket for the duration of the call and
    // `uid`/`gid` are valid out-pointers.
    let rc = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
    if rc == 0 { Ok(uid as u32) } else { Err(io::Error::last_os_error()) }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred`/`len` are valid for writes of the sizes given; the fd is a valid socket.
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 { Ok(cred.uid as u32) } else { Err(io::Error::last_os_error()) }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly", target_os = "linux", target_os = "android")))]
fn peer_uid(_s: &UnixStream) -> io::Result<u32> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "peer credentials are not available on this platform"))
}

/// Same-user check. Fails closed.
fn peer_is_us(s: &UnixStream) -> bool {
    // SAFETY: geteuid has no preconditions.
    matches!(peer_uid(s), Ok(uid) if uid == unsafe { libc::geteuid() } as u32)
}

// ---- socket setup -------------------------------------------------------------

/// Bind `path` as a private (0600) socket, reclaiming a stale one.
pub fn bind_private(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if !dir.exists() {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
    }
    match std::fs::symlink_metadata(path) {
        Ok(md) if md.file_type().is_socket() => {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, "another Rift instance is already serving MCP"));
            }
            std::fs::remove_file(path)?; // stale socket from a crashed run
        }
        Ok(_) => {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists and is not a socket", path.display())));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    // Create it 0600 from the start (umask), then make that explicit.
    // SAFETY: umask is always safe to call; the old mask is restored right away.
    let old = unsafe { libc::umask(0o177) };
    let bound = UnixListener::bind(path);
    // SAFETY: as above.
    unsafe { libc::umask(old) };
    let listener = bound?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Keeps the server alive; dropping it stops accepting and removes the socket.
pub struct ServerHandle {
    path: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock accept(), then remove our socket file.
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Start serving on `path` in a background thread.
pub fn start(path: &Path, backend: Arc<dyn Backend>, shared: Arc<Shared>) -> io::Result<ServerHandle> {
    let listener = bind_private(path)?;
    let stop = Arc::new(AtomicBool::new(false));
    let handle = ServerHandle { path: path.to_path_buf(), stop: stop.clone() };
    std::thread::Builder::new().name("mcp-accept".into()).spawn(move || accept_loop(listener, backend, shared, stop))?;
    Ok(handle)
}

fn accept_loop(listener: UnixListener, backend: Arc<dyn Backend>, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = conn else { continue };
        if !peer_is_us(&stream) {
            log::warn!("mcp: rejected a connection from another user");
            continue;
        }
        if shared.clients() >= MAX_CLIENTS {
            log::warn!("mcp: too many clients, rejecting");
            continue;
        }
        let (b, s) = (backend.clone(), shared.clone());
        let client = s.client_connected();
        crate::wake::wake(); // repaint the "MCP · n clients" indicator
        let spawned = std::thread::Builder::new().name(format!("mcp-client-{client}")).spawn(move || {
            serve_client(stream, client, &*b, &s);
            s.client_gone();
            crate::wake::wake();
        });
        if spawned.is_err() {
            shared.client_gone();
        }
    }
}

fn write_line(w: &mut impl Write, s: &str) -> io::Result<()> {
    w.write_all(s.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Serve one connection until EOF. Public for tests.
pub fn serve_client(stream: UnixStream, client: u64, backend: &dyn Backend, shared: &Shared) {
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let mut sess = Session::new(client);
    let mut limiter = RateLimiter::new(RATE_PER_SEC, Instant::now());
    loop {
        let line = match read_line_capped(&mut reader, MAX_LINE_BYTES) {
            Ok(Line::Text(l)) => l,
            Ok(Line::Eof) | Err(_) => break,
            Ok(Line::TooLong) => {
                let _ = write_line(&mut writer, &protocol::error_response("null", protocol::INVALID_REQUEST, "Request too large"));
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = if limiter.allow(Instant::now()) {
            protocol::handle_line(&mut sess, &line, backend, shared)
        } else {
            rate_limited(&line, client, shared)
        };
        if let Some(r) = reply {
            if write_line(&mut writer, &r).is_err() {
                break;
            }
        }
    }
}

/// Answer an over-limit request (requests get an error; notifications are dropped).
fn rate_limited(line: &str, client: u64, shared: &Shared) -> Option<String> {
    let msg = Json::parse(line.trim())?;
    let id = match msg.get("id") {
        Some(v @ (Json::Num(_) | Json::Str(_))) => protocol::ser(v),
        _ => return None,
    };
    let method = msg.get("method").and_then(Json::as_str).unwrap_or("?");
    shared.log(Activity {
        at: Instant::now(),
        client,
        tool: method.chars().take(40).collect(),
        summary: String::new(),
        outcome: Outcome::RateLimited,
        ms: 0,
    });
    Some(protocol::error_response(&id, protocol::RATE_LIMITED, &format!("Rate limit exceeded ({RATE_PER_SEC} requests/second); slow down")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::protocol::tests::FakeBackend;
    use crate::mcp::{AllowRun, AppRequest, Reply};
    use std::io::Cursor;
    use std::time::Duration;

    #[test]
    fn rate_limiter_allows_burst_then_throttles_then_refills() {
        let t0 = Instant::now();
        let mut rl = RateLimiter::new(20, t0);
        let allowed = (0..30).filter(|_| rl.allow(t0)).count();
        assert_eq!(allowed, 20, "burst equals the per-second budget");
        assert!(!rl.allow(t0 + Duration::from_millis(10)));
        // 100 ms later 2 tokens are back.
        let t1 = t0 + Duration::from_millis(110);
        assert!(rl.allow(t1));
        assert!(rl.allow(t1));
        assert!(!rl.allow(t1));
        // A long pause refills to the cap, not beyond.
        let t2 = t1 + Duration::from_secs(60);
        let n = (0..50).filter(|_| rl.allow(t2)).count();
        assert_eq!(n, 20);
    }

    #[test]
    fn line_reader_frames_caps_and_handles_crlf() {
        let mut c = Cursor::new(b"abc\r\ndef\nlast".to_vec());
        assert_eq!(read_line_capped(&mut c, 100).unwrap(), Line::Text("abc".into()));
        assert_eq!(read_line_capped(&mut c, 100).unwrap(), Line::Text("def".into()));
        assert_eq!(read_line_capped(&mut c, 100).unwrap(), Line::Text("last".into()));
        assert_eq!(read_line_capped(&mut c, 100).unwrap(), Line::Eof);
        let mut c = Cursor::new(vec![b'x'; 50]);
        assert_eq!(read_line_capped(&mut c, 10).unwrap(), Line::TooLong);
        let mut c = Cursor::new(b"0123456789\n".to_vec());
        assert_eq!(read_line_capped(&mut c, 10).unwrap(), Line::Text("0123456789".into()));
        let mut c = Cursor::new(b"01234567890\n".to_vec());
        assert_eq!(read_line_capped(&mut c, 10).unwrap(), Line::TooLong);
    }

    fn temp_sock(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rift-mcp-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("mcp.sock")
    }

    fn fake() -> Arc<FakeBackend> {
        Arc::new(FakeBackend::new(|req| match req {
            AppRequest::ListPanes => Reply::ok(r#"{"panes":[{"id":0}]}"#),
            _ => Reply::err("unsupported in fake"),
        }))
    }

    fn rpc(w: &mut UnixStream, r: &mut BufReader<UnixStream>, line: &str) -> Json {
        write_line(w, line).unwrap();
        let mut out = String::new();
        r.read_line(&mut out).unwrap();
        Json::parse(&out).unwrap_or_else(|| panic!("bad response: {out:?}"))
    }

    #[test]
    fn socket_round_trip_with_fake_backend() {
        let path = temp_sock("rt");
        let shared = Arc::new(Shared::new(AllowRun::Ask));
        let backend = fake();
        let handle = start(&path, backend.clone(), shared.clone()).expect("start");

        // Private permissions on the socket (and its fresh directory).
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket must be 0600");
        let dmode = std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700);

        let mut w = UnixStream::connect(&path).unwrap();
        let mut r = BufReader::new(w.try_clone().unwrap());
        let init = rpc(&mut w, &mut r, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"t"}}}"#);
        assert_eq!(init.get("result").and_then(|x| x.get("protocolVersion")).and_then(Json::as_str), Some("2025-06-18"));
        write_line(&mut w, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        let list = rpc(&mut w, &mut r, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        assert!(list.get("result").and_then(|x| x.get("tools")).and_then(Json::as_arr).is_some_and(|t| t.len() >= 5));
        let call = rpc(&mut w, &mut r, r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_panes","arguments":{}}}"#);
        let text = call.get("result").and_then(|x| x.get("content")).and_then(|c| c.idx(0)).and_then(|c| c.get("text")).and_then(Json::as_str).unwrap();
        assert_eq!(text, r#"{"panes":[{"id":0}]}"#);
        let bad = rpc(&mut w, &mut r, "this is not json");
        assert_eq!(bad.get("error").and_then(|e| e.get("code")).and_then(Json::as_f64), Some(protocol::PARSE_ERROR as f64));

        // Client indicator and activity log.
        assert_eq!(shared.clients(), 1);
        assert_eq!(shared.recent(5)[0].tool, "list_panes");
        drop(w);
        drop(r);
        for _ in 0..100 {
            if shared.clients() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(shared.clients(), 0, "disconnect is noticed");

        drop(handle);
        assert!(!path.exists(), "socket removed on shutdown");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn second_server_refuses_a_live_socket_and_reclaims_a_stale_one() {
        let path = temp_sock("stale");
        let shared = Arc::new(Shared::new(AllowRun::Ask));
        let first = start(&path, fake(), shared.clone()).unwrap();
        let err = start(&path, fake(), shared.clone()).err().expect("second start must fail");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        drop(first);

        // A leftover socket file with nobody listening is reclaimed.
        let l = UnixListener::bind(&path).unwrap();
        drop(l);
        assert!(path.exists());
        let again = start(&path, fake(), shared).expect("reclaim stale socket");
        drop(again);

        // A regular file in the way is never deleted.
        std::fs::write(&path, b"precious").unwrap();
        let err = bind_private(&path).err().expect("must refuse");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"precious");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn peer_credential_check_accepts_our_own_connection() {
        let (a, b) = UnixStream::pair().unwrap();
        assert!(peer_is_us(&a));
        assert!(peer_is_us(&b));
    }

    #[test]
    fn flooding_gets_rate_limit_errors_not_dropped_connections() {
        let (client, server) = UnixStream::pair().unwrap();
        let shared = Arc::new(Shared::new(AllowRun::Ask));
        let b = fake();
        let s2 = shared.clone();
        let t = std::thread::spawn(move || serve_client(server, 7, &*b, &s2));
        let mut w = client.try_clone().unwrap();
        let mut r = BufReader::new(client);
        let n = 60;
        for i in 0..n {
            write_line(&mut w, &format!(r#"{{"jsonrpc":"2.0","id":{i},"method":"ping"}}"#)).unwrap();
        }
        let mut limited = 0;
        for _ in 0..n {
            let mut out = String::new();
            r.read_line(&mut out).unwrap();
            let j = Json::parse(&out).unwrap();
            if j.get("error").and_then(|e| e.get("code")).and_then(Json::as_f64) == Some(protocol::RATE_LIMITED as f64) {
                limited += 1;
            }
        }
        assert!(limited >= 20, "expected throttling, got {limited} limited of {n}");
        assert!(shared.recent(100).iter().any(|a| a.outcome == Outcome::RateLimited));
        drop(w);
        drop(r);
        t.join().unwrap();
    }

    #[test]
    fn oversized_request_closes_the_connection_with_an_error() {
        let (client, server) = UnixStream::pair().unwrap();
        let shared = Arc::new(Shared::new(AllowRun::Ask));
        let b = fake();
        let s2 = shared.clone();
        let t = std::thread::spawn(move || serve_client(server, 1, &*b, &s2));
        let mut w = client.try_clone().unwrap();
        let big = vec![b'a'; MAX_LINE_BYTES + 10];
        let writer = std::thread::spawn(move || {
            let _ = w.write_all(&big);
            let _ = w.write_all(b"\n");
        });
        let mut r = BufReader::new(client);
        let mut out = String::new();
        r.read_line(&mut out).unwrap();
        assert!(out.contains("Request too large"), "{out}");
        t.join().unwrap();
        let _ = writer.join();
    }
}
