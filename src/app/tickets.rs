//! Adding and closing tickets and agents: the Add form, the close dialog and `taskman add` and
//! `taskman close` all end here. Also stacking (the bases a ticket can start from, where a stacked
//! ticket stands) and renaming an agent.

use super::poll::{remove_log_at, remove_status_at, remove_status_files};
use super::sync::Msg;
use super::ticket::{Added, merged_head, own_worktree};
use super::{AgentState, App, Away, Field, Form, FormKind, Level, Mode, TicketView};
use crate::session::Reaper;
use crate::store::{self, Base, Cli, Ticket};
use crate::{close, events_log, github, keys, resume, stack, untrusted, util, worktree};
use std::path::{Path, PathBuf};

/// What closing a ticket leaves its caller (`close_at`): the claim on its session, for removing
/// its worktree, or why it couldn't be taken; and the reaper of its CLI if one was still running,
/// which the removal waits for before git deletes the folder the CLI ran in, and which is kept
/// otherwise (`keep_reaper`) so a quit waits for it.
pub(super) struct Closed {
    pub(super) hold: Result<resume::Hold, String>,
    pub(super) reaper: Option<Reaper>,
}

/// A ticket closed while git was still creating its worktree, until git is done.
pub(super) struct ClosedCreating {
    pub(super) key: String,
    /// The close asked to keep the worktree (`taskman close --keep-worktree`).
    pub(super) keep: bool,
    /// git's thread, which a quit waits for.
    pub(super) thread: Option<std::thread::JoinHandle<()>>,
    /// The thread had ended at the last check (`may_quit`).
    pub(super) ended: bool,
}

/// The toast for ticket `key`'s worktree, which git finished creating after the ticket closed.
fn closed_while_created(key: &str, w: &close::Worktree) -> String {
    let what = format!("{key} closed while its worktree was being created");
    match w {
        close::Worktree::Removed(done) => format!("{what}. {done}"),
        close::Worktree::Kept(path, why) => format!("{what}. Kept {}: {why}", store::tilde(path)),
        close::Worktree::Asked(path) | close::Worktree::NotOurs(path) => {
            format!("{what}. Kept {}", store::tilde(path))
        }
        close::Worktree::None => what,
    }
}

impl App {
    /// Where ticket `t` stands in its stack, when it's stacked. Whether the base merged is known
    /// while the base ticket is open; otherwise the PR's target tells.
    pub fn stack_of<'a>(&'a self, t: &'a Ticket) -> Option<(&'a Base, stack::State)> {
        let base = t.base.as_ref()?;
        let pr = |key: &str| {
            self.tickets.iter().find(|tv| tv.t.is(&t.repo, key)).and_then(|tv| tv.pr.data.as_ref())
        };
        let base_merged =
            pr(&base.key).map(|p| p.as_ref().is_some_and(|p| p.state == github::PrState::Merged));
        Some((base, stack::state(base, pr(&t.key).and_then(Option::as_ref), base_merged)))
    }

    /// Where the ticket's PR goes, for the sidebar: the PR's target when it isn't the default
    /// branch, by key when it's another ticket's branch, and true when that base has merged (the
    /// PR would land in a dead branch). Before a PR exists, a stacked ticket's base.
    pub fn pr_target(&self, tv: &TicketView) -> Option<(String, bool)> {
        let stack = self.stack_of(&tv.t);
        match tv.pr.data.as_ref().and_then(Option::as_ref) {
            Some(pr)
                if pr.state == github::PrState::Open
                    && pr.base != self.default_branch(&tv.t.repo) =>
            {
                let label = self
                    .tickets
                    .iter()
                    .find(|x| {
                        x.t.repo == tv.t.repo
                            && x.t.key != tv.t.key
                            && x.t.branch.as_deref() == Some(pr.base.as_str())
                    })
                    .map(|x| x.t.key.clone())
                    .or_else(|| {
                        tv.t.base.as_ref().filter(|b| b.branch == pr.base).map(|b| b.key.clone())
                    })
                    .unwrap_or_else(|| util::truncate(&untrusted::line(&pr.base), 14));
                Some((label, matches!(stack, Some((_, stack::State::Stale)))))
            }
            None => {
                stack.filter(|(_, s)| *s == stack::State::Open).map(|(b, _)| (b.key.clone(), false))
            }
            _ => None,
        }
    }

    /// The project's tickets another ticket can stack on: open, with a branch of their own.
    pub(super) fn base_candidates(&self, p: usize, except: Option<&str>) -> Vec<Base> {
        let repo = &self.projects[p].repo;
        self.tickets
            .iter()
            .filter(|tv| {
                tv.t.repo == *repo
                    && tv.t.agent.is_none()
                    && !tv.waiting()
                    && Some(tv.t.key.as_str()) != except
            })
            .filter_map(|tv| {
                Some(Base {
                    key: tv.t.key.clone(),
                    branch: tv.t.branch.clone()?,
                    extra: Default::default(),
                })
            })
            .collect()
    }

    /// The base named `key` in project `p`, or why a ticket can't stack on it.
    pub(super) fn base_named(&self, p: usize, key: &str) -> Result<Base, String> {
        if let Some(b) = self.base_candidates(p, None).into_iter().find(|b| b.key == key) {
            return Ok(b);
        }
        let proj = &self.projects[p];
        Err(match self.tickets.iter().find(|tv| tv.t.is(&proj.repo, key) && tv.t.agent.is_none()) {
            None => format!("{key} isn't open in {}; add it first", proj.name),
            Some(tv) if tv.creating => {
                format!("{key}'s worktree is still being created; try again in a moment")
            }
            Some(tv) if tv.waiting() => {
                format!("{key} has no worktree yet; retry it or choose the main checkout")
            }
            Some(_) => format!("{key} has no branch yet to stack on"),
        })
    }

    /// The project's default branch name, like `main`.
    pub fn default_branch(&self, repo: &Path) -> String {
        self.default_branch_of(repo).name
    }

    /// As `worktree::default_branch_of`, asked once a project.
    pub fn default_branch_of(&self, repo: &Path) -> worktree::DefaultBranch {
        self.default_branches
            .borrow_mut()
            .entry(repo.to_path_buf())
            .or_insert_with(|| worktree::default_branch_of(repo))
            .clone()
    }

    /// Adds ticket `key` to project `p`, creating its worktree when `create` and it has none,
    /// from `base`'s branch when given. `focus` selects it, as the Add form does; without it
    /// (`taskman add`) the selection stays and the sidebar marks the ticket new.
    /// Err while its worktree from an earlier close is still being removed.
    pub(super) fn add_ticket(
        &mut self,
        p: usize,
        key: String,
        cli: Cli,
        create: bool,
        base: Option<Base>,
        focus: bool,
    ) -> Result<Added, String> {
        let repo = self.projects[p].repo.clone();
        if let Some(i) = self.tickets.iter().position(|tv| tv.t.is(&repo, &key)) {
            if focus {
                self.select(i);
            }
            return Ok(Added::Open(i));
        }
        if resume::peek(&resume::owner_file(&repo, &key)) == resume::Owner::Closing {
            return Err(resume::closing(&key));
        }
        // What a hook of its last session wrote after the ticket closed (see `TicketView::takes`).
        remove_status_at(&store::status_file(&repo, &key));
        remove_log_at(&store::status_file(&repo, &key));
        let found = github::find_worktree(&repo, &key);
        let create = found.is_none() && create && self.gh(&repo) != github::Gh::NotGit;
        let (cwd, branch) = match found {
            Some((path, b)) => (path, Some(b)),
            None => (repo.clone(), None),
        };
        let mut tv = TicketView::new(Ticket {
            key,
            repo,
            cwd,
            cli,
            branch,
            session_id: None,
            seen_jira: None,
            seen_gh: None,
            agent: None,
            role: None,
            created_worktree: false,
            base: None,
            pending_worktree: None,
            fork: None,
            extra: Default::default(),
        });
        if create {
            tv.t.pending_worktree = Some(store::PendingWorktree { base, ..Default::default() });
        }
        // Closed a moment ago, its CLI may still be stopping: the new session waits for it.
        tv.reaper = self.take_reaper(&tv.t.repo, &tv.t.key);
        self.tickets.push(tv);
        if create {
            self.create_worktree(p, self.tickets.len() - 1);
        }
        self.prefs.codex_explained |= !cli.is_default();
        Ok(Added::New(self.show_added(p, focus)))
    }

    /// Shows the ticket or agent just added (the last one) in project `p`, and saves.
    fn show_added(&mut self, p: usize, focus: bool) -> usize {
        let i = self.tickets.len() - 1;
        self.projects[p].collapsed = false;
        if focus {
            if let Some(prev) = self.tickets.get_mut(self.sel) {
                prev.leave();
            }
            self.sel = i;
        } else {
            self.tickets[i].new = true;
        }
        self.save();
        i
    }

    /// Runs the selected ticket's session in the main checkout after its worktree failed, as the
    /// user chose.
    pub(super) fn use_main_checkout(&mut self) {
        let Some(tv) = self.tickets.get_mut(self.sel) else { return };
        if tv.creating || tv.t.pending_worktree.as_ref().is_none_or(|w| w.error.is_none()) {
            self.warn(format!(
                "{} M is for a ticket whose worktree couldn't be created",
                keys::prefix()
            ));
            return;
        }
        tv.t.pending_worktree = None;
        // One being made again in place of a worktree that's gone was in its own folder.
        tv.t.cwd = tv.t.repo.clone();
        tv.t.created_worktree = false;
        let msg = format!("{} runs in the main checkout {}", tv.t.key, store::tilde(&tv.t.repo));
        self.save();
        self.toast(msg);
    }

    /// Adds a project agent: a session in the repo's main checkout, with no ticket. `focus` as
    /// for `add_ticket`. A `role` replaces the agent's, open or closed; its session picks it up
    /// at its next start.
    pub(super) fn add_agent(
        &mut self,
        p: usize,
        name: &str,
        cli: Cli,
        role: Option<String>,
        focus: bool,
    ) -> Result<Added, String> {
        let name = store::slug(name);
        if name.is_empty() {
            return Err("Use letters or digits in the name".into());
        }
        let role = role.map(|r| untrusted::clean(&r));
        let repo = &self.projects[p].repo;
        if let Some(i) = self.tickets.iter().position(|tv| tv.t.is_agent(repo, &name)) {
            if role.is_some() {
                self.tickets[i].t.role = role;
                self.save();
            }
            if focus {
                self.select(i);
            }
            return Ok(Added::Open(i));
        }
        let key = store::agent_key(&self.projects[p].name, &name);
        // An agent added before the project was renamed can have it: agent web-x of project
        // acme is acme-web-x, and so is agent x once the project is acme-web.
        if self.tickets.iter().any(|tv| tv.t.is(repo, &key)) {
            return Err(format!(
                "✦ {key} is already a session name in this project: pick another name"
            ));
        }
        // An agent closed earlier under this name picks its conversation back up, if it ran in
        // the same CLI, and its role.
        let resumed = self.projects[p]
            .closed_agents
            .iter()
            .position(|c| c.name == name)
            .map(|j| self.projects[p].closed_agents.remove(j));
        let role = role.or_else(|| resumed.as_ref().and_then(|c| c.role.clone()));
        let session_id = resumed
            .filter(|c| c.cli == cli)
            .map(|c| c.session_id)
            .filter(|sid| !sid.is_empty() && cli.has_conversation(sid));
        if session_id.is_some() {
            self.toast(format!("Resuming ✦ {name}'s earlier conversation"));
        }
        let repo = self.projects[p].repo.clone();
        // What a hook of its last session wrote after the agent closed (see `TicketView::takes`).
        remove_status_at(&store::status_file(&repo, &key));
        remove_log_at(&store::status_file(&repo, &key));
        let t = Ticket {
            key,
            repo: repo.clone(),
            cwd: repo,
            cli,
            branch: None,
            session_id,
            seen_jira: None,
            seen_gh: None,
            agent: Some(name),
            role,
            created_worktree: false,
            base: None,
            pending_worktree: None,
            fork: None,
            extra: Default::default(),
        };
        self.prefs.codex_explained |= !cli.is_default();
        let mut tv = TicketView::new(t);
        // Closed a moment ago, its CLI may still be stopping, on the conversation this one
        // resumes: the new session waits for it.
        tv.reaper = self.take_reaper(&tv.t.repo, &tv.t.key);
        self.tickets.push(tv);
        Ok(Added::New(self.show_added(p, focus)))
    }

    /// The worktree git finished for ticket `key` after the ticket closed. No session ran in it, so
    /// it goes as the close dialog's `r` removes one, when the removal check finds nothing to lose.
    /// It stays when the close asked to keep it (`keep`), when git didn't make it now (the folder
    /// already was the branch's worktree: someone else's), or when the ticket's project was
    /// deleted, which keeps worktrees. Returns the toast, when it says so at once.
    pub(super) fn created_after_close(
        &mut self,
        repo: PathBuf,
        key: String,
        c: worktree::Created,
        keep: bool,
    ) -> Option<String> {
        let kept = |path: PathBuf| Some(closed_while_created(&key, &close::Worktree::Asked(path)));
        if keep || !c.made || !self.projects.iter().any(|p| p.repo == repo) {
            return kept(c.path);
        }
        // Held while git removes it, so the ticket can't be added back into the folder meanwhile;
        // without it, nothing would keep a new session out, so the folder stays.
        let hold = match resume::take(&resume::owner_file(&repo, &key)) {
            Ok(Ok(hold)) => hold.closing(),
            // Open again somewhere: whoever runs it has the folder now.
            Ok(Err(_)) => return kept(c.path),
            Err(e) => {
                let w = close::Worktree::Kept(c.path, close::unreserved(&e));
                return Some(closed_while_created(&key, &w));
            }
        };
        let (tx, name, path) = (self.tx.clone(), key.clone(), c.path);
        let removal = std::thread::spawn(move || {
            let w = close::remove_worktree(&repo, &path, None, &hold);
            drop(hold);
            let _ = tx.send(Msg::Toast(Level::Warn, closed_while_created(&key, &w)));
        });
        self.removing.push((vec![name], removal));
        None
    }

    /// Asks before closing the selected ticket, checking first whether the worktree taskman
    /// created for it could go too.
    pub(super) fn confirm_close(&mut self) {
        if let Some(e) = self.close_refusal(self.sel) {
            return self.warn(e);
        }
        // Not a worktree of taskman's, or one git couldn't check: the dialog offers nothing,
        // and says why when it's the latter, as `taskman close` does.
        let checked =
            self.tickets.get(self.sel).filter(|tv| own_worktree(tv)).map(|tv| {
                worktree::removal_check(&tv.t.repo, &tv.t.cwd, merged_head(tv).as_deref())
            });
        (self.close_check, self.close_unchecked) = match checked {
            Some(Ok(check)) => (check, None),
            Some(Err(e)) => (None, Some(e)),
            None => (None, None),
        };
        self.mode = Mode::ConfirmClose;
    }

    /// Why ticket `i` can't close: its session runs in another terminal (`taskman resume`), which
    /// would go on running there, in a worktree the close could remove.
    fn close_refusal(&self, i: usize) -> Option<String> {
        let tv = self.tickets.get(i)?;
        let owner = resume::peek(&resume::owner_file(&tv.t.repo, &tv.t.key));
        owner.elsewhere().then(|| close::elsewhere(tv.t.display()))
    }

    /// Closes ticket `i`: its session stops (its CLI is hung up and reaped, `Closed::reaper`)
    /// and it leaves the list; a closed agent is remembered so adding it again resumes it.
    /// Removing its worktree is the caller's, after this and after the reaper, holding the
    /// session's owner lock this returns until it's done: while it's held (`Owner::Closing`) the
    /// ticket can't be added again nor its session started, here or with `taskman resume`, so the
    /// removal never deletes a folder a new session runs in. `Closed::hold` is Err when the
    /// ticket closed but the lock couldn't be taken (the state folder can't be written, say),
    /// with why: the worktree must stay then, as nothing reserves the folder. Err, with nothing
    /// changed, while its session runs elsewhere (the dialog checks as it opens, but the session
    /// may have moved since), or when the state can't be saved: the ticket stays open, its session
    /// running and claimed, as `taskman resume` would still find it. The Err is `taskman close`'s
    /// outcome: `Elsewhere` or `Unsaved`.
    pub(super) fn close_at(&mut self, i: usize) -> Result<Closed, close::Outcome> {
        if i >= self.tickets.len() {
            return Ok(Closed { hold: Err("there's no such ticket".into()), reaper: None });
        }
        if self.close_refusal(i).is_some() {
            return Err(close::Outcome::Elsewhere);
        }
        let tv = &mut self.tickets[i];
        // Taken before anything changes; kept from the pane's session, else claimed now. A state
        // folder that can't be written doesn't stop tickets closing, only removing.
        let from_pane = tv.hold.is_some();
        let hold = match tv.hold.take() {
            Some(hold) => Ok(hold),
            None => match resume::take(&resume::owner_file(&tv.t.repo, &tv.t.key)) {
                Ok(Ok(hold)) => Ok(hold),
                Ok(Err(_)) => return Err(close::Outcome::Elsewhere),
                Err(e) => Err(close::unreserved(&e)),
            },
        };
        // Saved before anything that's hard to undo: until the state no longer lists it, `taskman
        // resume` finds it, so its session stays claimed while its CLI runs, and its worktree
        // stays.
        let tv = self.tickets.remove(i);
        let (sel, p) = (self.sel, self.projects.iter().position(|p| p.repo == tv.t.repo));
        let closed = p.map(|p| (p, self.projects[p].closed_agents.clone()));
        if let Some(name) = &tv.t.agent
            && (tv.t.session_id.is_some() || tv.t.role.is_some())
            && let Some(p) = p
        {
            let closed = &mut self.projects[p].closed_agents;
            closed.retain(|c| &c.name != name);
            let session_id = tv.t.session_id.clone().unwrap_or_default();
            closed.push(store::ClosedAgent {
                name: name.clone(),
                session_id,
                cli: tv.t.cli,
                role: tv.t.role.clone(),
                extra: Default::default(),
            });
        }
        if i < self.sel {
            self.sel -= 1;
        }
        self.sel = self.sel.min(self.tickets.len().saturating_sub(1));
        if let Err(e) = self.try_save() {
            if let Some((p, closed)) = closed {
                self.projects[p].closed_agents = closed;
            }
            self.tickets.insert(i, tv);
            self.sel = sel;
            if from_pane {
                self.tickets[i].hold = hold.ok();
            }
            return Err(close::Outcome::Unsaved(e));
        }
        let mut tv = tv;
        let hold = hold.map(resume::Hold::closing);
        tv.stop_session();
        let reaper = tv.take_reaper();
        remove_status_files(&tv.t);
        if tv.creating {
            let (key, thread) = (tv.t.key.clone(), tv.creation.take());
            let closed = ClosedCreating { key, keep: false, thread, ended: false };
            self.closed_creating.insert(tv.creation_id, closed);
        }
        self.selection = None;
        if self.mark_selected() {
            self.save();
        }
        Ok(Closed { hold, reaper })
    }

    pub(super) fn open_rename_form(&mut self) {
        let Some(tv) = self.tickets.get(self.sel) else { return };
        let Some(name) = tv.t.agent.clone() else {
            self.warn("Only agents can be renamed; a ticket is named by its key");
            return;
        };
        let mut form = Form::new(
            FormKind::RenameAgent(self.sel),
            "Rename agent",
            vec![Field::text(
                "Name",
                name,
                "letters, digits and dashes; short names fit the sidebar",
            )],
        );
        form.note = Some(format!(
            "Its {} session restarts on the same conversation under the new name.",
            tv.t.cli.label()
        ));
        self.mode = Mode::Form(Box::new(form));
    }

    /// Renames an agent. Claude only takes --name at startup (and the status file follows the
    /// name), so the session restarts and resumes its conversation under the new name; other
    /// sessions now reach it by that name. The new name is claimed before anything changes, and
    /// the claim kept for the restart: with the new key saved and nothing holding its owner lock,
    /// `taskman resume` could start a second copy of the conversation while the old CLI is still
    /// stopping. The rename is saved before the session stops and the old name's claim goes.
    /// Err, with nothing changed, when either name's claim can't be taken or the rename can't be
    /// saved.
    pub(super) fn rename_agent(&mut self, i: usize, input: &str) -> Result<(), String> {
        let name = store::slug(input);
        if name.is_empty() {
            return Err("Use letters or digits in the name".into());
        }
        let Some(p) = self.project_of(&self.tickets[i].t) else {
            return Err("This agent's project is gone".into());
        };
        if self.tickets[i].t.agent.as_deref() == Some(name.as_str()) {
            self.mode = Mode::Normal;
            return Ok(());
        }
        let repo = &self.projects[p].repo;
        if self.tickets.iter().any(|tv| tv.t.is_agent(repo, &name)) {
            return Err(format!("This project already has an agent called {name}"));
        }
        let key = store::agent_key(&self.projects[p].name, &name);
        if self.tickets.iter().any(|tv| tv.t.is(repo, &key)) {
            return Err(format!(
                "✦ {key} is already a session name in this project: pick another name"
            ));
        }
        if matches!(self.tickets[i].away, Some(Away::Outside | Away::Leaving(_))) {
            return Err(elsewhere());
        }
        let tv = &mut self.tickets[i];
        let old_path = resume::owner_file(&tv.t.repo, &tv.t.key);
        // The old name's claim: the pane's, or taken now, as another terminal may have started
        // the session since the last poll. Held until the rename is saved (see below).
        let from_pane = tv.hold.is_some();
        let old_hold = match tv.hold.take() {
            Some(hold) => hold,
            None => match resume::take(&old_path) {
                Ok(Ok(hold)) => hold,
                Ok(Err(_)) => return Err(elsewhere()),
                Err(e) => return Err(lock_failed(tv.t.display(), &e)),
            },
        };
        let new_hold = match resume::take(&resume::owner_file(&tv.t.repo, &key)) {
            Ok(Ok(hold)) => Ok(hold),
            Ok(Err(_)) => Err(close::elsewhere(&name)),
            Err(e) => Err(lock_failed(&name, &e)),
        };
        let new_hold = match new_hold {
            Ok(hold) => hold,
            Err(why) => {
                if from_pane {
                    tv.hold = Some(old_hold);
                }
                return Err(why);
            }
        };
        // Saved before anything that's hard to undo: until the new name is, `taskman resume`
        // finds the agent under the old one, which must stay claimed while its CLI runs.
        let closed = self.projects[p].closed_agents.clone();
        self.projects[p].closed_agents.retain(|c| c.name != name);
        let tv = &mut self.tickets[i];
        let old_t = tv.t.clone();
        let old = tv.t.agent.replace(name.clone()).unwrap_or_default();
        tv.t.key = key;
        if let Err(e) = self.try_save() {
            self.projects[p].closed_agents = closed;
            let tv = &mut self.tickets[i];
            tv.t = old_t;
            if from_pane {
                tv.hold = Some(old_hold);
            }
            return Err(format!("✦ {old} keeps its name: {e}"));
        }
        let tv = &mut self.tickets[i];
        // The new name's session starts once this one's CLI is gone (`start_session`), under
        // the claim taken above, which it keeps as a restart does.
        tv.stop_session();
        tv.hold = Some(new_hold.taskman());
        tv.away = None;
        tv.status.state = AgentState::NotStarted;
        self.mode = Mode::Normal;
        // The old name's claim goes now, not once the old CLI is reaped: it kept `taskman
        // resume` off the old name, and with the rename saved nothing can resume the old name
        // any more (the command finds sessions in the saved state, and checks again once it
        // holds the claim). The CLI still stopping is the renamed agent's, kept out of the pane
        // by its reaper and off `taskman resume` by the new name's claim.
        drop(old_hold);
        // Its log goes with it, under the new name.
        let log = |t: &Ticket| events_log::log_path(&store::status_file(&t.repo, &t.key));
        let _ = std::fs::rename(log(&old_t), log(&self.tickets[i].t));
        remove_status_files(&old_t);
        self.toast(format!("Renamed ✦ {old} to ✦ {name}"));
        Ok(())
    }
}

/// The toast for ticket `name`, which `close_at` left open as `o` says.
pub(super) fn left_open(name: &str, o: &close::Outcome) -> String {
    match o {
        close::Outcome::Unsaved(e) => close::unsaved(name, e),
        _ => close::elsewhere(name),
    }
}

/// Why an agent whose session runs in another terminal can't be renamed.
fn elsewhere() -> String {
    "It's running in another terminal: quit it there first".into()
}

/// Why the rename can't take `name`'s session lock: `e`, from `resume::take`.
fn lock_failed(name: &str, e: &std::io::Error) -> String {
    format!(
        "Couldn't take {name}'s session lock: {e}; check that {} can be written",
        store::tilde(&store::status_dir())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::offline;

    #[test]
    fn keeps_a_worktree_created_after_its_ticket_closed_unless_git_just_made_it_for_it() {
        let project: store::Project =
            serde_json::from_value(serde_json::json!({ "name": "acme", "repo": "/src/acme" }))
                .unwrap();
        let mut app = offline(store::Saved { projects: vec![project], ..Default::default() });
        let created = |made| worktree::Created {
            path: PathBuf::from("/src/acme-ENG-52"),
            branch: "ENG-52".into(),
            note: None,
            stacked: false,
            made,
        };
        let kept = "ENG-52 closed while its worktree was being created. Kept /src/acme-ENG-52";
        let repo = || PathBuf::from("/src/acme");
        // The folder already was the branch's worktree: someone else's, so it stays.
        assert_eq!(
            app.created_after_close(repo(), "ENG-52".into(), created(false), false).as_deref(),
            Some(kept)
        );
        // `--keep-worktree`.
        assert_eq!(
            app.created_after_close(repo(), "ENG-52".into(), created(true), true).as_deref(),
            Some(kept)
        );
        // Its project was deleted, which keeps worktrees.
        app.projects.clear();
        assert_eq!(
            app.created_after_close(repo(), "ENG-52".into(), created(true), false).as_deref(),
            Some(kept)
        );
        assert!(app.removing.is_empty(), "nothing to remove");
    }

    #[test]
    fn says_what_became_of_a_worktree_created_after_its_ticket_closed() {
        use crate::close::Worktree;
        let path = PathBuf::from("/src/acme-ENG-52");
        assert_eq!(
            closed_while_created(
                "ENG-52",
                &Worktree::Removed("Removed /src/acme-ENG-52 and branch ENG-52".into())
            ),
            "ENG-52 closed while its worktree was being created. Removed /src/acme-ENG-52 and branch ENG-52"
        );
        assert_eq!(
            closed_while_created(
                "ENG-52",
                &Worktree::Kept(path.clone(), "it has 1 unpushed commit".into())
            ),
            "ENG-52 closed while its worktree was being created. Kept /src/acme-ENG-52: it has 1 unpushed commit"
        );
        assert_eq!(
            closed_while_created("ENG-52", &Worktree::Asked(path)),
            "ENG-52 closed while its worktree was being created. Kept /src/acme-ENG-52"
        );
    }
}
