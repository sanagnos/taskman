//! Stacked tickets: what a ticket's base and its PR's target say, for the sidebar, the GitHub
//! panel, `taskman status` and the brief. taskman records the base and starts the worktree from
//! it; keeping the stack up to date is left to git, GitHub and tools made for it.

use crate::github::Pr;
use crate::store::Base;
use crate::untrusted;

/// Where a stacked ticket stands.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum State {
    /// The base hasn't merged, as far as taskman can tell.
    Open,
    /// The base merged and the PR still targets its branch: merging it now would land in that
    /// branch, not the default one. GitHub retargets only when the base's branch is deleted.
    Stale,
    /// The base merged; the PR targets another branch now, or there's no PR yet.
    Merged,
    /// The ticket's own PR merged: nothing left to say.
    Done,
}

/// `pr` is the ticket's PR as the lookup shows it (open, else merged); `base_merged` whether the
/// base's PR merged, when taskman knows it (the base ticket is open and its PR fetched). Without
/// that, a PR that no longer targets the base's branch means GitHub retargeted it on the merge.
pub fn state(base: &Base, pr: Option<&Pr>, base_merged: Option<bool>) -> State {
    if pr.is_some_and(|p| p.state != "OPEN") {
        return State::Done;
    }
    let targets_base = pr.is_some_and(|p| p.base == base.branch);
    match (base_merged.unwrap_or(pr.is_some() && !targets_base), targets_base) {
        (false, _) => State::Open,
        (true, true) => State::Stale,
        (true, false) => State::Merged,
    }
}

const UPDATE: &str = "pull with rebase in case GitHub rebased the branch, and if the base was squash-merged and its \
                      commits still show in the PR, rebase onto";

/// One sentence for the GitHub panel and `taskman status`; `default` is the default branch. The
/// branches in it may be others' text, so it comes out on one line, cleaned.
pub fn hint(base: &Base, pr: Option<&Pr>, state: State, default: &str) -> Option<String> {
    let Base { key, branch, .. } = base;
    let hint = match (state, pr) {
        (State::Done, _) => return None,
        (State::Open, None) => format!("Stacked on {key}: open the PR against {branch}"),
        (State::Open, Some(p)) if p.base == *branch => {
            format!("Stacked on {key}: the PR targets {branch}")
        }
        (State::Open, Some(p)) => format!("Stacked on {key}, but the PR targets {}", p.base),
        (State::Stale, _) => format!("{key} merged; retarget the PR to {default}"),
        (State::Merged, None) => format!("{key} merged; open the PR against {default}"),
        (State::Merged, Some(p)) => {
            format!(
                "{key} merged; PR now targets {}. Before pushing again, {UPDATE} origin/{}",
                p.base, p.base
            )
        }
    };
    Some(untrusted::line(&hint))
}

/// What a stacked ticket's brief adds. `base_pr` is the base's open PR, when known. A base
/// branch `untrusted::branch` refuses is left out, as briefs hold no text others chose.
pub fn brief(base: &Base, base_pr: Option<u64>, merged: bool, default: &str) -> String {
    let key = &base.key;
    let (named, branch) = match untrusted::branch(&base.branch) {
        Some(b) => (format!("branch {b}"), b.to_string()),
        None => (untrusted::BRANCH_LEFT_OUT.to_string(), format!("{key}'s branch")),
    };
    if merged {
        return format!(
            " This ticket was stacked on {key} ({named}), which has merged, so its PR targets {default} now (retarget \
             it if GitHub didn't). Before pushing again, {UPDATE} origin/{default} and push with --force-with-lease."
        );
    }
    let pr = base_pr.map(|n| format!(", PR #{n}")).unwrap_or_default();
    format!(
        " This ticket stacks on {key} ({named}{pr}): the worktree started from that branch. Open your PR against \
         {branch}, push that branch first if it isn't on origin, and say in the PR body that it depends on {key}. When \
         {key} merges, GitHub may retarget your PR to {default} and rebase your branch on its side: if a push is then \
         refused, pull with rebase rather than force-pushing."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(state: &str, base: &str) -> Pr {
        Pr {
            number: 15,
            title: String::new(),
            url: String::new(),
            state: state.into(),
            is_draft: false,
            review_decision: String::new(),
            mergeable: String::new(),
            head: "eng-52".into(),
            cross_repository: false,
            head_oid: String::new(),
            base: base.into(),
            checks: vec![],
            reviews: vec![],
            comments: vec![],
            threads: Ok(Default::default()),
            stamp: Default::default(),
        }
    }

    #[test]
    fn follows_the_base_and_the_prs_target() {
        let base =
            Base { key: "ENG-51".into(), branch: "eng-51".into(), extra: Default::default() };
        let on_base = pr("OPEN", "eng-51");
        let on_main = pr("OPEN", "main");
        // The base ticket is open in taskman, so its PR says whether it merged.
        assert_eq!(state(&base, None, Some(false)), State::Open);
        assert_eq!(state(&base, Some(&on_base), Some(false)), State::Open);
        assert_eq!(state(&base, Some(&on_main), Some(false)), State::Open);
        assert_eq!(state(&base, Some(&on_base), Some(true)), State::Stale);
        assert_eq!(state(&base, Some(&on_main), Some(true)), State::Merged);
        assert_eq!(state(&base, None, Some(true)), State::Merged);
        // The base ticket was closed: a PR moved off its branch means GitHub retargeted it.
        assert_eq!(state(&base, Some(&on_base), None), State::Open);
        assert_eq!(state(&base, Some(&on_main), None), State::Merged);
        assert_eq!(state(&base, None, None), State::Open);
        // The ticket's own PR merged, alone or with the whole stack.
        assert_eq!(state(&base, Some(&pr("MERGED", "eng-51")), Some(false)), State::Done);
    }

    #[test]
    fn says_what_to_do() {
        let base =
            Base { key: "ENG-51".into(), branch: "eng-51".into(), extra: Default::default() };
        let h = |pr: Option<&Pr>, s| hint(&base, pr, s, "main");
        assert_eq!(h(None, State::Open).unwrap(), "Stacked on ENG-51: open the PR against eng-51");
        assert_eq!(
            h(Some(&pr("OPEN", "eng-51")), State::Open).unwrap(),
            "Stacked on ENG-51: the PR targets eng-51"
        );
        assert_eq!(
            h(Some(&pr("OPEN", "main")), State::Open).unwrap(),
            "Stacked on ENG-51, but the PR targets main"
        );
        assert_eq!(
            h(Some(&pr("OPEN", "eng-51")), State::Stale).unwrap(),
            "ENG-51 merged; retarget the PR to main"
        );
        assert!(
            h(Some(&pr("OPEN", "main")), State::Merged)
                .unwrap()
                .starts_with("ENG-51 merged; PR now targets main. Before pushing again")
        );
        assert_eq!(h(None, State::Merged).unwrap(), "ENG-51 merged; open the PR against main");
        assert_eq!(h(Some(&pr("MERGED", "main")), State::Done), None);
    }

    #[test]
    fn briefs_the_session() {
        let base = Base {
            key: "ENG-51".into(),
            branch: "you/eng-51-login".into(),
            extra: Default::default(),
        };
        let open = brief(&base, Some(14), false, "main");
        assert!(
            open.starts_with(" This ticket stacks on ENG-51 (branch you/eng-51-login, PR #14)"),
            "{open}"
        );
        assert!(
            open.contains("Open your PR against you/eng-51-login")
                && open.contains("depends on ENG-51"),
            "{open}"
        );
        assert!(open.contains("rather than force-pushing"), "{open}");
        assert!(brief(&base, None, false, "main").contains("(branch you/eng-51-login):"));
        // A branch with characters a brief can't vouch for is left out, and the hint cleans it.
        let odd = Base { branch: "eng-51\u{E0049}$(x) y".into(), ..base.clone() };
        let open = brief(&odd, Some(14), false, "main");
        assert!(
            open.starts_with(
                " This ticket stacks on ENG-51 (run `taskman status` for its branch, PR #14)"
            ) && open.contains("Open your PR against ENG-51's branch,")
                && !open.contains("eng-51\u{E0049}"),
            "{open}"
        );
        assert!(!brief(&odd, None, true, "main").contains("$(x)"));
        let h = hint(&odd, None, State::Open, "main").unwrap();
        assert_eq!(h, "Stacked on ENG-51: open the PR against eng-51$(x) y");
        let merged = brief(&base, Some(14), true, "main");
        assert!(
            merged.contains("which has merged, so its PR targets main")
                && merged.contains("origin/main"),
            "{merged}"
        );
    }
}
