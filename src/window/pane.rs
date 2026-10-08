use crate::pty::Pty;
use crate::tools::recording::Recorder;
use crate::network::ssh::SshPty;
use crate::terminal::{AnsiHandler, Terminal};
use winit::event_loop::EventLoopProxy;

pub enum PtyKind {
    Local(Pty),
    Ssh(SshPty),
}

impl PtyKind {
    pub fn try_read(&self) -> Option<Vec<u8>> {
        match self {
            PtyKind::Local(p) => p.try_read(),
            PtyKind::Ssh(s) => s.try_read(),
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        match self {
            PtyKind::Local(p) => p.write(data),
            PtyKind::Ssh(s) => s.write(data),
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        match self {
            PtyKind::Local(p) => p.resize(cols, rows, 0, 0),
            PtyKind::Ssh(s) => s.resize(cols, rows),
        }
    }
}

#[allow(dead_code)]
pub struct Pane {
    pub id: usize,
    pub terminal: Terminal,
    pub pty: PtyKind,
    pub vt_parser: vte::Parser,
    pub recorder: Option<Recorder>,
    pub label: Option<String>,
}

impl Pane {
    pub fn new(id: usize, cols: usize, rows: usize, proxy: EventLoopProxy<()>) -> Self {
        Self {
            id,
            terminal: Terminal::new(cols, rows),
            pty: PtyKind::Local(Pty::spawn(cols as u16, rows as u16, proxy)),
            vt_parser: vte::Parser::new(),
            recorder: None,
            label: None,
        }
    }

    /// Local pane whose shell starts in `cwd` (when it is an existing directory).
    pub fn new_in(id: usize, cols: usize, rows: usize, proxy: EventLoopProxy<()>, cwd: Option<&str>) -> Self {
        Self {
            id,
            terminal: Terminal::new(cols, rows),
            pty: PtyKind::Local(Pty::spawn_in(cols as u16, rows as u16, proxy, cwd)),
            vt_parser: vte::Parser::new(),
            recorder: None,
            label: None,
        }
    }

    pub fn from_ssh(id: usize, cols: usize, rows: usize, ssh: SshPty) -> Self {
        let label = ssh.config.display_name();
        Self {
            id,
            terminal: Terminal::new(cols, rows),
            pty: PtyKind::Ssh(ssh),
            vt_parser: vte::Parser::new(),
            recorder: None,
            label: Some(label),
        }
    }

    pub fn process_output(&mut self) -> bool {
        let mut changed = false;
        while let Some(data) = self.pty.try_read() {
            if let Some(rec) = &mut self.recorder {
                rec.record_output(&data);
            }
            let terminal = &mut self.terminal;
            let parser = &mut self.vt_parser;
            let mut handler = AnsiHandler::new(terminal);
            for &byte in &data {
                // Kitty graphics protocol data travels inside an APC string
                // (`ESC _ ... ST`), which `vte`'s Perform trait has no
                // callback for. Scan for it in parallel — see
                // AnsiHandler::feed_apc_byte for details.
                handler.feed_apc_byte(byte);
                parser.advance(&mut handler, byte);
            }
            changed = true;
        }
        changed
    }

    pub fn write(&mut self, data: &[u8]) {
        if let Some(rec) = &mut self.recorder {
            rec.record_input(data);
        }
        self.pty.write(data);
    }

    pub fn flush_responses(&mut self) {
        for response in self.terminal.response_queue.drain(..) {
            self.pty.write(&response);
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        if cols != self.terminal.cols || rows != self.terminal.rows {
            self.terminal.resize(cols, rows);
            self.pty.resize(cols as u16, rows as u16);
        }
    }

    pub fn title(&self) -> Option<&str> {
        if let Some(ref l) = self.label {
            return Some(l);
        }
        self.terminal.title.as_deref()
    }
}
