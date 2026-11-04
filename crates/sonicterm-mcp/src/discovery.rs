//! Finding the SonicTerm instance to connect to.
//!
//! The order is fixed: `--endpoint`, then `SONICTERM_CONTROL` (set in every pane while the server
//! runs), then the only live endpoint in the per-user directory. Zero or several live endpoints
//! is an error that lists what was found, so the bridge never guesses between instances.

use std::path::{Path, PathBuf};

/// Where the endpoint came from, for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `--endpoint` on the command line.
    Argument,
    /// The `SONICTERM_CONTROL` variable a pane's shell inherits.
    Environment,
    /// The only live endpoint in the per-user directory.
    Directory,
}

/// A chosen endpoint: a Unix socket path or a Windows pipe name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The socket path or pipe name.
    pub address: String,
    /// Where it came from.
    pub source: Source,
}

/// Why no endpoint was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryError {
    /// No live endpoint exists; the server is probably off.
    NoneFound {
        /// The directory searched, if one exists for this user.
        searched: Option<PathBuf>,
    },
    /// More than one live endpoint exists.
    Several(Vec<String>),
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoneFound { searched } => {
                write!(formatter, "no running SonicTerm control server was found")?;
                if let Some(dir) = searched {
                    write!(formatter, " in {}", dir.display())?;
                }
                write!(
                    formatter,
                    ". Is [mcp] enabled = true set in sonicterm.toml, and is SonicTerm running?"
                )
            }
            Self::Several(found) => {
                writeln!(formatter, "several SonicTerm instances are running; pass --endpoint:")?;
                for address in found {
                    writeln!(formatter, "  {address}")?;
                }
                Ok(())
            }
        }
    }
}

/// One live candidate: its address and the process that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The socket path or pipe name.
    pub address: String,
    /// The owning SonicTerm's process id.
    pub pid: u32,
}

/// Choose an endpoint from the argument, the environment and the live candidates, in that order.
pub fn choose(
    argument: Option<String>,
    environment: Option<String>,
    candidates: Vec<Candidate>,
    searched: Option<PathBuf>,
) -> Result<Endpoint, DiscoveryError> {
    if let Some(address) = argument.filter(|address| !address.is_empty()) {
        // When: a non-empty `argument` names the `address`, it overrides every other source.
        return Ok(Endpoint { address, source: Source::Argument });
    }
    if let Some(address) = environment.filter(|address| !address.is_empty()) {
        // When: a non-empty `environment` value names the `address`, the directory is not scanned.
        return Ok(Endpoint { address, source: Source::Environment });
    }
    match candidates.len() {
        0 => Err(DiscoveryError::NoneFound { searched }),
        1 => Ok(Endpoint {
            address: candidates
                .into_iter()
                .next()
                .map(|candidate| candidate.address)
                .unwrap_or_default(),
            source: Source::Directory,
        }),
        _ => Err(DiscoveryError::Several(
            candidates.into_iter().map(|candidate| candidate.address).collect(),
        )),
    }
}

/// The pid a directory entry names: `<pid>.sock` on Unix, `<pid>` on Windows.
pub fn entry_pid(name: &str) -> Option<u32> {
    name.strip_suffix(".sock").unwrap_or(name).parse().ok()
}

/// The live candidates in `dir`, given how to read an entry's address, whether a pid lives, and
/// whether an address has a server listening. A record left by a crash whose pid was reused fails
/// the last check, so it never makes the only running instance look like one of several.
pub fn scan(
    dir: &Path,
    address_of: impl Fn(&Path) -> Option<String>,
    alive: impl Fn(u32) -> bool,
    listening: impl Fn(&str) -> bool,
) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // When: `read_dir` cannot list `dir`, there are no `entries` and so no candidates.
        return Vec::new();
    };
    let mut found: Vec<Candidate> = entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry_pid(entry.file_name().to_str()?)?;
            if !alive(pid) {
                // When: `alive` reports `pid` gone, its record is stale and is skipped.
                return None;
            }
            let address = address_of(&entry.path())?;
            // A live pid with no listener is a stale record from an earlier process.
            listening(&address).then_some(Candidate { address, pid })
        })
        .collect();
    found.sort_by_key(|candidate| candidate.pid);
    found
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod discovery_tests;
