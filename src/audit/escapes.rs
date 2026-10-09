//! Item 9 (+ critic H4): malicious / pathological escape sequences.
use super::{pane, Soft};
use std::io::Write;
use std::time::{Duration, Instant};

fn rss_kb() -> u64 {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

/// Feed `data` into a fresh pane on a worker thread; report (elapsed_ms, panicked, rss_delta_MB, hung).
fn run_case(cols: usize, rows: usize, budget: Duration, data: Vec<u8>) -> (u128, Option<String>, i64, bool) {
    let _rss = super::rss_serial();
    let (tx, rx) = std::sync::mpsc::channel();
    let rss0 = rss_kb() as i64;
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let r = std::panic::catch_unwind(move || {
            let mut p = pane(cols, rows);
            p.feed(&data);
            (p.terminal.cursor_row, p.terminal.cursor_col, p.terminal.title.as_ref().map(|t| t.len()))
        });
        let msg = r.err().map(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "panic".into()));
        let _ = tx.send((t0.elapsed().as_millis(), msg));
    });
    match rx.recv_timeout(budget) {
        Ok((ms, msg)) => (ms, msg, (rss_kb() as i64 - rss0) / 1024, false),
        Err(_) => (budget.as_millis(), None, (rss_kb() as i64 - rss0) / 1024, true),
    }
}

fn case(s: &mut Soft, id: &str, cols: usize, rows: usize, data: Vec<u8>, max_ms: u64, max_mb: i64) {
    let n = data.len();
    // Wall-time and RSS-delta limits are sensitive to whatever else the test
    // process is doing (other tests run in parallel): a limit overshoot is
    // retried, a real regression overshoots every time. Panics / hangs are final.
    let mut attempt = 0;
    let (ms, panic, mb, hung, ok) = loop {
        attempt += 1;
        let (ms, panic, mb, hung) = run_case(cols, rows, Duration::from_secs(20), data.clone());
        let ok = panic.is_none() && !hung && (ms as u64) <= max_ms && mb <= max_mb;
        if ok || panic.is_some() || hung || attempt == 3 {
            break (ms, panic, mb, hung, ok);
        }
    };
    s.check(id, ok, format!("input {} bytes on {cols}x{rows}: {ms} ms, RSS delta {mb} MB{}{}", n, panic.map_or(String::new(), |p| format!(", PANIC: {p}")), if hung { ", HUNG >20s" } else { "" }));
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn csi_and_cursor_extremes() {
    let mut s = Soft::new("escapes");
    case(&mut s, "csi_H_99999999", 80, 24, b"\x1b[99999999;99999999Hx\x1b[99999999;1Hy\x1b[1;99999999Hz".to_vec(), 500, 50);
    for f in ["A", "B", "C", "D", "E", "F", "G", "d", "S", "T", "L", "M", "P", "X", "@", "b", "I", "Z"] {
        case(&mut s, &format!("csi_{f}_65535"), 200, 60, format!("text\r\n\x1b[65535{f}x\x1b[99999999{f}y").into_bytes(), 3000, 200);
    }
    // 20k params
    let mut p = b"\x1b[".to_vec();
    for _ in 0..20_000 { p.extend_from_slice(b"1;"); }
    p.extend_from_slice(b"mHELLO");
    case(&mut s, "csi_20000_params", 80, 24, p, 500, 50);
    // overlong param digits
    let mut p = b"\x1b[".to_vec();
    p.extend(std::iter::repeat(b'9').take(5_000_000));
    p.extend_from_slice(b"HX");
    case(&mut s, "csi_5M_digits", 80, 24, p, 1000, 100);
    // scroll with 400 rows and huge S
    case(&mut s, "scroll_up_65535_on_400_rows", 400, 400, b"\x1b[65535S\x1b[65535T\x1b[65535L\x1b[65535M".to_vec(), 3000, 300);
    // DECSTBM degenerate
    case(&mut s, "decstbm_degenerate", 80, 24, b"\x1b[0;0r\x1b[99;1r\x1b[5;3r\x1b[1;1r\x1b[24;24r\x1b[3;4rA\r\n\r\n\r\n\r\n\r\n\x1bM\x1bM\x1bM\x1b[5L\x1b[5M".to_vec(), 500, 50);
    // tiny terminals
    for (c, r) in [(1usize, 1usize), (1, 2), (2, 1), (2, 2)] {
        case(&mut s, &format!("tiny_{c}x{r}_mixed"), c, r, "a中b\t\r\n\x1b[2J\x1b[H你好é\u{301}\x1b[3;3H\x1b[@\x1b[P\x1b[L\x1b[M\x1b[S\x1b[T\x1b[10b\x1bM\x1bD\x1b7\x1b8\x1b[?1049h\x1b[?1049l".as_bytes().to_vec(), 500, 50);
    }
    s.finish();
}

#[test]
fn osc_dcs_apc_sizes() {
    let mut s = Soft::new("escapes");
    let big = |n: usize| std::iter::repeat(b'A').take(n).collect::<Vec<u8>>();
    let mut v = b"\x1b]2;".to_vec(); v.extend(big(64 << 20)); v.push(7);
    case(&mut s, "osc2_title_64MB", 80, 24, v, 3000, 300);
    let mut v = b"\x1b]52;c;".to_vec(); v.extend(big(64 << 20)); v.push(7);
    case(&mut s, "osc52_set_64MB", 80, 24, v, 3000, 300);
    let mut v = b"\x1b]7;file:///".to_vec(); v.extend(big(32 << 20)); v.push(7);
    case(&mut s, "osc7_32MB", 80, 24, v, 3000, 300);
    let mut v = b"\x1b]1337;CurrentDir=/".to_vec(); v.extend(big(32 << 20)); v.push(7);
    case(&mut s, "osc1337_32MB", 80, 24, v, 3000, 300);
    // unterminated OSC then normal text: parser recovers?
    let mut p = pane(80, 24);
    p.feed(b"\x1b]0;unterminated title ");
    p.feed(b"plain text after");
    let t = super::screen_text(&p.terminal);
    s.info("osc_unterminated_then_text", format!("screen={t:?} (vte keeps swallowing input until ST/BEL/ESC/CAN/SUB)"));
    p.feed(b"\x18recovered");
    s.check("can_aborts_osc", super::screen_text(&p.terminal).contains("recovered"), "CAN (0x18) returns to ground state");
    // DCS: DECRQSS, sixel, huge
    case(&mut s, "decrqss", 80, 24, b"\x1bP$qm\x1b\\\x1bP$q\"p\x1b\\\x1bP$qr\x1b\\x".to_vec(), 500, 50);
    let mut v = b"\x1bPq\"1;1;99999;99999#0;2;0;0;0".to_vec();
    for _ in 0..1000 { v.extend_from_slice(b"!65535~-"); }
    v.extend_from_slice(b"\x1b\\X");
    case(&mut s, "sixel_huge_raster", 80, 24, v, 1000, 100);
    let mut v = b"\x1bP1;1;0q".to_vec(); v.extend(big(48 << 20)); v.extend_from_slice(b"\x1b\\X");
    case(&mut s, "dcs_48MB", 80, 24, v, 3000, 200);
    // APC unterminated 64MB+ cap
    let mut v = b"\x1b_Gf=32,s=10,v=10;".to_vec(); v.extend(big(80 << 20));
    case(&mut s, "apc_unterminated_80MB", 80, 24, v, 4000, 300);
    s.finish();
}

#[test]
fn invalid_utf8_and_random_soup() {
    let mut s = Soft::new("escapes");
    let mut r = Rng(0x1234_5678_9abc_def1);
    let bin: Vec<u8> = (0..20 << 20).map(|_| r.next() as u8).collect();
    case(&mut s, "random_binary_20MB", 120, 40, bin, 5000, 200);
    case(&mut s, "overlong_and_surrogates", 80, 24, b"\xc0\xaf\xe0\x80\xaf\xed\xa0\x80\xf4\x90\x80\x80\xff\xfe\x80\x80\xf8\x88\x80\x80\x80ok\xe4\xb8".to_vec(), 500, 50);
    // escape soup: bytes biased toward ESC [ ? ; digits and final bytes
    let alphabet: &[u8] = b"\x1b\x1b[[]]()?;;;0123456789 !\"#$%&'*+,-./:<=>@ABCDEFGHIJKLMNOPQRSTUVWXYZ^_`abcdefghijklmnopqrstuvwxyz{|}~\x07\x08\x09\x0a\x0d\x0e\x0f\x18\x1a\\PXg";
    for (cols, rows, seed) in [(80usize, 24usize, 1u64), (1, 1, 2), (3, 2, 3), (200, 60, 4), (7, 300, 5)] {
        let mut rr = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let soup: Vec<u8> = (0..16 << 20).map(|_| alphabet[(rr.next() >> 8) as usize % alphabet.len()]).collect();
        case(&mut s, &format!("escape_soup_16MB_{cols}x{rows}"), cols, rows, soup, 15000, 600);
    }
    s.finish();
}

#[test]
fn query_response_amplification_and_clipboard_h4() {
    let _rss = super::rss_serial();
    let mut s = Soft::new("escapes");
    // CPR/DA flood: response_queue is unbounded
    let mut p = pane(80, 24);
    let rss0 = rss_kb() as i64;
    let q: Vec<u8> = b"\x1b[6n".iter().cycle().take(4 * 5_000_000).copied().collect();
    let t0 = Instant::now();
    p.feed(&q);
    let n = p.terminal.response_queue.len();
    s.check("dsr_flood_response_queue_bounded", n < 10_000, format!("5,000,000 x CSI 6n (20 MB) queued {n} responses ({} MB RSS growth, {} ms)", (rss_kb() as i64 - rss0) / 1024, t0.elapsed().as_millis()));
    // H4: OSC 52 query surfaces a Query request that lifecycle.rs answers with the real clipboard
    let mut p = pane(80, 24);
    p.feed(b"\x1b]52;c;?\x07");
    let is_query = matches!(p.terminal.clipboard_request, Some(crate::terminal::ClipboardRequest::Query));
    s.check("osc52_query_is_gated_by_consent", !is_query, "terminal records ClipboardRequest::Query for any program; src/app/lifecycle.rs:~735 then replies `ESC ] 52 ; c ; <base64 of system clipboard>` with no config flag / prompt (code-read confirmed; app layer cannot be driven headlessly)");
    let mut p = pane(80, 24);
    p.feed(b"\x1b]52;c;aGVsbG8=\x07");
    let is_set = matches!(&p.terminal.clipboard_request, Some(crate::terminal::ClipboardRequest::Set(d)) if d == "aGVsbG8=");
    s.check("osc52_write_is_gated_by_consent", !is_set, "any output (cat of a hostile file, ssh remote) overwrites the system clipboard silently (lifecycle.rs copies decoded text unconditionally)");
    // title injection
    let mut p = pane(80, 24);
    p.feed(b"\x1b]2;evil\x1b[31m\ntitle\x07\x1b]0;a\x01b\x07");
    s.info("title_control_chars", format!("title={:?}", p.terminal.title));
    p.feed(b"\x1b]2;rm -rf ~\x07");
    s.info("title_report_csi21t", "CSI 21 t (report title) is not implemented -> no title-injection-into-stdin vector (REFUTED for that variant)");
    p.feed(b"\x1b[21t");
    s.check("no_title_report_response", p.terminal.response_queue.is_empty(), format!("{:?}", p.terminal.response_queue));
    // DA responses
    let mut p = pane(80, 24);
    p.feed(b"\x1b[c\x1b[>c\x1b[0c\x1b[=c");
    s.info("da_responses", format!("{:?}", p.terminal.response_queue.iter().map(|b| String::from_utf8_lossy(b).replace('\x1b', "\\e")).collect::<Vec<_>>()));
    // unsupported probes that modern apps send; no reply means apps time out
    let mut p = pane(80, 24);
    p.feed(b"\x1b[?2026$p\x1b[?u\x1b[>q\x1b[?1004$p");
    s.info("decrqm_kitty_kbd_xtversion_probes", format!("{} responses (DECRQM 2026, kitty keyboard query, XTVERSION): apps relying on replies fall back after a timeout", p.terminal.response_queue.len()));
    p.terminal.response_queue.clear();
    p.feed(b"\x1b[?2026h\x1b[>1u");
    let pending = p.terminal.sync_pending();
    let kitty = p.terminal.kitty_keyboard_flags();
    p.feed(b"\x1b[?2026$p");
    let rq = p.terminal.response_queue.iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect::<String>();
    s.check("sync_output_2026_supported", pending && kitty == 1 && rq.contains("2026;1$y"), format!("sync_pending={pending} kitty_flags={kitty} decrqm_reply={rq:?}"));
    s.finish();
}

// ---- child-process bombs (image protocol) ----

fn kitty(keys: &str, payload: &str) -> Vec<u8> {
    format!("\x1b_G{keys};{payload}\x1b\\").into_bytes()
}

fn b64(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        o.push(A[(n >> 18) as usize & 63] as char);
        o.push(A[(n >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        o.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    o
}

fn zip_bomb_png(w: u32, h: u32, gigabytes: usize) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data { c ^= b as u32; for _ in 0..8 { c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 }; } }
        !c
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    let zeros = vec![0u8; 1 << 20];
    for _ in 0..(gigabytes << 10) { z.write_all(&zeros).unwrap(); }
    let idat = z.finish().unwrap();
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut push = |ty: &[u8; 4], d: &[u8]| {
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        let mut body = ty.to_vec(); body.extend_from_slice(d);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes()); ihdr.extend_from_slice(&h.to_be_bytes()); ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    push(b"IHDR", &ihdr);
    push(b"IDAT", &idat);
    push(b"IEND", &[]);
    out
}

#[test]
fn child_scenario() {
    let Ok(name) = std::env::var("AUDIT_CHILD") else { return };
    let mut p = pane(80, 24);
    match name.as_str() {
        "raw_rgba_65535" => p.feed(&kitty("a=T,f=32,s=65535,v=65535", "AAAA")),
        "raw_rgba_100000" => p.feed(&kitty("a=T,f=32,s=100000,v=100000", "AAAA")),
        "many_images_20x4000" => {
            for i in 1..=20 { p.feed(&kitty(&format!("a=T,f=32,s=4000,v=4000,i={i}"), "AAAA")); }
        }
        "chunk_accum_400MB" => {
            let chunk = "A".repeat(10 << 20);
            for _ in 0..40 { p.feed(&kitty("a=t,f=32,s=10,v=10,m=1", &chunk)); }
        }
        "png_zipbomb_1GB" => {
            let png = zip_bomb_png(8000, 8000, 1);
            let enc = b64(&png);
            println!("png bytes {} b64 {}", png.len(), enc.len());
            p.feed(&kitty("a=T,f=100", &enc));
        }
        _ => {}
    }
    let imgs = p.terminal.image_store.has_placements();
    println!("CHILD_DONE placements={imgs}");
}

fn run_child(name: &str, cap_mb: u64) -> (String, Option<u64>, bool, f64) {
    let exe = std::env::current_exe().unwrap();
    let mut child = std::process::Command::new("/usr/bin/time")
        .arg("-l").arg(exe).args(["audit::escapes::child_scenario", "--exact", "--nocapture", "--test-threads=1"])
        .env("AUDIT_CHILD", name)
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let t0 = Instant::now();
    let mut killed = false;
    let mut peak = 0u64;
    loop {
        if let Ok(Some(_)) = child.try_wait() { break; }
        // child of /usr/bin/time: find the real pid via pgrep -P
        let out = std::process::Command::new("pgrep").args(["-P", &child.id().to_string()]).output().unwrap();
        for pid in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            let r = std::process::Command::new("ps").args(["-o", "rss=", "-p", pid]).output().unwrap();
            let kb: u64 = String::from_utf8_lossy(&r.stdout).trim().parse().unwrap_or(0);
            peak = peak.max(kb / 1024);
            if kb / 1024 > cap_mb { let _ = std::process::Command::new("kill").args(["-9", pid]).output(); killed = true; }
        }
        if t0.elapsed() > Duration::from_secs(90) { let _ = child.kill(); killed = true; }
        std::thread::sleep(Duration::from_millis(100));
    }
    let o = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    let maxrss = err.lines().find(|l| l.contains("maximum resident set size")).and_then(|l| l.split_whitespace().next()).and_then(|n| n.parse::<u64>().ok()).map(|b| b / (1 << 20));
    let ok_done = stdout.contains("CHILD_DONE");
    let sig = o.status.code().map_or(format!("signal/abort ({:?})", o.status), |c| format!("exit {c}"));
    (format!("{sig}; done_marker={ok_done}; peak sampled {peak} MB; stderr_tail={:?}", err.lines().filter(|l| !l.contains("  ")).last().unwrap_or("").chars().take(120).collect::<String>()), maxrss.or(Some(peak)), killed, t0.elapsed().as_secs_f64())
}

#[test]
fn image_protocol_bombs() {
    if std::env::var("AUDIT_CHILD").is_ok() { return; }
    let mut s = Soft::new("escapes");
    for (name, cap) in [("raw_rgba_65535", 4000u64), ("raw_rgba_100000", 4000), ("many_images_20x4000", 4000), ("chunk_accum_400MB", 4000), ("png_zipbomb_1GB", 4000)] {
        let (desc, peak, killed, secs) = run_child(name, cap);
        let ok = !killed && peak.unwrap_or(0) < 1000 && desc.contains("exit 0") && desc.contains("done_marker=true");
        s.check(&format!("image_{name}"), ok, format!("{desc}; max RSS {:?} MB; {secs:.1}s; killed_by_watchdog(>{cap}MB)={killed}", peak));
    }
    s.finish();
}
