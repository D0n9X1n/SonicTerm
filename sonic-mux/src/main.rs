//! sonic-mux daemon entrypoint.
//!
//! Subcommands:
//!   sonic-mux daemon --socket <path>     run the server
//!   sonic-mux list   --socket <path>     list live sessions
//!   sonic-mux kill <pane_id> --socket <path>
//!
//! v0.1 — minimal CLI; future versions may grow flags for foreground mode,
//! pid-file, log-file, etc.

use std::{env, process::ExitCode, sync::Arc, thread};

use anyhow::{anyhow, Context, Result};
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
use interprocess::{
    local_socket::{prelude::*, ListenerOptions, Stream},
    TryClone,
};
use sonic_mux::{
    frame::{read_frame, write_frame},
    handle_connection,
    proto::{ClientMsg, ServerMsg},
    ServerState,
};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sonic-mux: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let sub = args.next().ok_or_else(|| anyhow!(usage()))?;
    let rest: Vec<String> = args.collect();
    let socket = extract_socket(&rest)?;
    match sub.as_str() {
        "daemon" => cmd_daemon(&socket),
        "list" => cmd_list(&socket),
        "kill" => {
            let pane_id_str = rest
                .iter()
                .find(|a| !a.starts_with("--"))
                .ok_or_else(|| anyhow!("kill: pane id required"))?;
            let pane_id: u64 = pane_id_str.parse().context("pane id must be u64")?;
            cmd_kill(&socket, pane_id)
        }
        "--help" | "-h" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(anyhow!("unknown subcommand: {other}\n\n{}", usage())),
    }
}

fn usage() -> &'static str {
    "sonic-mux <daemon|list|kill <pane_id>> --socket <path>"
}

fn extract_socket(args: &[String]) -> Result<String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == "--socket" {
            return iter.next().cloned().ok_or_else(|| anyhow!("--socket requires a value"));
        }
    }
    // Default per CLAUDE.md example.
    Ok("/tmp/sonic-mux.sock".to_string())
}

fn cmd_daemon(socket: &str) -> Result<()> {
    let _ = std::fs::remove_file(socket);
    let name = make_socket_name(socket)?;
    let listener = ListenerOptions::new().name(name).create_sync()?;
    // Lock the socket down to the current user. World-accessible (0755)
    // sockets would let any local user attach to our PTYs. On Windows the
    // named-pipe namespace already gives per-session isolation via the
    // default DACL; tightening that further would require raw Win32 SDDL
    // plumbing (TODO before multi-user Windows support).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(socket, perms).with_context(|| format!("chmod 600 {socket}"))?;
    }
    let state = ServerState::new();
    tracing::info!(socket, "sonic-mux daemon listening");
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let state = state.clone();
                thread::spawn(move || {
                    if let Err(e) = serve_stream(state, stream) {
                        tracing::warn!(error = %e, "client connection ended with error");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
            }
        }
    }
    Ok(())
}

fn serve_stream(state: Arc<ServerState>, stream: Stream) -> Result<()> {
    // interprocess 2.x streams are full-duplex; split with try_clone so
    // reader and writer can live on separate threads.
    let writer = stream.try_clone()?;
    handle_connection(state, stream, writer)
}

fn cmd_list(socket: &str) -> Result<()> {
    let (mut r, mut w) = connect(socket)?;
    write_frame(&mut w, &ClientMsg::ListSessions)?;
    let resp: ServerMsg = read_frame(&mut r)?;
    match resp {
        ServerMsg::Sessions(list) => {
            if list.is_empty() {
                println!("(no sessions)");
            } else {
                for s in list {
                    println!("session {} — {} pane(s)", s.id, s.pane_count);
                }
            }
            Ok(())
        }
        other => Err(anyhow!("unexpected reply: {other:?}")),
    }
}

fn cmd_kill(socket: &str, pane_id: u64) -> Result<()> {
    let (mut _r, mut w) = connect(socket)?;
    write_frame(&mut w, &ClientMsg::Kill { pane_id })?;
    println!("killed pane {pane_id}");
    Ok(())
}

fn connect(socket: &str) -> Result<(Stream, Stream)> {
    let name = make_socket_name(socket)?;
    let stream = Stream::connect(name)?;
    let write_half = stream.try_clone()?;
    Ok((stream, write_half))
}

#[cfg(unix)]
fn make_socket_name(path: &str) -> Result<interprocess::local_socket::Name<'_>> {
    Ok(path.to_fs_name::<GenericFilePath>()?)
}

#[cfg(windows)]
fn make_socket_name(path: &str) -> Result<interprocess::local_socket::Name<'_>> {
    // On Windows, treat the path as a named-pipe-style identifier under the
    // namespaced root. Strip leading separators to keep it portable.
    let trimmed = path.trim_start_matches(['/', '\\']);
    Ok(trimmed.to_ns_name::<GenericNamespaced>()?)
}

#[allow(dead_code)]
fn _unused_io() {}
