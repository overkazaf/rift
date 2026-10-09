//! Local PTY: shell process, bounded output channel, child reaping.
//!
//! Output path: a reader thread pulls up to 64 KB at a time from the PTY master
//! and pushes it into a *bounded* channel ([`CHANNEL_CHUNKS`] chunks). When the UI
//! thread falls behind the channel fills, the reader blocks, the kernel PTY
//! buffer fills and the child is throttled by its own `write()` blocking - the
//! correct flow control for `yes` / `cat big.log`. Memory is therefore capped at
//! roughly `CHANNEL_CHUNKS * READ_CHUNK` (4 MB) per pane regardless of UI stalls.
//!
//! Lifecycle: a reaper thread owns the `Child` and `wait()`s on it, so exited
//! shells never linger as zombies; dropping the [`Pty`] sends SIGHUP to the
//! shell's process group and escalates to SIGKILL if it ignores it.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use winit::event_loop::EventLoopProxy;

/// Bytes requested per `read()` from the PTY master.
pub const READ_CHUNK: usize = 64 * 1024;
/// Channel capacity in chunks (64 x 64 KB = 4 MB worst case in flight).
pub const CHANNEL_CHUNKS: usize = 64;
/// Grace period between SIGHUP and SIGKILL when a pane is closed.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// Delay between the child exiting and the exit being published, so the reader
/// thread can deliver the last output first ("[process exited]" comes after it).
const EXIT_PUBLISH_DELAY: Duration = Duration::from_millis(120);

/// Called from background threads when new data / state is available.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

const RUNNING: i64 = i64::MIN;

struct ChildState {
    /// `RUNNING`, or the exit code (published after `EXIT_PUBLISH_DELAY`).
    code: AtomicI64,
    /// Set the instant `wait()` returns (the child is reaped; its pid is free).
    reaped: AtomicBool,
}

pub struct Pty {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    rx: mpsc::Receiver<Vec<u8>>,
    /// True while a wake-up for pending data is already in flight; keeps the
    /// reader from posting one event-loop event per chunk during floods.
    wake_pending: Arc<AtomicBool>,
    pid: Option<u32>,
    state: Arc<ChildState>,
}

impl Pty {
    pub fn spawn(cols: u16, rows: u16, proxy: EventLoopProxy<()>) -> Self {
        Self::spawn_in(cols, rows, proxy, None)
    }

    /// Like [`Pty::spawn`], starting the shell in `cwd` when it is an existing directory.
    pub fn spawn_in(cols: u16, rows: u16, proxy: EventLoopProxy<()>, cwd: Option<&str>) -> Self {
        // Launches the user's shell with OSC 133/OSC 7 shell integration
        // injected (see shell_integration; opt out with RIFT_NO_SHELL_INTEGRATION).
        let mut cmd = crate::shell_integration::build_shell_command();
        if let Some(dir) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
            cmd.cwd(dir);
        }
        let waker: Waker = Arc::new(move || {
            let _ = proxy.send_event(());
        });
        Self::spawn_cmd(cols, rows, cmd, waker).expect("Failed to spawn shell")
    }

    /// Spawn `cmd` on a fresh PTY. `waker` is invoked from the reader / reaper
    /// threads when output arrives (coalesced) or the child exits.
    pub fn spawn_cmd(cols: u16, rows: u16, cmd: CommandBuilder, waker: Waker) -> Result<Self, String> {
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("openpty: {e}"))?;
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| format!("spawn: {e}"))?;
        let pid = child.process_id();
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| format!("clone reader: {e}"))?;
        let writer = pair.master.take_writer().map_err(|e| format!("take writer: {e}"))?;

        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(CHANNEL_CHUNKS);
        let wake_pending = Arc::new(AtomicBool::new(false));
        {
            let wake_pending = wake_pending.clone();
            let waker = waker.clone();
            thread::Builder::new()
                .name("pty-reader".into())
                .spawn(move || {
                    let mut buf = vec![0u8; READ_CHUNK];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                // Blocks while the channel is full (backpressure).
                                if tx.send(buf[..n].to_vec()).is_err() {
                                    return;
                                }
                                if !wake_pending.swap(true, Ordering::AcqRel) {
                                    waker();
                                }
                            }
                        }
                    }
                    // EOF: let the UI notice the channel disconnect.
                    drop(tx);
                    waker();
                })
                .map_err(|e| format!("reader thread: {e}"))?;
        }

        let state = Arc::new(ChildState { code: AtomicI64::new(RUNNING), reaped: AtomicBool::new(false) });
        {
            let state = state.clone();
            thread::Builder::new()
                .name("pty-reaper".into())
                .spawn(move || {
                    let code = match child.wait() {
                        Ok(status) => status.exit_code() as i64,
                        Err(_) => 255,
                    };
                    state.reaped.store(true, Ordering::Release);
                    thread::sleep(EXIT_PUBLISH_DELAY);
                    state.code.store(code, Ordering::Release);
                    waker();
                })
                .map_err(|e| format!("reaper thread: {e}"))?;
        }

        Ok(Self { writer, master: pair.master, rx, wake_pending, pid, state })
    }

    #[allow(dead_code)]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Exit code once the child has exited and its output had time to drain.
    pub fn exit_status(&self) -> Option<u32> {
        match self.state.code.load(Ordering::Acquire) {
            RUNNING => None,
            c => Some(c as u32),
        }
    }

    /// Has the child been reaped (no zombie, pid released)?
    pub fn is_reaped(&self) -> bool {
        self.state.reaped.load(Ordering::Acquire)
    }

    /// Next pending output chunk, if any.
    pub fn try_read(&self) -> Option<Vec<u8>> {
        match self.rx.try_recv() {
            Ok(v) => Some(v),
            Err(mpsc::TryRecvError::Empty) => {
                // Re-arm the reader's wake-up *before* the final check so a chunk
                // pushed in between cannot be missed.
                self.wake_pending.store(false, Ordering::Release);
                self.rx.try_recv().ok()
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }

    /// Ask the shell to go away: SIGHUP to its process group now, SIGKILL after
    /// a grace period if it is still alive. Idempotent; also run from `Drop`.
    pub fn hangup(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid.filter(|p| *p > 1) {
            if self.is_reaped() {
                return;
            }
            // SAFETY: plain signal syscalls on a pid we spawned; failures (ESRCH) are ignored.
            // The shell is a session leader (portable-pty setsid), so pgid == pid.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGHUP);
            }
            let state = self.state.clone();
            let _ = thread::Builder::new().name("pty-killer".into()).spawn(move || {
                thread::sleep(KILL_GRACE);
                if !state.reaped.load(Ordering::Acquire) {
                    // SAFETY: as above; guarded by `reaped` so a recycled pid is not targeted.
                    unsafe {
                        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                        libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    }
                }
            });
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    pub fn resize(&self, cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) {
        let _ = self.master.resize(PtySize { rows, cols, pixel_width, pixel_height });
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.hangup();
    }
}
