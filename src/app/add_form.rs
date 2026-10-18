//! The Add form (`prefix n`): a ticket, by number, key or link, words of an open issue's title, or
//! an open issue picked with ↑/↓ (on a Local files board, a title is a new ticket instead), or an
//! agent; its CLI, and the worktree and base it starts from. Also `parse_key`, which the commands
//! use too.

use super::sync::Msg;
use super::ticket::Added;
use super::{App, Field, Form, FormKind, List, Mode, SettingsItem};
use crate::store::{self, Cli, Provider, Tracker};
use crate::tracker::{self, Access, Listed};
use crate::{github, keys, local, untrusted, worktree};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A project's open issues, which the Add form lists when its tracker can
/// (`Tracker::lists_open`).
pub(crate) enum OpenIssues {
    /// Being listed by the request with this id (`Listing`).
    Loading(u64),
    Listed(Vec<Listed>),
    Failed(String),
}

/// A request for a project's open issues, which its reply carries (`Msg::OpenIssues`). The reply
/// is kept only while its folder still waits on this request and the project's tracker is still
/// the one asked: a list that lands after the tracker was changed (another site, prefix or board),
/// or the project deleted or added again, would otherwise pick another tracker's issues.
#[derive(Clone)]
pub(crate) struct Listing {
    id: u64,
    repo: PathBuf,
    tracker: Tracker,
}

/// What the Ticket field lists: the open issues the input matches, or a note saying why there are
/// none (empty when the field's hint says it already).
enum Shown<'a> {
    Issues(Vec<&'a Listed>),
    Note(Vec<String>),
}

/// How long a project's open issues are listed from what was fetched before the form asks again.
const OPEN_ISSUES_FRESH: Duration = Duration::from_secs(60);
/// The open issues shown under the Ticket field at once; ↑/↓ scroll through more.
const OPEN_ISSUES_SHOWN: usize = 5;

const CREATE: &str = "Create";
pub(super) const NO_WORKTREE: &str = "Don't create";

/// What the Add form's ticket field names in a Local files project whose prefix is `prefix`.
#[derive(Debug, PartialEq)]
enum LocalInput {
    /// A ticket on the board, by its number or key.
    Key(String),
    /// Anything else is the title of a new ticket.
    Title,
}

/// Reads the Add form's ticket field in a Local files project; None while it's empty.
fn local_input(input: &str, prefix: &str) -> Option<LocalInput> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if !prefix.is_empty() && input.chars().all(|c| c.is_ascii_digit()) {
        return Some(LocalInput::Key(format!("{}-{input}", prefix.to_uppercase())));
    }
    Some(parse_key(input).map_or(LocalInput::Title, LocalInput::Key))
}

/// The ticket the Add form names in a Local files project: a file on the board, checked, or a new
/// one created from a title. Read fresh, not from the cache, since it's what gets added.
fn local_ticket(board: &Path, prefix: &str, input: &str) -> Result<String, String> {
    match local_input(input, prefix) {
        None => Err("Enter a ticket number or key, or a title for a new ticket".into()),
        Some(LocalInput::Key(key)) => local::find(board, &key).map(|f| f.key),
        Some(LocalInput::Title) => {
            local::create(board, prefix, input, "").map(|f| f.key).map_err(|e| e.message)
        }
    }
}

/// A ticket key to show in examples: the project's prefix, or ENG.
pub(super) fn example_key(tracker: Option<&Tracker>) -> String {
    let prefix = tracker.map(|t| t.key.as_str()).filter(|k| !k.is_empty()).unwrap_or("ENG");
    format!("{}-52", prefix.to_uppercase())
}

/// A ticket's branch as the Add form can tell before the issue is fetched: `…` for the title.
fn branch_preview(format: &str, provider: Option<Provider>, key: &str) -> String {
    let tracker = provider.and_then(Provider::branch_source).unwrap_or("{KEY}-…");
    let b = format
        .replace(worktree::TRACKER, tracker)
        .replace("{KEY}", &key.to_uppercase())
        .replace("{key}", &key.to_lowercase());
    b.replace("{slug}", "…")
}

/// Accepts `ENG-52`, `eng-52`, or a link to one: Jira's `/browse/ENG-52` or `selectedIssue=ENG-52`,
/// Linear's `/issue/ENG-52/slug`. Any other link gives the first part of its path that reads as a
/// key, then of its query, then of its fragment: a slug ending in `phase-2` after the key, or a
/// fragment `#comment-10`, is never the ticket.
pub fn parse_key(input: &str) -> Option<String> {
    let s = input.trim();
    let (s, fragment) = s.split_once('#').unwrap_or((s, ""));
    let (path, query) = s.split_once('?').unwrap_or((s, ""));
    // The host can read as a key (`eng-52.example.com`), so the path starts after it.
    let path = path
        .split_once("://")
        .map_or(path, |(_, rest)| rest.split_once('/').map_or("", |(_, p)| p));
    let parts: Vec<&str> = path.split(['/', ' ']).collect();
    let after =
        |marker: &str| parts.windows(2).find(|w| w[0] == marker && is_key(w[1])).map(|w| w[1]);
    let params = || query.split('&').filter_map(|p| p.split_once('='));
    let key = after("browse")
        .or_else(|| after("issue"))
        .or_else(|| {
            params().find(|&(name, v)| name == "selectedIssue" && is_key(v)).map(|(_, v)| v)
        })
        .or_else(|| parts.iter().copied().find(|p| is_key(p)))
        .or_else(|| params().map(|(_, v)| v).find(|v| is_key(v)))
        .or_else(|| fragment.split(['/', '=', '?', '&', ' ']).find(|p| is_key(p)))?;
    Some(key.to_uppercase())
}

/// Whether what's typed as an agent's name names a ticket of the project whose prefix is
/// `prefix` instead: a number (`52`, `#52`), a link, or a key with the project's prefix (any key
/// when it has none). Another key stays a name: `phase-2` reads as a key, and is a fair name.
fn names_ticket(prefix: &str, input: &str) -> bool {
    let input = input.trim();
    let number = input.strip_prefix('#').unwrap_or(input);
    if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if input.contains("://") {
        return true;
    }
    parse_key(input).is_some_and(|key| {
        prefix.is_empty()
            || key.split_once('-').is_some_and(|(p, _)| p.eq_ignore_ascii_case(prefix))
    })
}

/// Moves the Add form's focus to its text field, the Ticket or Name: what's typed or pasted on
/// one of its choices is meant for it, and a choice would drop it.
pub(super) fn focus_text(form: &mut Form) {
    if let Some(i) = form.fields.iter().position(|f| matches!(f.label, "Ticket" | "Name")) {
        form.focus = i;
    }
}

fn is_key(s: &str) -> bool {
    let Some((proj, num)) = s.split_once('-') else { return false };
    !proj.is_empty()
        && proj.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && proj.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit())
}

impl App {
    pub(super) fn open_ticket_form(&mut self, project: Option<usize>) {
        if self.projects.is_empty() {
            self.open_settings(SettingsItem::NewProject, true);
            return;
        }
        let names: Vec<String> = self.projects.iter().map(|p| p.name.clone()).collect();
        let current = names[project.unwrap_or(0).min(names.len() - 1)].clone();
        let kinds = vec!["Ticket".to_string(), "Agent".to_string()];
        let mut form = Form::new(
            FormKind::Ticket,
            "Add",
            vec![
                Field::choice("Project", names, current, "←/→ to change"),
                Field::choice(
                    "Type",
                    kinds,
                    "Agent",
                    "a ticket's session, or an agent with no ticket",
                ),
                Field::choice(
                    "CLI",
                    Cli::ALL.iter().map(|c| c.label().to_string()).collect(),
                    Cli::Claude.label(),
                    "",
                ),
                Field::choice("Worktree", vec![], "", ""),
                Field::choice("Base", vec![], "", ""),
                // Last, with its open issues under it: a long list has nothing below it to get
                // past.
                Field::text("Ticket", "", ""),
            ],
        );
        form.focus_on("Type");
        self.form_worktrees.replace(None);
        self.list_open_issues(project.unwrap_or(0).min(self.projects.len() - 1));
        self.update_ticket_hint(&mut form);
        self.update_worktree_field(&mut form, true);
        self.mode = Mode::Form(Box::new(form));
    }

    /// The key the Add form's ticket field names in project `p`: a bare number gets its prefix.
    /// In a Local files project, a title names the ticket it would create; elsewhere `#12` (on
    /// GitHub Issues), an issue's link, or words of one open issue's title name it.
    fn ticket_key(&self, p: usize, input: &str) -> Option<String> {
        if let Some((board, prefix)) = self.local_board(p) {
            return match local_input(input, &prefix)? {
                LocalInput::Key(key) => Some(key),
                LocalInput::Title => {
                    Some(local::next_key(&self.trackers.board_files(&board), &prefix))
                }
            };
        }
        if let Some(r) = self.tracker_input(p, input) {
            return r.ok();
        }
        let prefix = self.projects[p].tracker.as_ref().map(|t| t.key.clone()).unwrap_or_default();
        if !prefix.is_empty() && !input.is_empty() && input.chars().all(|c| c.is_ascii_digit()) {
            return Some(format!("{}-{input}", prefix.to_uppercase()));
        }
        parse_key(input)
    }

    /// What the ticket field names when it's written the tracker's own way (`#12`, an issue's
    /// link) or as words of an open issue's title, or why it names nothing; None when it reads as
    /// any tracker's number or key.
    fn tracker_input(&self, p: usize, input: &str) -> Option<Result<String, String>> {
        let t = self.projects[p].tracker.as_ref()?;
        if let Some(r) = t.key_from_input(input) {
            return Some(r);
        }
        let s = input.trim();
        let words =
            !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()) && parse_key(s).is_none();
        if !t.lists_open() || !words {
            return None;
        }
        Some(match self.open_issues.get(&self.projects[p].repo).map(|(_, l)| l) {
            Some(OpenIssues::Listed(list)) => match tracker::matching(list, s)[..] {
                [one] => Ok(one.key.clone()),
                [] => Err(format!("No open issue's title has \"{s}\"; type the issue's number")),
                ref many => Err(format!(
                    "{} open issues match; type more of the title, or the number",
                    many.len()
                )),
            },
            Some(OpenIssues::Failed(_)) => {
                Err("Couldn't list the open issues; type the issue's number".into())
            }
            Some(OpenIssues::Loading(_)) | None => {
                Err("Still listing the open issues; type the number, or wait a moment".into())
            }
        })
    }

    /// Lists project `p`'s open issues in the background for the Add form, when its tracker can
    /// and the last list is older than `OPEN_ISSUES_FRESH`; a board, read from the disk, every
    /// time.
    pub(super) fn list_open_issues(&mut self, p: usize) {
        let Some(proj) = self.projects.get(p) else { return };
        let Some(t) = proj.tracker.as_ref().filter(|t| t.lists_open()) else { return };
        let Access::Client(client) = self.trackers.access(&self.config, Some(proj)) else {
            return;
        };
        if let Some((at, list)) = self.open_issues.get(&proj.repo)
            && (matches!(list, OpenIssues::Loading(_))
                || (client.remote() && at.elapsed() < OPEN_ISSUES_FRESH))
        {
            return;
        }
        let prefix = t.key.clone();
        let Some(listing) = self.start_listing(p) else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let res = client.open_issues(&prefix);
            let _ = tx.send(Msg::OpenIssues(listing, res));
        });
    }

    /// Project `p`'s open issues are being listed: the request its folder now waits on.
    pub(crate) fn start_listing(&mut self, p: usize) -> Option<Listing> {
        let proj = &self.projects[p];
        let t = proj.tracker.as_ref()?;
        self.fetches += 1;
        let listing = Listing { id: self.fetches, repo: proj.repo.clone(), tracker: t.clone() };
        let loading = (Instant::now(), OpenIssues::Loading(listing.id));
        self.open_issues.insert(listing.repo.clone(), loading);
        Some(listing)
    }

    /// A project's open issues arrived: kept, and the Add form shows them if it's open. Dropped
    /// when they no longer answer what the project's folder waits on (`Listing`).
    pub(crate) fn open_issues_listed(
        &mut self,
        listing: Listing,
        res: Result<Vec<Listed>, String>,
    ) {
        let awaited = self.open_issues.get(&listing.repo).map(|(_, l)| l);
        let tracker =
            self.projects.iter().find(|p| p.repo == listing.repo).and_then(|p| p.tracker.as_ref());
        if !matches!(awaited, Some(OpenIssues::Loading(id)) if *id == listing.id)
            || tracker != Some(&listing.tracker)
        {
            return;
        }
        let list = match res {
            Ok(list) => OpenIssues::Listed(list),
            Err(e) => OpenIssues::Failed(e),
        };
        self.open_issues.insert(listing.repo, (Instant::now(), list));
        if let Mode::Form(form) = &mut self.mode
            && matches!(form.kind, FormKind::Ticket)
        {
            let mut form =
                std::mem::replace(form, Box::new(Form::new(FormKind::Ticket, "", vec![])));
            // A new list: the highlight would land on another issue.
            if let Some(f) = form.field_mut("Ticket") {
                (f.list.at, f.list.top) = (None, 0);
            }
            self.update_ticket_hint(&mut form);
            self.update_worktree_field(&mut form, false);
            self.mode = Mode::Form(form);
        }
    }

    /// The open issues the Ticket field's `input` matches in project `p`, assigned to you first;
    /// or the issue its key names; or why there are none.
    fn open_shown(&self, p: usize, input: &str) -> Shown<'_> {
        let list = match self.open_issues.get(&self.projects[p].repo).map(|(_, l)| l) {
            Some(OpenIssues::Listed(list)) => list,
            Some(OpenIssues::Failed(e)) => {
                return Shown::Note(vec![format!(
                    "Couldn't list the open issues: {}",
                    untrusted::line(e)
                )]);
            }
            Some(OpenIssues::Loading(_)) => {
                return Shown::Note(vec!["Listing the open issues…".into()]);
            }
            // Not asked for: the tracker's account has no token yet, say.
            None => return Shown::Note(Vec::new()),
        };
        let tracker = self.projects[p].tracker.as_ref();
        let written = |key: &str| tracker.map_or(key.to_string(), |t| t.provider.written_key(key));
        // On a board, the hint says what a key or title that lists nothing adds.
        let board = self.local_board(p).is_some();
        let s = input.trim();
        let digits = s.strip_prefix('#').unwrap_or(s);
        // Digits pick the issues whose number starts with them, as typed; a key or link names one
        // issue, open or not; anything else is words of a title.
        let typed = !digits.chars().all(|c| c.is_ascii_digit());
        if typed {
            let named = match tracker.and_then(|t| t.key_from_input(s)) {
                Some(Ok(key)) => Some(key),
                Some(Err(e)) => return Shown::Note(vec![e]),
                None => parse_key(s),
            };
            if let Some(key) = named {
                return match list.iter().find(|i| i.key.eq_ignore_ascii_case(&key)) {
                    Some(i) => Shown::Issues(vec![i]),
                    None if board => Shown::Note(Vec::new()),
                    None => Shown::Note(vec![format!(
                        "{} isn't an open issue; Enter adds it as it is",
                        written(&key)
                    )]),
                };
            }
        }
        let found = tracker::matching(list, s);
        if !found.is_empty() {
            return Shown::Issues(found);
        }
        Shown::Note(if list.is_empty() {
            vec!["No open issues".into()]
        } else if board {
            Vec::new()
        } else if typed {
            vec!["No open issue's title has those words".into()]
        } else {
            vec![format!("No open issue's number starts with {digits}; Enter adds it as it is")]
        })
    }

    /// Fills the list under the Ticket field for its `input` in project `p`: a window of the open
    /// issues it matches around the highlight, and how many more there are above and below it.
    fn fill_open_list(&self, p: usize, input: &str, list: &mut List) {
        let found = match self.open_shown(p, input) {
            Shown::Issues(found) => found,
            Shown::Note(lines) => {
                *list = List { lines, ..List::default() };
                return;
            }
        };
        let provider = self.projects[p].tracker.as_ref().map(|t| t.provider);
        let line = |i: &Listed| {
            let key = provider.map_or(i.key.clone(), |pr| pr.written_key(&i.key));
            let mine = if i.mine { " · assigned to you" } else { "" };
            format!("{key} {}{mine}", untrusted::line(&i.title))
        };
        list.picks = found.len();
        list.at = list.at.filter(|&a| a < found.len());
        list.scroll(OPEN_ISSUES_SHOWN);
        let window = &found[list.top..(list.top + OPEN_ISSUES_SHOWN).min(found.len())];
        list.lines = window.iter().map(|i| line(i)).collect();
        if found.len() > OPEN_ISSUES_SHOWN {
            let below = found.len() - list.top - window.len();
            let more = [("↑", list.top), ("↓", below)]
                .iter()
                .filter(|(_, n)| *n > 0)
                .map(|(arrow, n)| format!("{arrow} {n} more"))
                .collect::<Vec<_>>()
                .join(" · ");
            list.lines.push(format!("{more}; type to narrow"));
        }
    }

    /// The key of the open issue highlighted under the Add form's Ticket field, in project `p`.
    fn picked(&self, p: usize, form: &Form) -> Option<String> {
        let f = form.field("Ticket")?;
        match self.open_shown(p, &f.value) {
            Shown::Issues(found) => found.get(f.list.at?).map(|i| i.key.clone()),
            Shown::Note(_) => None,
        }
    }

    /// The key the Add form names in project `p`: the highlighted issue, or what's typed.
    fn form_ticket_key(&self, p: usize, form: &Form) -> Option<String> {
        self.picked(p, form).or_else(|| self.ticket_key(p, &form.value("Ticket")))
    }

    /// Project `p`'s Local files board and ticket prefix, if that's its tracker.
    fn local_board(&self, p: usize) -> Option<(PathBuf, String)> {
        let t = self.projects[p].tracker.as_ref()?;
        Some((t.board(&self.projects[p].repo)?, t.key.to_uppercase()))
    }

    /// The Add form's ticket hint in a Local files project: the file a key names, or the ticket a
    /// title creates.
    fn local_ticket_hint(&self, board: &Path, prefix: &str, input: &str) -> String {
        let Some(pick) = local_input(input, prefix) else {
            return format!(
                "a number ({prefix}-3) or key on the board, or a title for a new ticket"
            );
        };
        let files = self.trackers.board_files(board);
        match pick {
            LocalInput::Key(key) => match local::find_in(&files, &key, board) {
                Ok(f) => format!("{} in {}", local::rel(board, &f.path), f.column),
                Err(e) => e,
            },
            LocalInput::Title => {
                format!("new: {} in todo, titled as typed", local::next_key(&files, prefix))
            }
        }
    }

    /// The Add form's Worktree field: create one for the ticket or not, with where it would go;
    /// or the worktree the ticket already has. `reset` sets the choice to the project's default.
    pub(super) fn update_worktree_field(&self, form: &mut Form, reset: bool) {
        self.update_worktree_choice(form, reset);
        self.update_base_field(form);
    }

    /// The Add form's Base field: the default branch, or another ticket of the project whose
    /// branch the new worktree starts from. Only when the form creates a worktree.
    fn update_base_field(&self, form: &mut Form) {
        let Some(i) = form.fields.iter().position(|f| f.label == "Base") else { return };
        let Some(p) = self.projects.iter().position(|p| p.name == form.value("Project")) else {
            return;
        };
        let creating = form.value("Worktree") == CREATE;
        let key = self.form_ticket_key(p, form);
        let f = &mut form.fields[i];
        if !creating {
            (f.choices, f.value) = (vec!["—".into()], "—".into());
            f.hint = "only when creating a worktree".into();
            return;
        }
        let default = self.default_branch(&self.projects[p].repo);
        let bases = self.base_candidates(p, key.as_deref());
        f.choices =
            std::iter::once(default.clone()).chain(bases.iter().map(|b| b.key.clone())).collect();
        if !f.choices.contains(&f.value) {
            f.value = default.clone();
        }
        f.hint = match bases.iter().find(|b| b.key == f.value) {
            Some(b) => format!("stacked: starts from {}'s branch {}", b.key, b.branch),
            None if bases.is_empty() => {
                format!("from origin/{default}; stacking needs another ticket with a branch")
            }
            None => format!("from origin/{default}; ←/→ to stack on another ticket"),
        };
    }

    fn update_worktree_choice(&self, form: &mut Form, reset: bool) {
        let Some(i) = form.fields.iter().position(|f| f.label == "Worktree") else { return };
        let Some(p) = self.projects.iter().position(|p| p.name == form.value("Project")) else {
            return;
        };
        let proj = &self.projects[p];
        if self.gh(&proj.repo) == github::Gh::NotGit {
            let f = &mut form.fields[i];
            (f.choices, f.value) = (vec![NO_WORKTREE.into()], NO_WORKTREE.into());
            f.hint = "the folder isn't a git repository: the session runs in it".into();
            return;
        }
        let key = self.form_ticket_key(p, form);
        let existing = key.as_ref().and_then(|k| {
            let mut cache = self.form_worktrees.borrow_mut();
            if cache.as_ref().is_none_or(|(repo, _)| *repo != proj.repo) {
                *cache =
                    Some((proj.repo.clone(), github::worktrees(&proj.repo).unwrap_or_default()));
            }
            github::pick_worktree(&cache.as_ref()?.1, k)
        });
        let f = &mut form.fields[i];
        if let Some((path, branch)) = existing {
            (f.choices, f.value) = (vec!["Existing".into()], "Existing".into());
            f.hint = format!("{} · branch {branch}", store::tilde(&path));
            return;
        }
        let keep = !reset && (f.value == CREATE || f.value == NO_WORKTREE);
        f.choices = vec![CREATE.into(), NO_WORKTREE.into()];
        if !keep {
            f.value = if proj.worktrees.creates() { CREATE } else { NO_WORKTREE }.into();
        }
        f.hint = if f.value == CREATE {
            let key = key.unwrap_or_else(|| example_key(proj.tracker.as_ref()));
            let folder = worktree::folder_path(proj.worktrees.folder_format(), &proj.repo, &key);
            let provider = proj.tracker.as_ref().map(|t| t.provider);
            format!(
                "{} on {}",
                store::tilde(&folder),
                branch_preview(proj.worktrees.branch_format(), provider, &key)
            )
        } else {
            "the session runs in the main checkout".into()
        };
    }

    /// After typing or pasting into an agent's Name: a name that names one of the project's
    /// tickets switches Type to Ticket, keeping what was typed. Only then, not on every change:
    /// ←/→ back to Agent would otherwise be undone at once.
    pub(super) fn name_typed(&self, form: &mut Form) {
        let project = self.projects.iter().find(|p| p.name == form.value("Project"));
        let Some(tracker) = project.and_then(|p| p.tracker.as_ref()) else { return };
        if form.value("Type") == "Agent" && names_ticket(&tracker.key, &form.value("Name")) {
            form.set("Type", "Ticket");
        }
    }

    /// A paste into the Add form, which lands in its text field wherever the focus is.
    pub(super) fn paste_into_add_form(&mut self, text: &str) {
        let Mode::Form(form) = &mut self.mode else { return };
        let mut f = std::mem::replace(form, Box::new(Form::new(FormKind::Ticket, "", vec![])));
        if !f.fields[f.focus].is_text() {
            focus_text(&mut f);
        }
        let name = f.field("Name").map(|n| n.value.clone());
        super::forms::paste_into(&mut f, text);
        if f.field("Name").map(|n| &n.value) != name.as_ref() {
            self.name_typed(&mut f);
        }
        self.update_ticket_hint(&mut f);
        self.update_worktree_field(&mut f, false);
        self.mode = Mode::Form(f);
    }

    pub(super) fn update_ticket_hint(&self, form: &mut Form) {
        let codex = Cli::from_label(&form.value("CLI")) == Cli::Codex;
        if let Some(f) = form.field_mut("CLI") {
            f.hint = if codex {
                "works alone: can't message or be messaged"
            } else {
                "the coding CLI the session runs"
            }
            .into();
        }
        // Said once, the first time someone picks Codex.
        form.note = (codex && !self.prefs.codex_explained).then(|| {
            "Keep a project's tickets on one CLI; use the other for one-offs, like a review.".into()
        });
        // Tickets come from a tracker: without one, a project has agents only.
        let tracker = self
            .projects
            .iter()
            .find(|p| p.name == form.value("Project"))
            .is_some_and(|p| p.tracker.is_some());
        let kinds: Vec<String> =
            if tracker { vec!["Ticket".into(), "Agent".into()] } else { vec!["Agent".into()] };
        if let Some(f) = form.field_mut("Type") {
            if !kinds.contains(&f.value) {
                f.value = kinds[0].clone();
            }
            f.hint = if tracker {
                "a ticket's session, or an agent with no ticket"
            } else {
                "agents only: connect a tracker in Settings for tickets"
            }
            .into();
            f.choices = kinds;
        }
        // Agents run in the main checkout, so the Worktree and Base rows are the agent's role
        // instead; the Ticket or Name field stays last.
        let agent = form.value("Type") == "Agent";
        if agent != form.field("Role").is_some() {
            let focused = form.fields[form.focus].label;
            form.fields.retain(|f| !matches!(f.label, "Role" | "Worktree" | "Base"));
            let at = form.fields.iter().position(|f| matches!(f.label, "Ticket" | "Name"));
            let at = at.unwrap_or(form.fields.len());
            if agent {
                let role = Field::text(
                    "Role",
                    "",
                    "optional: a standing instruction for its brief, e.g. review PRs",
                );
                form.fields.insert(at, Field { prose: true, ..role });
            } else {
                form.fields.splice(
                    at..at,
                    [
                        Field::choice("Worktree", vec![], "", ""),
                        Field::choice("Base", vec![], "", ""),
                    ],
                );
            }
            form.focus = form.focus.min(form.fields.len() - 1);
            form.focus_on(focused);
        }
        let p = self.projects.iter().position(|p| p.name == form.value("Project"));
        // The ticket's key, or the agent's name: one field, relabelled with the type.
        let lists = |p: &usize| self.projects[*p].tracker.as_ref().is_some_and(Tracker::lists_open);
        let listing = p.filter(|p| !agent && lists(p));
        let Some(f) = form.fields.iter_mut().find(|f| matches!(f.label, "Ticket" | "Name")) else {
            return;
        };
        match listing {
            Some(p) => self.fill_open_list(p, &f.value, &mut f.list),
            None => f.list = List::default(),
        }
        let picked = p.and_then(|p| self.picked(p, form));
        let Some(f) = form.fields.iter_mut().find(|f| matches!(f.label, "Ticket" | "Name")) else {
            return;
        };
        if agent {
            f.label = "Name";
            f.hint = "short names fit the sidebar, e.g. oss-planner; no worktree".into();
            return;
        }
        f.label = "Ticket";
        if let Some((board, prefix)) = p.and_then(|p| self.local_board(p)) {
            f.hint = self.local_ticket_hint(&board, &prefix, picked.as_deref().unwrap_or(&f.value));
            return;
        }
        let tracker = p.and_then(|p| self.projects[p].tracker.as_ref());
        let key = tracker.map(|t| t.key.clone());
        if let Some(t) = tracker.filter(|_| listing.is_some()) {
            let example = format!("{}-12", t.key.to_uppercase());
            let written = t.provider.written_key(&example);
            f.hint = if written == example {
                format!("a number ({example}), a key, a link, or words from an open issue's title")
            } else {
                format!(
                    "a number ({example} or {written}), a link, or words from an open issue's title"
                )
            };
            return;
        }
        f.hint = match key {
            Some(k) if !k.is_empty() => format!("a number ({k}-52), a key, or a link"),
            _ => "a key like ENG-52, or a link".into(),
        };
    }

    pub(super) fn submit_ticket(&mut self, form: &Form) -> Result<(), String> {
        let Some(p) = self.projects.iter().position(|p| p.name == form.value("Project")) else {
            return Err("Pick a project".into());
        };
        let agent = form.value("Type") == "Agent";
        let input = form.value(if agent { "Name" } else { "Ticket" });
        let cli = Cli::from_label(&form.value("CLI"));
        if agent {
            let name = if input.is_empty() { "main" } else { &input };
            let role = Some(form.value("Role")).filter(|r| !r.is_empty());
            let new_role = if role.is_some() {
                format!("; its new role applies when its session restarts ({} R)", keys::prefix())
            } else {
                String::new()
            };
            if let Added::Open(_) = self.add_agent(p, name, cli, role, true)? {
                self.toast(format!(
                    "✦ {} is already open in {}; switched to it{new_role}",
                    store::slug(name),
                    self.projects[p].name
                ));
            }
            self.mode = Mode::Normal;
            return Ok(());
        }
        if self.projects[p].tracker.is_none() {
            return Err(self.projects[p].agents_only());
        }
        let key = match (self.picked(p, form), self.local_board(p)) {
            (Some(key), _) => key,
            (None, Some((board, prefix))) => local_ticket(&board, &prefix, &input)?,
            (None, None) => {
                if let Some(Err(e)) = self.tracker_input(p, &input) {
                    return Err(e);
                }
                self.ticket_key(p, &input)
                    .ok_or("Expected a ticket number, a key like ENG-52, or a link")?
            }
        };
        let create = form.value("Worktree") == CREATE;
        let base = match form.value("Base") {
            b if !create || b == "—" || b == self.default_branch(&self.projects[p].repo) => None,
            b => Some(self.base_named(p, &b)?),
        };
        if let Added::Open(_) = self.add_ticket(p, key.clone(), cli, create, base, true)? {
            self.toast(format!("{key} is already open; switched to it"));
        }
        self.mode = Mode::Normal;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::offline;
    use crate::store::Saved;

    #[test]
    fn a_number_link_or_the_projects_key_names_a_ticket_and_other_text_an_agent() {
        for input in ["52", "#52", " 52 ", "eng-52", "ENG-52", "https://example.com/t/12"] {
            assert!(names_ticket("ENG", input), "{input}");
        }
        for input in ["", "#", "lead", "oss-planner", "phase-2", "web-12", "52a"] {
            assert!(!names_ticket("ENG", input), "{input}");
        }
        // With no prefix, any key could be the project's.
        assert!(names_ticket("", "web-12"));
    }

    #[test]
    fn add_form_rows_follow_its_type() {
        let project = |name: &str, tracker: serde_json::Value| -> store::Project {
            serde_json::from_value(
                serde_json::json!({ "name": name, "repo": format!("/nonexistent/{name}"), "tracker": tracker }),
            )
            .unwrap()
        };
        let linear = serde_json::json!({ "provider": "linear", "site": "acme", "key": "ENG" });
        let saved = Saved {
            projects: vec![project("acme", linear), project("notes", serde_json::Value::Null)],
            ..Default::default()
        };
        let mut app = offline(saved);
        // No git: the form offers no worktree, so it runs no git.
        for p in &app.projects {
            app.github.insert(p.repo.clone(), crate::github::Gh::NotGit);
        }
        app.open_ticket_form(Some(0));
        let Mode::Form(mut form) = std::mem::replace(&mut app.mode, Mode::Normal) else {
            panic!("no form")
        };
        let labels = |f: &Form| f.fields.iter().map(|x| x.label).collect::<Vec<_>>();
        // It opens on Type, set to Agent: Worktree and Base give way to its role.
        assert_eq!(labels(&form), ["Project", "Type", "CLI", "Role", "Name"]);
        assert_eq!(form.fields[form.focus].label, "Type");
        form.set("Name", "reviewer");
        form.set("Type", "Ticket");
        app.update_ticket_hint(&mut form);
        assert_eq!(labels(&form), ["Project", "Type", "CLI", "Worktree", "Base", "Ticket"]);
        assert_eq!(form.value("Ticket"), "reviewer");
        form.set("Type", "Agent");
        app.update_ticket_hint(&mut form);
        assert_eq!(labels(&form), ["Project", "Type", "CLI", "Role", "Name"]);
        form.set("Type", "Ticket");
        app.update_ticket_hint(&mut form);
        assert_eq!(labels(&form), ["Project", "Type", "CLI", "Worktree", "Base", "Ticket"]);
        assert_eq!(form.field("CLI").unwrap().hint, "the coding CLI the session runs");
        // A project without a tracker has agents only.
        form.set("Project", "notes");
        app.update_ticket_hint(&mut form);
        assert_eq!(form.value("Type"), "Agent");
        assert_eq!(labels(&form), ["Project", "Type", "CLI", "Role", "Name"]);
    }

    #[test]
    fn local_add_takes_a_key_or_a_title() {
        assert_eq!(local_input("  ", "WP"), None);
        assert_eq!(local_input("3", "WP"), Some(LocalInput::Key("WP-3".into())));
        assert_eq!(local_input("wp-12", "WP"), Some(LocalInput::Key("WP-12".into())));
        assert_eq!(local_input("Fix the login form", "WP"), Some(LocalInput::Title));
        // A number with no prefix to put before it is a title too.
        assert_eq!(local_input("2026", ""), Some(LocalInput::Title));
        let dir = std::env::temp_dir().join(format!("taskman-add-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let board = local::board(&dir, "workplan");
        assert_eq!(local_ticket(&board, "WP", "Fix login"), Ok("WP-1".into()));
        assert_eq!(local_ticket(&board, "WP", "1"), Ok("WP-1".into()));
        assert!(local_ticket(&board, "WP", "WP-2").unwrap_err().starts_with("WP-2 not found"));
        assert!(local_ticket(&board, "WP", "").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn previews_branches_before_the_issue_is_fetched() {
        assert_eq!(
            branch_preview("{tracker}", Some(Provider::Linear), "ENG-52"),
            "Linear's branch name"
        );
        assert_eq!(branch_preview("{tracker}", Some(Provider::Jira), "ENG-52"), "ENG-52-…");
        assert_eq!(
            branch_preview("{tracker}", Some(Provider::Local), "WP-3"),
            "the ticket file's name"
        );
        assert_eq!(branch_preview("feature/{key}-{slug}", None, "ENG-52"), "feature/eng-52-…");
    }

    #[test]
    fn parses_keys_and_links() {
        assert_eq!(parse_key("eng-52").as_deref(), Some("ENG-52"));
        assert_eq!(
            parse_key("https://example.atlassian.net/browse/ENG-52").as_deref(),
            Some("ENG-52")
        );
        assert_eq!(
            parse_key("https://example.atlassian.net/jira/software/projects/ENG/boards/1?selectedIssue=ENG-7")
                .as_deref(),
            Some("ENG-7")
        );
        // What follows the key in a link isn't the ticket, even when it reads as a key.
        assert_eq!(
            parse_key("https://example.atlassian.net/browse/ENG-52?focusedCommentId=10#comment-10")
                .as_deref(),
            Some("ENG-52")
        );
        assert_eq!(
            parse_key("https://linear.app/acme/issue/ENG-52/phase-2").as_deref(),
            Some("ENG-52")
        );
        assert_eq!(parse_key("linear.app/acme/issue/eng-52/phase-2").as_deref(), Some("ENG-52"));
        // A workspace or host that reads as a key doesn't count either.
        assert_eq!(parse_key("https://linear.app/team-1/issue/ENG-52").as_deref(), Some("ENG-52"));
        assert_eq!(
            parse_key("https://eng-1.example.com/tickets/ENG-52").as_deref(),
            Some("ENG-52")
        );
        // Other links: the first key in the path, then in the query.
        assert_eq!(parse_key("https://example.com/t/ENG-52/split-2").as_deref(), Some("ENG-52"));
        assert_eq!(
            parse_key("https://example.com/view?id=ENG-52&page=2").as_deref(),
            Some("ENG-52")
        );
        // The fragment only when the path and query have no key.
        assert_eq!(
            parse_key("https://example.atlassian.net/browse/ENG-52#comment-10").as_deref(),
            Some("ENG-52")
        );
        assert_eq!(parse_key("https://example.com/#ENG-52").as_deref(), Some("ENG-52"));
        assert_eq!(parse_key("https://example.com/#/view?id=ENG-52").as_deref(), Some("ENG-52"));
        assert_eq!(parse_key("hello"), None);
        assert_eq!(parse_key("ENG-"), None);
    }
}
