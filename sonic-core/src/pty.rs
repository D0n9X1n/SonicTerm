//! Cross-platform PTY spawning.
//!
//! Wraps the [`portable-pty`] crate so callers don't need to depend on it
//! directly. `PtyHandle` owns the slave-side child and the master read/write
//! pair, all decoupled by channels for use from the render thread.

use std::{
    io::{Read, Write},
    sync::Arc,
    thread,
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

/// Outgoing message: bytes to write to the pty master (typed by user).
type Outgoing = Vec<u8>;
/// Incoming message: bytes read from the pty master (program output).
type Incoming = Vec<u8>;

/// Handle to a running pty process.
pub struct PtyHandle {
    pub out_rx: Receiver<Incoming>,
    pub in_tx: Sender<Outgoing>,
    pub resize: Box<dyn Fn(u16, u16) + Send + Sync>,
    _child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
}

impl PtyHandle {
    /// Spawn the user's default shell.
    pub fn spawn_default_shell(cols: u16, rows: u16) -> Result<Self> {
        let shell = default_shell();
        Self::spawn(&shell, cols, rows)
    }

    /// Spawn `cmd` (may include arguments via shell-style splitting handled
    /// upstream — we expect a single program path here for simplicity).
    pub fn spawn(cmd: &str, cols: u16, rows: u16) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let mut builder = CommandBuilder::new(cmd);
        if let Ok(home) = std::env::var("HOME") {
            builder.cwd(home);
        }
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");

        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let master = pair.master;
        let reader = master.try_clone_reader()?;
        let writer = master.take_writer()?;
        let master = Arc::new(Mutex::new(master));

        let (out_tx, out_rx) = crossbeam_channel::unbounded::<Incoming>();
        let (in_tx, in_rx) = crossbeam_channel::unbounded::<Outgoing>();

        // Reader thread: pty -> out_rx.
        spawn_reader_thread(reader, out_tx);
        // Writer thread: in_rx -> pty.
        spawn_writer_thread(writer, in_rx);

        let resize_master = master.clone();
        let resize = Box::new(move |cols: u16, rows: u16| {
            let _ = resize_master.lock().resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        });

        Ok(Self { out_rx, in_tx, resize, _child: Arc::new(Mutex::new(child)) })
    }
}

fn spawn_reader_thread(mut reader: Box<dyn Read + Send>, tx: Sender<Incoming>) {
    thread::Builder::new()
        .name("sonic-pty-reader".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("pty read error: {e}");
                        break;
                    }
                }
            }
        })
        .expect("spawn pty reader");
}

fn spawn_writer_thread(mut writer: Box<dyn Write + Send>, rx: Receiver<Outgoing>) {
    thread::Builder::new()
        .name("sonic-pty-writer".into())
        .spawn(move || {
            while let Ok(bytes) = rx.recv() {
                if let Err(e) = writer.write_all(&bytes) {
                    tracing::warn!("pty write error: {e}");
                    break;
                }
                let _ = writer.flush();
            }
        })
        .expect("spawn pty writer");
}

fn default_shell() -> String {
    if cfg!(target_os = "windows") {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
    }
}
