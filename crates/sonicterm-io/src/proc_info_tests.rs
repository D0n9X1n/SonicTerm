use super::*;

#[test]
fn normalizes_posix_and_windows_paths_to_the_same_key() {
    assert_eq!(normalize_proc_name("/usr/local/bin/Claude"), "claude");
    assert_eq!(normalize_proc_name(r"C:\Program Files\Claude\Claude.EXE"), "claude");
}

#[test]
fn strips_one_login_prefix_from_the_basename() {
    assert_eq!(normalize_proc_name("/bin/-ZSH"), "zsh");
    assert_eq!(normalize_proc_name("--zsh"), "-zsh");
}

#[test]
fn strips_one_case_insensitive_exe_suffix() {
    assert_eq!(normalize_proc_name("PowerShell.EXE"), "powershell");
    assert_eq!(normalize_proc_name("tool.exe.exe"), "tool.exe");
}

#[test]
fn accepts_basename_only_and_preserves_non_exe_suffixes() {
    assert_eq!(normalize_proc_name("NVIM"), "nvim");
    assert_eq!(normalize_proc_name("python3.12"), "python3.12");
    assert_eq!(normalize_proc_name("archive.com"), "archive.com");
}

#[test]
fn dotted_path_components_do_not_affect_basename_suffix_handling() {
    assert_eq!(normalize_proc_name("/opt/tools.v2/bin/Node.EXE"), "node");
    assert_eq!(normalize_proc_name(r"C:\tools.v2\bin\Node.EXE"), "node");
}

#[test]
fn normalization_does_not_parse_arguments() {
    assert_eq!(normalize_proc_name(r"C:\bin\node.exe --version"), "node.exe --version");
}

/// Run `walk` on another thread and fail after 5 s instead of hanging the suite on a cycle.
fn within_deadline<T: Send + 'static>(walk: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(walk());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the process-tree walk did not finish: it looped on a cycle")
}

#[test]
fn deepest_descendant_follows_the_longest_chain() {
    // Protect the ordinary choice: the deepest descendant of the shell, not a shallower sibling.
    let links = [(20, 10), (30, 20), (40, 10), (50, 30)];
    assert_eq!(deepest_descendant(10, &links), 50);
    assert_eq!(deepest_descendant(99, &links), 99);
}

#[test]
fn deepest_descendant_ends_on_self_parent_and_cycles() {
    // Protect the macOS probe from looping forever when a non-atomic process snapshot links pids
    // in a cycle, including a pid listed as its own parent; each pid is visited at most once.
    let self_parent = within_deadline(|| deepest_descendant(10, &[(20, 10), (20, 20)]));
    assert_eq!(self_parent, 20);
    let two_cycle = within_deadline(|| deepest_descendant(10, &[(20, 10), (30, 20), (20, 30)]));
    assert_eq!(two_cycle, 30);
    let three_cycle =
        within_deadline(|| deepest_descendant(10, &[(20, 10), (30, 20), (40, 30), (20, 40)]));
    assert_eq!(three_cycle, 40);
    let root_cycle = within_deadline(|| deepest_descendant(10, &[(20, 10), (10, 20)]));
    assert_eq!(root_cycle, 20);
}

/// The walk before it was indexed: rescan every link for each visited pid. Kept as the reference
/// the indexed walk must agree with, child order and tie-breaking included.
fn unindexed_deepest(root: u32, links: &[(u32, u32)]) -> u32 {
    let mut deepest = root;
    let mut deepest_depth = 0usize;
    let mut visited = std::collections::HashSet::new();
    visited.insert(root);
    let mut frontier: Vec<(u32, usize)> = vec![(root, 0)];
    while let Some((current, depth)) = frontier.pop() {
        for &(child, parent) in links {
            if parent != current || !visited.insert(child) {
                // When: `child` is not under `current`, or was already visited, skip it.
                continue;
            }
            if depth + 1 > deepest_depth {
                deepest_depth = depth + 1;
                deepest = child;
            }
            frontier.push((child, depth + 1));
        }
    }
    deepest
}

#[test]
fn equal_depth_siblings_keep_the_first_listed_winner() {
    // Two children at the same depth: the first one the links list wins, as before indexing, so
    // the tab icon does not flip between siblings when only the walk's data structure changed.
    assert_eq!(deepest_descendant(10, &[(20, 10), (30, 10)]), 20);
    assert_eq!(deepest_descendant(10, &[(30, 10), (20, 10)]), 30);
    let tied = [(20, 10), (30, 10), (21, 20), (31, 30)];
    assert_eq!(deepest_descendant(10, &tied), unindexed_deepest(10, &tied));
}

#[test]
fn the_indexed_walk_matches_the_unindexed_one_on_random_trees() {
    // Seeded random link lists with repeated pids, self-parents and cycles: the indexed walk picks
    // exactly the pid the per-node rescan picked, from every root.
    let mut seed: u64 = 0x1793;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    for _ in 0..300 {
        let pid_count = next(30) + 2;
        let link_count = next(60) as usize;
        let links: Vec<(u32, u32)> =
            (0..link_count).map(|_| (next(pid_count) as u32, next(pid_count) as u32)).collect();
        for root in 0..pid_count as u32 {
            assert_eq!(
                deepest_descendant(root, &links),
                unindexed_deepest(root, &links),
                "root {root} over {links:?}"
            );
        }
    }
}

#[test]
fn the_indexed_walk_reads_each_link_at_most_once() {
    // The walk's own work is counted: one read per child link it reaches. A 5,000-process chain
    // reads 4,999 links where the rescanning walk compared 12.5 million, and on random link lists
    // with cycles and repeats the walk never reads more links than exist.
    let links: Vec<(u32, u32)> = (1..5_000).map(|pid| (pid + 1, pid)).collect();
    let children = children_by_parent(&links);
    let mut examined = 0;
    assert_eq!(deepest_counting(1, &children, &mut examined), 5_000);
    assert_eq!(examined, links.len(), "a chain reads each link exactly once");

    let mut seed: u64 = 0x1793_0001;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    for _ in 0..200 {
        let pid_count = next(40) + 2;
        let links: Vec<(u32, u32)> = (0..next(120) as usize)
            .map(|_| (next(pid_count) as u32, next(pid_count) as u32))
            .collect();
        let children = children_by_parent(&links);
        for root in 0..pid_count as u32 {
            let mut examined = 0;
            deepest_counting(root, &children, &mut examined);
            assert!(examined <= links.len(), "{examined} reads over {} links", links.len());
        }
    }
}

/// On macOS the batch probe agrees with the single-pid probe for this test process, which has no
/// children, so both name the test binary itself; a pid that does not exist resolves to nothing.
#[cfg(target_os = "macos")]
#[test]
fn the_batch_probe_matches_the_single_probe() {
    let own = std::process::id();
    let single = foreground_process_info(own);
    let batch = foreground_processes_info(&[own, u32::MAX, own]);
    assert!(single.is_some(), "this process can always read itself");
    assert_eq!(batch, vec![single.clone(), None, single]);
}
