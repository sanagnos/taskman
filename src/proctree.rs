//! A process's descendants, for telling whether a session left work running that its hooks don't
//! report (see "Sleeping sessions" in docs/design.md). `ps` lists every process with its parent on
//! macOS and Linux alike, so one code path serves both.

use crate::proc;
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::time::Duration;

/// Every process under `root`, children and theirs in turn, not `root` itself. Runs `ps`, so not on
/// the UI thread.
pub fn descendants(root: u32) -> Result<HashSet<u32>, String> {
    let mut ps = Command::new("ps");
    ps.args(["-A", "-o", "pid=,ppid=,stat="]);
    let out = proc::run(&mut ps, Duration::from_secs(5)).map_err(|e| format!("ps: {e}"))?;
    if !out.status.success() {
        return Err(format!("ps: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(below(&pairs(&String::from_utf8_lossy(&out.stdout)), root))
}

/// `ps -o pid=,ppid=,stat=`'s lines as (pid, parent) pairs; a line that isn't two numbers and a
/// state is skipped, and so is a zombie (`Z`): an ended process its parent hasn't reaped yet runs
/// nothing.
fn pairs(text: &str) -> Vec<(u32, u32)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (pid, ppid) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
            let state = it.next()?;
            (it.next().is_none() && !state.starts_with('Z')).then_some((pid, ppid))
        })
        .collect()
}

/// The pids under `root` in a list of (pid, parent) pairs.
fn below(pairs: &[(u32, u32)], root: u32) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid) in pairs {
        children.entry(ppid).or_default().push(pid);
    }
    let mut found = HashSet::new();
    let mut todo = vec![root];
    while let Some(p) = todo.pop() {
        for &c in children.get(&p).into_iter().flatten() {
            // A pid listed twice (a process reused its number between reads) can't loop.
            if c != root && found.insert(c) {
                todo.push(c);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reads_ps_output() {
        let text = "    1     0 Ss\n  100     1 S+\n  101   100 Ssl\n 2002   101 R\n  300   100 Z\n  \
                    301   100 Z+\nnot a line\n 7 1\n 8 9 10 11\n";
        assert_eq!(pairs(text), [(1, 0), (100, 1), (101, 100), (2002, 101)]);
        assert!(pairs("").is_empty());
    }

    #[test]
    fn finds_what_is_under_a_process() {
        let tree = [(1, 0), (100, 1), (101, 100), (102, 100), (103, 102), (200, 1), (201, 200)];
        assert_eq!(below(&tree, 100), HashSet::from([101, 102, 103]));
        assert_eq!(below(&tree, 103), HashSet::new());
        assert_eq!(below(&tree, 999), HashSet::new());
        // Never itself, even in a loop.
        assert_eq!(below(&[(5, 6), (6, 5)], 5), HashSet::from([6]));
    }

    #[test]
    fn runs_ps_and_finds_its_own_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let found = descendants(std::process::id());
        let _ = child.kill();
        let _ = child.wait();
        assert!(found.unwrap().contains(&child.id()));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn reading_ps_never_panics(text in "(\\PC|\\n){0,200}") {
            let _ = below(&pairs(&text), 1);
        }

        #[test]
        fn what_is_found_hangs_from_the_root(
            list in proptest::collection::vec((1u32..40, 0u32..40), 0..60),
            root in 1u32..40,
        ) {
            let found = below(&list, root);
            prop_assert!(!found.contains(&root));
            for pid in &found {
                prop_assert!(list.iter().any(|(p, _)| p == pid));
            }
        }
    }
}
