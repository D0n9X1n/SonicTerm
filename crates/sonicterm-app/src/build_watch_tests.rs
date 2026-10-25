use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::*;

/// An `exists` over exactly `present`, so each case states which files are on disk.
fn on_disk(present: &[&str]) -> impl Fn(&Path) -> bool {
    let present: HashSet<PathBuf> = present.iter().map(PathBuf::from).collect();
    move |path| present.contains(path)
}

/// A detached HEAD names no branch, so HEAD alone is watched, even with `packed-refs` on disk.
#[test]
fn a_detached_head_watches_head_only() {
    let git = Path::new("/repo/.git");
    let disk = on_disk(&["/repo/.git/HEAD", "/repo/.git/packed-refs"]);
    assert_eq!(head_watch_paths(git, git, None, disk), vec![PathBuf::from("/repo/.git/HEAD")]);
}

/// A branch whose loose ref exists watches HEAD and that ref; removing the ref is a change Cargo
/// sees, so the set is chosen again.
#[test]
fn a_loose_branch_watches_head_and_its_ref() {
    let git = Path::new("/repo/.git");
    let disk =
        on_disk(&["/repo/.git/HEAD", "/repo/.git/refs/heads/main", "/repo/.git/packed-refs"]);
    assert_eq!(
        head_watch_paths(git, git, Some("refs/heads/main"), disk),
        vec![PathBuf::from("/repo/.git/HEAD"), PathBuf::from("/repo/.git/refs/heads/main")]
    );
}

/// A branch whose ref is only packed watches HEAD, `packed-refs` and the nearest existing parent
/// directory of its missing loose ref, which changes when the loose ref is written.
#[test]
fn a_packed_branch_watches_packed_refs_and_the_nearest_existing_parent() {
    let git = Path::new("/repo/.git");
    let disk = on_disk(&[
        "/repo/.git/HEAD",
        "/repo/.git/packed-refs",
        "/repo/.git/refs",
        "/repo/.git/refs/heads",
    ]);
    assert_eq!(
        head_watch_paths(git, git, Some("refs/heads/feat/about"), disk),
        vec![
            PathBuf::from("/repo/.git/HEAD"),
            PathBuf::from("/repo/.git/packed-refs"),
            PathBuf::from("/repo/.git/refs/heads"),
        ]
    );
}

/// In a worktree, HEAD is the worktree's own, and the branch ref is read from the common directory.
#[test]
fn a_worktree_watches_its_head_and_the_common_ref() {
    let worktree = Path::new("/repo/.git/worktrees/strip");
    let common = Path::new("/repo/.git");
    let disk = on_disk(&[
        "/repo/.git/worktrees/strip/HEAD",
        "/repo/.git/refs/heads/main",
        "/repo/.git/packed-refs",
    ]);
    assert_eq!(
        head_watch_paths(worktree, common, Some("refs/heads/main"), disk),
        vec![
            PathBuf::from("/repo/.git/worktrees/strip/HEAD"),
            PathBuf::from("/repo/.git/refs/heads/main"),
        ]
    );
}

/// No emitted path is missing, in every layout: detached, loose, packed with and without
/// `packed-refs`, and a worktree, since one missing path reruns the build script on every build.
#[test]
fn no_emitted_path_is_missing() {
    let layouts: [(&str, Option<&str>, &[&str]); 5] = [
        ("/r/.git", None, &["/r/.git/HEAD"]),
        ("/r/.git", Some("refs/heads/main"), &["/r/.git/HEAD", "/r/.git/refs/heads/main"]),
        ("/r/.git", Some("refs/heads/a/b"), &["/r/.git/HEAD", "/r/.git/packed-refs", "/r/.git"]),
        ("/r/.git", Some("refs/heads/a/b"), &["/r/.git/HEAD", "/r/.git/refs"]),
        ("/r/.git/worktrees/w", Some("refs/heads/x"), &["/r/.git/worktrees/w/HEAD", "/r/.git"]),
    ];
    for (git_dir, head_ref, present) in layouts {
        let exists = on_disk(present);
        let paths = head_watch_paths(Path::new(git_dir), Path::new("/r/.git"), head_ref, &exists);
        assert!(!paths.is_empty(), "{git_dir} {head_ref:?}: HEAD is watched");
        for path in &paths {
            assert!(exists(path), "{git_dir} {head_ref:?}: {} is missing", path.display());
        }
    }
}
