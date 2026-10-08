use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;

use portable_pty::{native_pty_system, MasterPty, PtySize};
use winit::event_loop::EventLoopProxy;

pub struct Pty {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl Pty {
    pub fn spawn(cols: u16, rows: u16, proxy: EventLoopProxy<()>) -> Self {
        Self::spawn_in(cols, rows, proxy, None)
    }

    /// Like [`Pty::spawn`], starting the shell in `cwd` when it is an existing directory.
    pub fn spawn_in(cols: u16, rows: u16, proxy: EventLoopProxy<()>, cwd: Option<&str>) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .expect("Failed to open PTY");

        // Launches the user's shell with OSC 133/OSC 7 shell integration
        // injected (see shell_integration; opt out with RIFT_NO_SHELL_INTEGRATION).
        let mut cmd = crate::shell_integration::build_shell_command();
        if let Some(dir) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
            cmd.cwd(dir);
        }
        let _child = pair.slave.spawn_command(cmd).expect("Failed to spawn shell");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("Failed to clone PTY reader");
        let writer = pair.master.take_writer().expect("Failed to take PTY writer");

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() { break; }
                        let _ = proxy.send_event(());
                    }
                }
            }
        });

        Self { writer, master: pair.master, rx }
    }

    pub fn try_read(&self) -> Option<Vec<u8>> {
        self.rx.try_recv().ok()
    }

    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    pub fn resize(&self, cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) {
        let _ = self.master.resize(PtySize { rows, cols, pixel_width, pixel_height });
    }
}
