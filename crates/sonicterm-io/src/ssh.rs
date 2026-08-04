//! SSH client backend.
//!
//! Provides `SshHandle`, an API-compatible sibling to
//! [`crate::pty::PtyHandle`] that runs a remote interactive shell over SSH
//! using the pure-Rust `russh` client. Bytes flow over crossbeam channels
//! so the rest of the engine (Parser, Grid, App) is unchanged: the choice
//! between a local pty and an SSH session is just which struct lives in
//! the pane.
//!
//! Gated behind the `ssh` Cargo feature because russh pulls a tokio runtime
//! into the dependency graph. With the feature off, only the parser /
//! validator types are compiled (so keymap deserialization still works in
//! both binaries even if neither was built with SSH).
//!
//! ## Security
//!
//! The target string (`user@host[:port]`) arrives from a keybinding, a
//! command-palette entry, or — long-term — clickable hyperlinks; treat it
//! as untrusted. [`validate_host`] applies a strict allow-list of
//! characters before any russh call, even though russh itself doesn't
//! shell-execute the host (defense in depth). Port is parsed as u16. We
//! never accept passwords on the command line; authentication uses key files only.
//!
//! ## Auth precedence
//!
//! 1. If the caller passes an explicit key path, only that file is tried.
//! 2. Otherwise, try `~/.ssh/id_ed25519` then `~/.ssh/id_rsa`.
//!
//! SSH-agent authentication is not implemented.
//!
//! Password / keyboard-interactive auth is intentionally not supported
//! in v1.

use std::fmt;

/// Parsed SSH target — `user@host[:port]`. Default port is 22.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// Remote username.
    pub user: String,
    /// Remote host (hostname or IP).
    pub host: String,
    /// TCP port; defaults to 22.
    pub port: u16,
}

impl fmt::Display for SshTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.port == 22 {
            write!(f, "{}@{}", self.user, self.host)
        } else {
            // When: `port` is nondefault, include it so formatting preserves the target endpoint.
            write!(f, "{}@{}:{}", self.user, self.host, self.port)
        }
    }
}

/// Parse `user@host[:port]`. The user / host components are validated by
/// the strict allow-list; an invalid target returns `Err` and never
/// reaches the network.
pub fn parse_target(s: &str) -> Result<SshTarget, SshError> {
    if s.is_empty() {
        // When: `s` is empty, no user or host can be parsed without inventing identity.
        return Err(SshError::ParseTarget("empty target".into()));
    }
    if s.len() > 256 {
        // When: `s` exceeds the target bound, reject it before parsing or allocation.
        return Err(SshError::ParseTarget("target too long".into()));
    }
    let (user, rest) =
        s.split_once('@').ok_or_else(|| SshError::ParseTarget("missing '@'".into()))?;
    if user.is_empty() {
        // When: `user` is empty, authentication has no remote account name.
        return Err(SshError::ParseTarget("empty user".into()));
    }
    validate_user(user)?;

    let (host, port) = if let Some((h, p)) = rest.rsplit_once(':') {
        // When: `rest` splits at its last colon; bare IPv6 must be bracketed or its tail parses as a port.
        // Bracketed IPv6 with port: `[::1]:2222` — strip the brackets.
        let (h, p) = if let Some(stripped) = h.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            (stripped, p)
        } else {
            // When: `h` has no brackets to strip, so it is already the bare host.
            (h, p)
        };
        let port: u16 = p.parse().map_err(|_| SshError::ParseTarget(format!("bad port {p:?}")))?;
        if port == 0 {
            // When: `port` is zero, no TCP service endpoint can be contacted.
            return Err(SshError::ParseTarget("port 0".into()));
        }
        (h, port)
    } else {
        // When: `rest` has no explicit port suffix, use the SSH default.
        (rest, 22)
    };
    if host.is_empty() {
        // When: `host` is empty, network resolution has no destination.
        return Err(SshError::ParseTarget("empty host".into()));
    }
    validate_host(host)?;

    Ok(SshTarget { user: user.to_string(), host: host.to_string(), port })
}

/// Strict allow-list: ASCII alphanumeric, `.`, `-`, `_`, plus `:` for IPv6.
/// Rejects every shell metachar (& | ^ < > " ' ` $ * ? ; \ space NUL
/// CR LF, etc). Length capped at 253 (DNS limit).
pub fn validate_host(host: &str) -> Result<(), SshError> {
    if host.is_empty() || host.len() > 253 {
        // When: `host` is empty or exceeds the DNS bound, reject it before network access.
        return Err(SshError::ParseTarget("host length out of range".into()));
    }
    for ch in host.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':');
        if !ok {
            // When: `ok` is false, the host contains a character outside the network-name allow-list.
            return Err(SshError::ParseTarget(format!("forbidden char in host: {ch:?}")));
        }
    }
    Ok(())
}

/// Strict allow-list for the user component: ASCII alphanumeric plus
/// `._-`. Length capped at 64.
pub fn validate_user(user: &str) -> Result<(), SshError> {
    if user.is_empty() || user.len() > 64 {
        // When: `user` is empty or exceeds the account-name bound, reject it before authentication.
        return Err(SshError::ParseTarget("user length out of range".into()));
    }
    for ch in user.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-');
        if !ok {
            // When: `ok` is false, the username contains a character outside the account-name allow-list.
            return Err(SshError::ParseTarget(format!("forbidden char in user: {ch:?}")));
        }
    }
    Ok(())
}

/// SSH-related error type, used by both the parser (always built) and the
/// connect path (only built with `feature = "ssh"`).
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// Failed to parse a `user@host[:port]` target string.
    #[error("invalid SSH target: {0}")]
    ParseTarget(String),
    /// TCP / SSH handshake failure.
    #[cfg(feature = "ssh")]
    #[error("ssh connection failed: {0}")]
    Connect(String),
    /// All authentication methods were tried and failed.
    #[cfg(feature = "ssh")]
    #[error("ssh authentication failed: all methods exhausted")]
    AuthExhausted,
    /// Underlying I/O error.
    #[cfg(feature = "ssh")]
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// ----------------------------------------------------------------------
// Live connection — feature-gated.
// ----------------------------------------------------------------------

#[cfg(feature = "ssh")]
mod live {
    use super::{SshError, SshTarget};
    use crossbeam_channel::{Receiver, Sender};
    use parking_lot::Mutex;
    use std::{path::PathBuf, sync::Arc};

    /// Handle to a running SSH session with one PTY-allocated interactive
    /// channel. The shape mirrors [`crate::pty::PtyHandle`] so it can be
    /// dropped into the same `PaneBackend` slot.
    pub struct SshHandle {
        pub out_rx: Receiver<Vec<u8>>,
        pub in_tx: Sender<Vec<u8>>,
        pub resize: Box<dyn Fn(u16, u16) + Send + Sync>,
        /// Set to true on drop; the background tokio task observes this and
        /// closes the channel + disconnects cleanly.
        shutdown: Arc<Mutex<bool>>,
        /// Resize sender; Drop also sends `(0, 0)` to wake the background task.
        resize_tx: Sender<(u16, u16)>,
    }

    impl SshHandle {
        /// Connect to `target` with the given auth source.
        ///
        /// `key_path_or_agent`:
        /// - `Some(path)` — try only that key file.
        /// - `None` — try `~/.ssh/id_ed25519` then `~/.ssh/id_rsa`.
        ///
        /// SSH-agent authentication is not implemented.
        pub fn connect(
            target: SshTarget,
            key_path_or_agent: Option<PathBuf>,
            cols: u16,
            rows: u16,
        ) -> Result<Self, SshError> {
            // Defense in depth: `SshTarget` exposes public fields, so a
            // direct caller can bypass `parse_target` and stuff arbitrary
            // bytes into `host` / `user`. Re-run the same validators here
            // — the network MUST NOT be touched on a malformed target.
            // See the Security section in the module docstring.
            super::validate_host(&target.host)?;
            super::validate_user(&target.user)?;
            if target.port == 0 {
                // When: `target.port` is zero, reject a directly constructed target before network access.
                return Err(SshError::ParseTarget("port 0".into()));
            }

            let (out_tx, out_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
            let (in_tx, in_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
            let (resize_tx, resize_rx) = crossbeam_channel::unbounded::<(u16, u16)>();
            let shutdown = Arc::new(Mutex::new(false));

            let target_for_task = target.clone();
            let shutdown_for_task = shutdown.clone();
            std::thread::Builder::new()
                .name("sonic-ssh-session".into())
                .spawn(move || {
                    let rt =
                        match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                            Ok(rt) => rt,
                            Err(e) => {
                                // When: runtime construction fails, the session thread cannot drive any SSH future.
                                tracing::error!("ssh: failed to build runtime: {e}");
                                return;
                            }
                        };
                    if let Err(e) = rt.block_on(super::live_impl::run_session(
                        target_for_task,
                        key_path_or_agent,
                        cols,
                        rows,
                        out_tx,
                        in_rx,
                        resize_rx,
                        shutdown_for_task,
                    )) {
                        tracing::error!("ssh session ended: {e}");
                    }
                })
                .map_err(|e| SshError::Connect(format!("spawn session thread: {e}")))?;

            let resize_tx_for_closure = resize_tx.clone();
            let resize = Box::new(move |cols: u16, rows: u16| {
                let _ = resize_tx_for_closure.send((cols, rows));
            });

            Ok(Self { out_rx, in_tx, resize, shutdown, resize_tx })
        }
    }

    // Lifecycle: dropping `SshHandle` sets `shutdown` and queues a `resize_tx` nudge; the 16 ms poll observes shutdown and disconnects.
    impl Drop for SshHandle {
        fn drop(&mut self) {
            *self.shutdown.lock() = true;
            // Queue a sentinel so the next resize drain does no remote resize.
            // The channel poll timeout bounds when the task observes shutdown.
            let _ = self.resize_tx.send((0, 0));
        }
    }
}

#[cfg(feature = "ssh")]
pub use live::SshHandle;

#[cfg(feature = "ssh")]
mod live_impl {
    use super::{SshError, SshTarget};
    use crossbeam_channel::{Receiver, Sender, TryRecvError};
    use parking_lot::Mutex;
    use russh::client::{self, Handle, Handler};
    use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKey};
    use russh::{ChannelMsg, CryptoVec, Disconnect};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    struct Client;

    // `Handler`'s methods return `impl Future + Send` directly rather than
    // being `#[async_trait]` methods, so the implementation is a plain
    // `async fn`. Wrapping this impl in `#[async_trait::async_trait]` rewrites
    // the signature into a boxed future and no longer matches the trait.
    impl Handler for Client {
        type Error = russh::Error;
        async fn check_server_key(
            &mut self,
            _server_public_key: &PublicKey,
        ) -> Result<bool, Self::Error> {
            // Host keys are currently accepted unconditionally. No key is
            // persisted or compared on later connections, so this does not
            // authenticate the server. A known-hosts policy belongs here
            // before SSH is exposed as a complete user feature.
            Ok(true)
        }
    }

    #[allow(clippy::too_many_arguments)] // internal session runner — splitting into a struct here would just rename the bag of args
    pub async fn run_session(
        target: SshTarget,
        key_path: Option<PathBuf>,
        cols: u16,
        rows: u16,
        out_tx: Sender<Vec<u8>>,
        in_rx: Receiver<Vec<u8>>,
        resize_rx: Receiver<(u16, u16)>,
        shutdown: Arc<Mutex<bool>>,
    ) -> Result<(), SshError> {
        let cfg = Arc::new(client::Config::default());
        let addr = (target.host.as_str(), target.port);
        let mut sess = client::connect(cfg, addr, Client)
            .await
            .map_err(|e| SshError::Connect(format!("{e}")))?;

        authenticate(&mut sess, &target.user, key_path).await?;

        let mut chan = sess
            .channel_open_session()
            .await
            .map_err(|e| SshError::Connect(format!("open session: {e}")))?;
        chan.request_pty(false, "xterm-256color", u32::from(cols), u32::from(rows), 0, 0, &[])
            .await
            .map_err(|e| SshError::Connect(format!("request pty: {e}")))?;
        chan.request_shell(false)
            .await
            .map_err(|e| SshError::Connect(format!("request shell: {e}")))?;

        loop {
            if *shutdown.lock() {
                // When: `shutdown` is set by Drop or input loss, leave the poll loop and disconnect cleanly.
                break;
            }
            // Drain pending user input (non-blocking).
            loop {
                // When: `in_rx` data is sent; empty ends this drain, while disconnection requests shutdown.
                match in_rx.try_recv() {
                    Ok(bytes) => {
                        if let Err(e) = chan.data(bytes.as_slice()).await {
                            tracing::warn!("ssh: write to channel failed: {e:?}");
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        *shutdown.lock() = true;
                        break;
                    }
                }
            }
            // Drain pending resize requests (last wins).
            let mut latest_resize: Option<(u16, u16)> = None;
            loop {
                // When: `resize_rx` yields a zero `c`/`r` nudge, a real `pair`, or an error ending this drain.
                match resize_rx.try_recv() {
                    Ok((c, r)) if c == 0 && r == 0 => {}
                    Ok(pair) => latest_resize = Some(pair),
                    Err(_) => break,
                }
            }
            if let Some((c, r)) = latest_resize {
                // When: `latest_resize` exists, send only the newest queued dimensions to the server.
                let _ = chan.window_change(u32::from(c), u32::from(r), 0, 0).await;
            }

            // Poll the channel for output without blocking forever.
            let next = tokio::time::timeout(Duration::from_millis(16), chan.wait()).await;
            // When: `next` data is forwarded, stream end stops, control is ignored, and timeout repolls.
            match next {
                Ok(Some(ChannelMsg::Data { data })) => {
                    if out_tx.send(data.to_vec()).is_err() {
                        // When: `out_tx` has no consumer, stop the remote session instead of buffering output.
                        break;
                    }
                }
                Ok(Some(ChannelMsg::ExtendedData { data, ext: _ })) => {
                    if out_tx.send(data.to_vec()).is_err() {
                        // When: `out_tx` has no consumer, stop the remote session instead of buffering output.
                        break;
                    }
                }
                Ok(Some(ChannelMsg::Eof)) | Ok(Some(ChannelMsg::Close)) | Ok(None) => break,
                Ok(Some(_)) => {}
                Err(_) => {}
            }
        }

        let _ = CryptoVec::new(); // keep import alive on stripped builds
        let _ = sess.disconnect(Disconnect::ByApplication, "bye", "en").await;
        Ok(())
    }

    async fn authenticate(
        sess: &mut Handle<Client>,
        user: &str,
        explicit_key: Option<PathBuf>,
    ) -> Result<(), SshError> {
        if let Some(path) = explicit_key {
            // When: `explicit_key` is supplied, try only that file as the caller requested.
            return try_key_file(sess, user, &path).await;
        }
        // ssh-agent fallback is not wired up: agent auth requires implementing
        // a custom `Signer`, so only on-disk keys are tried here.
        if let Some(home) = std::env::var_os("HOME") {
            // When: `home` is available, probe the supported default private-key paths in order.
            let home = PathBuf::from(home);
            for name in ["id_ed25519", "id_rsa"] {
                let p = home.join(".ssh").join(name);
                if p.exists() && try_key_file(sess, user, &p).await.is_ok() {
                    // When: `p` exists and authenticates successfully, no later default key is needed.
                    return Ok(());
                }
            }
        }
        Err(SshError::AuthExhausted)
    }

    async fn try_key_file(
        sess: &mut Handle<Client>,
        user: &str,
        path: &std::path::Path,
    ) -> Result<(), SshError> {
        let key = load_secret_key(path, None)
            .map_err(|e| SshError::Connect(format!("load key {}: {e}", path.display())))?;
        // `None` selects the legacy SHA-1 `ssh-rsa` signature for RSA keys and
        // is ignored for every other algorithm. Modern servers reject SHA-1
        // signatures, so RSA keys need an explicit stronger hash before RSA is
        // advertised as supported.
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
        let result = sess
            .authenticate_publickey(user, key)
            .await
            .map_err(|e| SshError::Connect(format!("publickey auth: {e}")))?;
        if result.success() {
            Ok(())
        } else {
            // When: `result.success` is false, the server rejected this public key.
            Err(SshError::AuthExhausted)
        }
    }
}
