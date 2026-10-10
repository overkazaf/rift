//! Download, verify and install a release package.
//!
//! macOS: mount the `.dmg` read-only, `ditto` Rift.app next to the target,
//! clear the quarantine flag and swap it in atomically (`renamex_np` with
//! `RENAME_SWAP`). Linux: unpack the `rift` binary from the tarball and
//! `rename` it over the running executable.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::release::{select_asset, user_agent, Asset, Platform, Release};
use super::sha256::{parse_checksum_file, Sha256};

/// Where an upgrade goes, derived from the running executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Running from `<...>/X.app/Contents/MacOS/rift`: replace that bundle.
    MacBundle { app: PathBuf },
    /// macOS binary outside a bundle: install to /Applications/Rift.app.
    MacApplications { app: PathBuf },
    /// `cargo run` / `target/{debug,release}`: never overwrite a dev build.
    DevBuild { exe: PathBuf },
    /// Linux: replace this executable.
    LinuxExe { exe: PathBuf },
    Unsupported,
}

impl Target {
    pub fn describe(&self) -> String {
        match self {
            Target::MacBundle { app } => format!("replace {}", app.display()),
            Target::MacApplications { app } => format!("install {}", app.display()),
            Target::DevBuild { exe } => format!("development build at {} (not replaced)", exe.display()),
            Target::LinuxExe { exe } => format!("replace {}", exe.display()),
            Target::Unsupported => "unsupported platform".into(),
        }
    }
}

fn is_dev_build(exe: &Path) -> bool {
    let comps: Vec<String> = exe.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    comps.windows(2).any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
        || comps.windows(3).any(|w| w[0] == "target" && (w[2] == "debug" || w[2] == "release"))
}

/// Pure target detection (unit-tested).
pub fn detect_target(exe: &Path, platform: Platform) -> Target {
    if platform == Platform::Unsupported {
        return Target::Unsupported;
    }
    // `.../Name.app/Contents/MacOS/<bin>`
    if platform == Platform::MacOs {
        let macos = exe.parent();
        let contents = macos.and_then(Path::parent);
        let app = contents.and_then(Path::parent);
        if let (Some(m), Some(c), Some(a)) = (macos, contents, app) {
            if m.file_name().is_some_and(|n| n == "MacOS")
                && c.file_name().is_some_and(|n| n == "Contents")
                && a.extension().is_some_and(|e| e == "app")
            {
                return Target::MacBundle { app: a.to_path_buf() };
            }
        }
    }
    if is_dev_build(exe) {
        return Target::DevBuild { exe: exe.to_path_buf() };
    }
    match platform {
        Platform::MacOs => Target::MacApplications { app: PathBuf::from("/Applications/Rift.app") },
        Platform::LinuxX86_64 => Target::LinuxExe { exe: exe.to_path_buf() },
        Platform::Unsupported => Target::Unsupported,
    }
}

pub fn current_target() -> Target {
    match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(exe) => detect_target(&exe, Platform::current()),
        Err(_) => Target::Unsupported,
    }
}

/// Progress callback: (stage message) or (downloaded, total) bytes.
pub enum Progress<'a> {
    Stage(&'a str),
    Bytes(u64, u64),
}

/// Fresh private working directory under the system temp dir.
pub fn work_dir() -> std::io::Result<PathBuf> {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("rift-upgrade-{}-{ts}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

fn agent_get(url: &str, timeout: Duration) -> Result<ureq::http::Response<ureq::Body>, String> {
    let resp = ureq::get(url)
        .header("User-Agent", &user_agent())
        .header("Accept", "application/octet-stream")
        .config()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(timeout))
        .build()
        .call()
        .map_err(|e| format!("download failed (offline?): {e}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("download failed: HTTP {status} for {url}"));
    }
    Ok(resp)
}

/// Fetch the expected digest from the `.sha256` asset.
pub fn fetch_checksum(sum: &Asset, pkg_name: &str) -> Result<String, String> {
    let mut resp = agent_get(&sum.url, Duration::from_secs(30))?;
    let text = resp.body_mut().with_config().limit(64 * 1024).read_to_string().map_err(|e| format!("reading checksum: {e}"))?;
    parse_checksum_file(&text, pkg_name).ok_or_else(|| format!("{} does not contain a SHA-256 for {pkg_name}", sum.name))
}

/// Stream `asset` into `dir`, hashing as we go. Returns (path, sha256 hex).
pub fn download(asset: &Asset, dir: &Path, progress: &mut dyn FnMut(Progress)) -> Result<(PathBuf, String), String> {
    let mut resp = agent_get(&asset.url, Duration::from_secs(30 * 60))?;
    let total = resp.body().content_length().unwrap_or(asset.size);
    let path = dir.join(&asset.name);
    let mut file = std::fs::File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut reader = resp.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| format!("write {}: {e}", path.display()))?;
        got += n as u64;
        progress(Progress::Bytes(got, total));
    }
    file.sync_all().ok();
    if total > 0 && got != total {
        return Err(format!("download truncated: got {got} of {total} bytes"));
    }
    Ok((path, hasher.finish_hex()))
}

/// Compare digests; error text explains a mismatch.
pub fn verify(actual: &str, expected: &str, name: &str) -> Result<(), String> {
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!("checksum mismatch for {name}: expected {expected}, got {actual}. Refusing to install."))
    }
}

/// Turn an io error into advice (permissions are the common failure).
pub fn explain_io(action: &str, path: &Path, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(libc::EACCES) || e.raw_os_error() == Some(libc::EPERM) {
        format!(
            "{action} {}: permission denied. Re-run with sufficient rights (e.g. `sudo rift upgrade`), \
             or move Rift somewhere you own and upgrade from there.",
            path.display()
        )
    } else {
        format!("{action} {}: {e}", path.display())
    }
}

// ── tar.gz (Linux) ──────────────────────────────────────────────────────

/// Extract the regular file whose basename is `want` from a `.tar.gz`.
pub fn extract_from_tar_gz(archive: &Path, want: &str, dest: &Path) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| format!("open {}: {e}", archive.display()))?;
    let mut gz = flate2::read::GzDecoder::new(std::io::BufReader::new(f));
    let mut header = [0u8; 512];
    let mut long_name: Option<String> = None;
    loop {
        if read_full(&mut gz, &mut header)? == 0 || header.iter().all(|b| *b == 0) {
            return Err(format!("`{want}` not found in {}", archive.display()));
        }
        let field = |a: usize, b: usize| {
            let s = &header[a..b];
            let end = s.iter().position(|c| *c == 0).unwrap_or(s.len());
            String::from_utf8_lossy(&s[..end]).into_owned()
        };
        let size = u64::from_str_radix(field(124, 136).trim().trim_matches(char::from(0)), 8).map_err(|_| "corrupt tar header".to_string())?;
        let kind = header[156];
        let mut name = field(0, 100);
        if &header[257..262] == b"ustar" {
            let prefix = field(345, 500);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        if let Some(n) = long_name.take() {
            name = n;
        }
        let padded = size.div_ceil(512) * 512;
        match kind {
            b'L' => {
                // GNU long name: the data block holds the next entry's name.
                let mut data = vec![0u8; padded as usize];
                read_full(&mut gz, &mut data)?;
                let end = data[..size as usize].iter().position(|c| *c == 0).unwrap_or(size as usize);
                long_name = Some(String::from_utf8_lossy(&data[..end]).into_owned());
                continue;
            }
            b'0' | 0 if Path::new(&name).file_name().is_some_and(|n| n == want) => {
                let mut out = std::fs::File::create(dest).map_err(|e| explain_io("create", dest, &e))?;
                let mut left = size;
                let mut buf = vec![0u8; 64 * 1024];
                while left > 0 {
                    let n = (left as usize).min(buf.len());
                    read_exact(&mut gz, &mut buf[..n])?;
                    out.write_all(&buf[..n]).map_err(|e| explain_io("write", dest, &e))?;
                    left -= n as u64;
                }
                out.sync_all().ok();
                return Ok(());
            }
            _ => skip(&mut gz, padded)?,
        }
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<usize, String> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) => return Err(format!("corrupt archive: {e}")),
        }
    }
    if got != 0 && got != buf.len() {
        return Err("corrupt archive: truncated".into());
    }
    Ok(got)
}

fn read_exact(r: &mut impl Read, buf: &mut [u8]) -> Result<(), String> {
    r.read_exact(buf).map_err(|e| format!("corrupt archive: {e}"))
}

fn skip(r: &mut impl Read, n: u64) -> Result<(), String> {
    std::io::copy(&mut r.take(n), &mut std::io::sink()).map_err(|e| format!("corrupt archive: {e}"))?;
    Ok(())
}

/// Atomically replace `exe` with `new_bin` (same directory rename).
pub fn replace_executable(new_bin: &Path, exe: &Path) -> Result<(), String> {
    let dir = exe.parent().ok_or("executable has no parent directory")?;
    let staged = dir.join(format!(".rift.upgrade-{}", std::process::id()));
    std::fs::copy(new_bin, &staged).map_err(|e| explain_io("write", &staged, &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(|e| explain_io("chmod", &staged, &e))?;
    }
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::remove_file(&staged);
        return Err(explain_io("replace", exe, &e));
    }
    Ok(())
}

// ── macOS ───────────────────────────────────────────────────────────────

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{what} failed: {}", err.trim()))
    }
}

/// `hdiutil attach` guard: detaches on drop.
struct Mount(PathBuf);

impl Drop for Mount {
    fn drop(&mut self) {
        let ok = Command::new("hdiutil").arg("detach").arg(&self.0).arg("-quiet").status().is_ok_and(|s| s.success());
        if !ok {
            let _ = Command::new("hdiutil").arg("detach").arg(&self.0).arg("-force").arg("-quiet").status();
        }
    }
}

/// First `*.app` bundle in `dir` that contains `Contents/MacOS/rift`
/// (preferring `Rift.app`).
pub fn find_app_bundle(dir: &Path) -> Option<PathBuf> {
    let preferred = dir.join("Rift.app");
    if preferred.join("Contents/MacOS/rift").is_file() {
        return Some(preferred);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "app") && p.join("Contents/MacOS/rift").is_file())
}

#[cfg(target_os = "macos")]
fn swap_dirs(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes())?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths.
    let rc = unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) };
    if rc == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

#[cfg(not(target_os = "macos"))]
fn swap_dirs(_a: &Path, _b: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "swap"))
}

/// Install `Rift.app` from `dmg` at `target_app` (replacing an existing one).
pub fn install_dmg(dmg: &Path, target_app: &Path, work: &Path, progress: &mut dyn FnMut(Progress)) -> Result<(), String> {
    progress(Progress::Stage("Mounting disk image"));
    let mnt = work.join("mnt");
    std::fs::create_dir_all(&mnt).map_err(|e| format!("create {}: {e}", mnt.display()))?;
    run(
        Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-quiet", "-mountpoint"]).arg(&mnt).arg(dmg),
        "hdiutil attach",
    )?;
    let mount = Mount(mnt.clone());
    let src = find_app_bundle(&mount.0).ok_or("no Rift.app found in the disk image")?;
    install_app_bundle(&src, target_app, progress)
}

/// Copy `src` (an .app) next to `target_app` and swap it in.
pub fn install_app_bundle(src: &Path, target_app: &Path, progress: &mut dyn FnMut(Progress)) -> Result<(), String> {
    let parent = target_app.parent().ok_or("target has no parent directory")?;
    let stem = target_app.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Rift.app".into());
    let staging = parent.join(format!(".{stem}.upgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    progress(Progress::Stage("Copying Rift.app"));
    // Probe write access first for a clear message (ditto's is terse).
    if let Err(e) = std::fs::create_dir(&staging) {
        return Err(explain_io("write to", parent, &e));
    }
    let _ = std::fs::remove_dir(&staging);
    if let Err(e) = run(Command::new("ditto").arg(src).arg(&staging), "ditto") {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    // Unsigned app: drop the quarantine flag so Gatekeeper doesn't block it.
    let _ = Command::new("xattr").args(["-dr", "com.apple.quarantine"]).arg(&staging).status();
    progress(Progress::Stage("Replacing app bundle"));
    let res = if target_app.exists() {
        match swap_dirs(&staging, target_app) {
            Ok(()) => Ok(()), // staging now holds the old app
            Err(_) => {
                // Fallback: two renames (brief window without an app).
                let old = parent.join(format!(".{stem}.old-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&old);
                std::fs::rename(target_app, &old)
                    .map_err(|e| explain_io("move aside", target_app, &e))
                    .and_then(|_| {
                        std::fs::rename(&staging, target_app).map_err(|e| {
                            let _ = std::fs::rename(&old, target_app);
                            explain_io("install", target_app, &e)
                        })
                    })
                    .map(|_| {
                        let _ = std::fs::remove_dir_all(&old);
                    })
            }
        }
    } else {
        std::fs::rename(&staging, target_app).map_err(|e| explain_io("install", target_app, &e))
    };
    let _ = std::fs::remove_dir_all(&staging);
    res
}

// ── The whole pipeline ──────────────────────────────────────────────────

#[derive(Debug)]
pub struct Outcome {
    pub version: String,
    /// `Some` when an installation was replaced/created (relaunch it).
    pub installed: Option<PathBuf>,
    /// Verified package left on disk (dev builds: nothing is replaced).
    pub artifact: Option<PathBuf>,
}

pub struct Options {
    /// Allow installing when the release has no `.sha256` (never on mismatch).
    pub no_verify: bool,
}

/// Download, verify and install `release` at `target`. Blocking.
pub fn upgrade(release: &Release, target: &Target, opts: &Options, progress: &mut dyn FnMut(Progress)) -> Result<Outcome, String> {
    let platform = Platform::current();
    if *target == Target::Unsupported || platform == Platform::Unsupported {
        return Err(format!("self-upgrade is not supported on this platform; download from {}", super::release::RELEASES_PAGE));
    }
    let (pkg, sum) = select_asset(release, platform)
        .ok_or_else(|| format!("release {} has no package for this platform", release.tag))?;
    let expected = match sum {
        Some(s) => {
            progress(Progress::Stage("Fetching checksum"));
            Some(fetch_checksum(s, &pkg.name)?)
        }
        None if opts.no_verify => None,
        None => {
            return Err(format!(
                "release {} has no {}.sha256 checksum; refusing to install an unverified download (use --no-verify to override)",
                release.tag, pkg.name
            ))
        }
    };
    let work = work_dir().map_err(|e| format!("temp dir: {e}"))?;
    progress(Progress::Stage("Downloading"));
    let (file, actual) = match download(pkg, &work, progress) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&work);
            return Err(e);
        }
    };
    match &expected {
        Some(exp) => {
            if let Err(e) = verify(&actual, exp, &pkg.name) {
                let _ = std::fs::remove_dir_all(&work);
                return Err(e);
            }
            progress(Progress::Stage("Checksum verified (SHA-256)"));
        }
        None => progress(Progress::Stage("WARNING: no checksum published; installing unverified (--no-verify)")),
    }
    let version = release.version_str();
    let res = match target {
        Target::MacBundle { app } | Target::MacApplications { app } => {
            install_dmg(&file, app, &work, progress).map(|_| Outcome { version: version.clone(), installed: Some(app.clone()), artifact: None })
        }
        Target::LinuxExe { exe } => {
            progress(Progress::Stage("Unpacking"));
            let bin = work.join("rift.new");
            extract_from_tar_gz(&file, "rift", &bin)
                .and_then(|_| {
                    progress(Progress::Stage("Replacing executable"));
                    replace_executable(&bin, exe)
                })
                .map(|_| Outcome { version: version.clone(), installed: Some(exe.clone()), artifact: None })
        }
        Target::DevBuild { .. } => {
            // Keep the verified package; the caller tells the user where it is.
            return Ok(Outcome { version, installed: None, artifact: Some(file) });
        }
        Target::Unsupported => unreachable!(),
    };
    let _ = std::fs::remove_dir_all(&work);
    res
}

/// Start the freshly installed Rift once this process has exited.
pub fn relaunch(path: &Path) -> std::io::Result<()> {
    let script = if cfg!(target_os = "macos") { "sleep 1; exec open -n \"$0\"" } else { "sleep 1; exec \"$0\" </dev/null >/dev/null 2>&1" };
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Human-readable byte count.
pub fn fmt_bytes(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_detection() {
        let mac = |p: &str| detect_target(Path::new(p), Platform::MacOs);
        assert_eq!(mac("/Applications/Rift.app/Contents/MacOS/rift"), Target::MacBundle { app: "/Applications/Rift.app".into() });
        assert_eq!(mac("/Users/me/Apps/Rift Beta.app/Contents/MacOS/rift"), Target::MacBundle { app: "/Users/me/Apps/Rift Beta.app".into() });
        assert_eq!(mac("/src/rift/target/Rift.app/Contents/MacOS/rift"), Target::MacBundle { app: "/src/rift/target/Rift.app".into() }, "bundle wins");
        assert_eq!(mac("/src/rift/target/release/rift"), Target::DevBuild { exe: "/src/rift/target/release/rift".into() });
        assert_eq!(mac("/src/rift/target/aarch64-apple-darwin/debug/rift"), Target::DevBuild { exe: "/src/rift/target/aarch64-apple-darwin/debug/rift".into() });
        assert_eq!(mac("/usr/local/bin/rift"), Target::MacApplications { app: "/Applications/Rift.app".into() });
        let lin = |p: &str| detect_target(Path::new(p), Platform::LinuxX86_64);
        assert_eq!(lin("/usr/local/bin/rift"), Target::LinuxExe { exe: "/usr/local/bin/rift".into() });
        assert_eq!(lin("/home/me/rift/target/debug/rift"), Target::DevBuild { exe: "/home/me/rift/target/debug/rift".into() });
        assert_eq!(lin("/opt/X.app/Contents/MacOS/rift"), Target::LinuxExe { exe: "/opt/X.app/Contents/MacOS/rift".into() }, "no bundles on Linux");
        assert_eq!(detect_target(Path::new("/x/rift"), Platform::Unsupported), Target::Unsupported);
    }

    #[test]
    fn verify_refuses_mismatch() {
        let d = super::super::sha256::digest_hex(b"payload");
        assert!(verify(&d, &d.to_ascii_uppercase(), "f").is_ok());
        let e = verify(&d, &"0".repeat(64), "pkg.dmg").unwrap_err();
        assert!(e.contains("mismatch") && e.contains("pkg.dmg") && e.contains("Refusing"), "{e}");
    }

    /// Minimal ustar writer for tests.
    fn tar_entry(out: &mut Vec<u8>, name: &str, data: &[u8], kind: u8) {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000755");
        h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| *b as u32).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.extend(std::iter::repeat(0).take((512 - data.len() % 512) % 512));
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rift-update-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn extracts_binary_from_release_tarball() {
        use std::io::Write as _;
        let dir = scratch("tar");
        let mut tar = Vec::new();
        tar_entry(&mut tar, "rift-0.4.1-linux-x86_64/", b"", b'5');
        tar_entry(&mut tar, "rift-0.4.1-linux-x86_64/README.md", &vec![b'r'; 700], b'0');
        tar_entry(&mut tar, "rift-0.4.1-linux-x86_64/rift", b"\x7fELF-binary", b'0');
        tar.extend_from_slice(&[0u8; 1024]);
        let gz_path = dir.join("pkg.tar.gz");
        let mut enc = flate2::write::GzEncoder::new(std::fs::File::create(&gz_path).unwrap(), flate2::Compression::fast());
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap();
        let out = dir.join("rift.new");
        extract_from_tar_gz(&gz_path, "rift", &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"\x7fELF-binary");
        let e = extract_from_tar_gz(&gz_path, "missing", &dir.join("x")).unwrap_err();
        assert!(e.contains("not found"), "{e}");

        // Atomic replace of a fake "installed" binary in the scratch dir.
        let exe = dir.join("installed-rift");
        std::fs::write(&exe, b"old").unwrap();
        replace_executable(&out, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"\x7fELF-binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777, 0o755);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn app_bundle_swap_in_scratch_dir() {
        let dir = scratch("app");
        let mk = |root: &Path, body: &[u8]| {
            std::fs::create_dir_all(root.join("Contents/MacOS")).unwrap();
            std::fs::write(root.join("Contents/MacOS/rift"), body).unwrap();
        };
        let src_parent = dir.join("mnt");
        mk(&src_parent.join("Rift.app"), b"new");
        assert_eq!(find_app_bundle(&src_parent), Some(src_parent.join("Rift.app")));
        let target = dir.join("Apps/Rift.app");
        mk(&target, b"old");
        std::fs::write(target.join("Contents/stale"), b"x").unwrap();
        install_app_bundle(&src_parent.join("Rift.app"), &target, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(target.join("Contents/MacOS/rift")).unwrap(), b"new");
        assert!(!target.join("Contents/stale").exists(), "old bundle fully replaced");
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("Apps")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "staging cleaned up: {leftovers:?}");
        // Fresh install (no existing target).
        let fresh = dir.join("Apps2/Rift.app");
        std::fs::create_dir_all(fresh.parent().unwrap()).unwrap();
        install_app_bundle(&src_parent.join("Rift.app"), &fresh, &mut |_| {}).unwrap();
        assert!(fresh.join("Contents/MacOS/rift").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bytes_format() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2 KB");
        assert_eq!(fmt_bytes(31_457_280), "30.0 MB");
    }
}
