//! The files `build.rs` watches so the build stamp follows HEAD. `build.rs` includes this file;
//! the library compiles it only to test it.

use std::path::{Path, PathBuf};

/// The existing files whose change means HEAD moved. `exists` says whether a path exists now; a
/// missing watched path makes Cargo rerun the build script on every build, so none is returned.
///
/// - Detached HEAD: `HEAD` in `git_dir` only.
/// - On a branch whose loose ref exists: `HEAD` and that ref, in `common_dir`.
/// - On a branch whose ref is only packed: `HEAD`, `packed-refs` and the nearest existing parent
///   directory of the missing loose ref, whose modification time changes when the ref is written.
///
/// A worktree's `git_dir` holds only its own `HEAD`; branches and packed refs live in `common_dir`.
pub(crate) fn head_watch_paths(
    git_dir: &Path,
    common_dir: &Path,
    head_ref: Option<&str>,
    exists: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut paths = vec![git_dir.join("HEAD")];
    if let Some(name) = head_ref {
        let loose = common_dir.join(name);
        if exists(&loose) {
            // When: the loose ref exists, it changes on every commit to the branch.
            paths.push(loose);
        } else {
            // The ref is only packed: `packed-refs` holds it, and writing the loose ref later
            // changes its nearest existing parent directory.
            paths.push(common_dir.join("packed-refs"));
            paths.extend(
                loose.ancestors().skip(1).find(|parent| exists(parent)).map(Path::to_path_buf),
            );
        }
    }
    paths.retain(|path| exists(path));
    paths
}

#[cfg(test)]
#[path = "build_watch_tests.rs"]
mod build_watch_tests;
