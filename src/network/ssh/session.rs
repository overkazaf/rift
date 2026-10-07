use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;

use winit::event_loop::EventLoopProxy;

#[derive(Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
}

impl SshConfig {
    pub fn display_name(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub enum AuthMethod {
    Password(String),
    KeyFile(PathBuf),
    Agent,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum SessionStatus {
    Connected,
    Disconnected,
    Error(String),
}

#[derive(Debug)]
pub enum SshError {
    Connection(String),
    Auth(String),
    Channel(String),
    Io(std::io::Error),
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SshError::Connection(msg) => write!(f, "connection: {msg}"),
            SshError::Auth(msg) => write!(f, "auth: {msg}"),
            SshError::Channel(msg) => write!(f, "channel: {msg}"),
            SshError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl From<std::io::Error> for SshError {
    fn from(e: std::io::Error) -> Self {
        SshError::Io(e)
    }
}

/// An SSH session that mirrors the local `Pty` interface.
#[allow(dead_code)]
pub struct SshPty {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<SshCommand>,
    data_rx: mpsc::Receiver<Vec<u8>>,
    pub status: SessionStatus,
    pub config: SshConfig,
    _thread: thread::JoinHandle<()>,
}

enum SshCommand {
    Write(Vec<u8>),
    Resize(u32, u32),
    Close,
}

impl SshPty {
    pub fn connect(
        config: SshConfig,
        cols: u16,
        rows: u16,
        proxy: EventLoopProxy<()>,
    ) -> Result<Self, SshError> {
        let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<SshCommand>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), SshError>>(1);

        let cfg = config.clone();
        let handle = thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(ssh_main(cfg, cols, rows, data_tx, cmd_rx, proxy, ready_tx));
        });

        match ready_rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(Ok(())) => Ok(Self {
                cmd_tx,
                data_rx,
                status: SessionStatus::Connected,
                config,
                _thread: handle,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(SshError::Connection("connection timeout".into())),
        }
    }

    pub fn try_read(&self) -> Option<Vec<u8>> {
        self.data_rx.try_recv().ok()
    }

    pub fn write(&mut self, data: &[u8]) {
        let _ = self.cmd_tx.send(SshCommand::Write(data.to_vec()));
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.cmd_tx.send(SshCommand::Resize(cols as u32, rows as u32));
    }
}

impl Drop for SshPty {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(SshCommand::Close);
    }
}

// ── Async internals ──

struct Handler;

impl russh::client::Handler for Handler {
    type Error = russh::Error;

    fn check_server_key(
        &mut self,
        _key: &russh::keys::PublicKeyOrCertificate,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        std::future::ready(Ok(true))
    }
}

async fn ssh_main(
    config: SshConfig,
    cols: u16,
    rows: u16,
    data_tx: mpsc::Sender<Vec<u8>>,
    cmd_rx: tokio::sync::mpsc::UnboundedReceiver<SshCommand>,
    proxy: EventLoopProxy<()>,
    ready_tx: mpsc::SyncSender<Result<(), SshError>>,
) {
    if let Err(e) = ssh_run(config, cols, rows, data_tx, cmd_rx, proxy, &ready_tx).await {
        log::error!("SSH: {e}");
        let _ = ready_tx.try_send(Err(e));
    }
}

async fn ssh_run(
    config: SshConfig,
    cols: u16,
    rows: u16,
    data_tx: mpsc::Sender<Vec<u8>>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<SshCommand>,
    proxy: EventLoopProxy<()>,
    ready_tx: &mpsc::SyncSender<Result<(), SshError>>,
) -> Result<(), SshError> {
    let ssh_cfg = Arc::new(russh::client::Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(120)),
        keepalive_interval: Some(std::time::Duration::from_secs(15)),
        ..Default::default()
    });

    log::info!("SSH: connecting to {}", config.display_name());
    let mut handle = russh::client::connect(ssh_cfg, (&*config.host, config.port), Handler)
        .await
        .map_err(|e| SshError::Connection(e.to_string()))?;

    authenticate(&mut handle, &config).await?;
    log::info!("SSH: authenticated as {}", config.user);

    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| SshError::Channel(e.to_string()))?;
    channel
        .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await
        .map_err(|e| SshError::Channel(e.to_string()))?;
    channel
        .request_shell(false)
        .await
        .map_err(|e| SshError::Channel(e.to_string()))?;

    log::info!("SSH: shell opened ({}x{})", cols, rows);
    let _ = ready_tx.send(Ok(()));

    let (mut reader, writer) = channel.split();

    let write_loop = async {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                SshCommand::Write(data) => {
                    if writer.data_bytes(bytes::Bytes::from(data)).await.is_err() {
                        break;
                    }
                }
                SshCommand::Resize(c, r) => {
                    let _ = writer.window_change(c, r, 0, 0).await;
                }
                SshCommand::Close => break,
            }
        }
    };

    let read_loop = async {
        loop {
            match reader.wait().await {
                Some(russh::ChannelMsg::Data { data }) => {
                    if data_tx.send(data.to_vec()).is_err() { break; }
                    let _ = proxy.send_event(());
                }
                Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                    if data_tx.send(data.to_vec()).is_err() { break; }
                    let _ = proxy.send_event(());
                }
                Some(russh::ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }
    };

    tokio::select! {
        _ = write_loop => {}
        _ = read_loop => {}
    }

    let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "").await;
    log::info!("SSH: disconnected");
    Ok(())
}

async fn authenticate(
    handle: &mut russh::client::Handle<Handler>,
    config: &SshConfig,
) -> Result<(), SshError> {
    match &config.auth {
        AuthMethod::KeyFile(path) => auth_with_key(handle, &config.user, path).await,
        AuthMethod::Password(pw) => {
            let r = handle
                .authenticate_password(&config.user, pw)
                .await
                .map_err(|e| SshError::Auth(e.to_string()))?;
            if r.success() { Ok(()) } else { Err(SshError::Auth("password rejected".into())) }
        }
        AuthMethod::Agent => {
            for path in default_key_paths() {
                if !path.exists() { continue; }
                if auth_with_key(handle, &config.user, &path).await.is_ok() {
                    return Ok(());
                }
            }
            Err(SshError::Auth("no valid key in ~/.ssh/".into()))
        }
    }
}

async fn auth_with_key(
    handle: &mut russh::client::Handle<Handler>,
    user: &str,
    path: &PathBuf,
) -> Result<(), SshError> {
    let key = russh::keys::load_secret_key(path, None)
        .map_err(|e| SshError::Auth(format!("load {}: {e}", path.display())))?;
    let hash = handle
        .best_supported_rsa_hash()
        .await
        .map_err(|e| SshError::Auth(e.to_string()))?
        .flatten();
    let r = handle
        .authenticate_publickey(user, russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await
        .map_err(|e| SshError::Auth(e.to_string()))?;
    if r.success() { Ok(()) } else { Err(SshError::Auth(format!("{} rejected", path.display()))) }
}

fn default_key_paths() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default().join(".ssh");
    vec![home.join("id_ed25519"), home.join("id_ecdsa"), home.join("id_rsa")]
}
