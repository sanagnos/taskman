//! Projects in Settings: the new-project form and a project's settings (its tracker and account,
//! and how its tickets' worktrees are made), saving them, and adding, updating and deleting a
//! project.

use super::accounts::AfterAccount;
use super::add_form::{NO_WORKTREE, example_key};
use super::forms::Check;
use super::poll::remove_status_files;
use super::{App, Field, Form, FormKind, Remote, SettingsItem};
use crate::store::{self, Project, Provider, Tracker, WorktreePrefs};
use crate::tracker::Client;
use crate::{github, local, worktree};
use std::path::{Path, PathBuf};

/// The Account picker's entry for creating one.
const NEW_ACCOUNT: &str = "+ new account";

/// Tracker, Site, Ticket prefix and Account: the fields the add-project and settings forms share.
fn tracker_fields() -> Vec<Field> {
    let trackers = std::iter::once("None")
        .chain(Provider::ALL.map(Provider::label))
        .map(String::from)
        .collect();
    vec![
        Field::choice("Tracker", trackers, "None", ""),
        Field::text("Site", "", ""),
        Field::text(
            "Tickets folder",
            local::FOLDER,
            "in the project folder; each subfolder is a column, made if missing",
        ),
        Field::text("Ticket prefix", "", "ENG for ENG-12; then typing 12 adds it"),
        Field::choice("Account", vec![NEW_ACCOUNT.into()], NEW_ACCOUNT, ""),
    ]
}

/// Shows a project form's tracker fields once a tracker is picked, and its worktree settings only
/// when it has tickets to make them for (a tracker) in a `git` repository. Hidden fields keep their
/// values, so picking None and back loses nothing.
pub(super) fn show_project_rows(form: &mut Form, git: bool) {
    let provider = tracker_from_form(form).map(|t| t.provider);
    let tracker = provider.is_some();
    let board = provider.is_some_and(Provider::has_board);
    let remote = provider.is_some_and(Provider::site_from_remote);
    for f in &mut form.fields {
        f.hidden = match f.label {
            "Site" => !tracker || board || remote,
            "Account" => !provider.is_some_and(Provider::needs_account),
            "Tickets folder" => !board,
            "Ticket prefix" => !tracker,
            "New tickets" | "Branch" | "Folder" => !(tracker && git),
            _ => false,
        };
        if f.label == "Tracker" {
            f.hint = provider
                .map_or(
                    "none: agents only · ←/→ to connect a tracker (Jira, Linear, Local files, GitHub Issues) for tickets",
                    Provider::hint,
                )
                .into();
        }
        if f.label == "Site" {
            f.hint = provider
                .map_or(
                    "Jira: your-site.atlassian.net · Linear: the workspace",
                    Provider::site_hint,
                )
                .into();
        }
        if f.label == "Ticket prefix" {
            // Hidden without a tracker.
            f.hint = provider.unwrap_or(Provider::Jira).prefix_hint().into();
        }
    }
}

/// The tracker the form describes, without its account; None when Tracker is None.
pub(super) fn tracker_from_form(form: &Form) -> Option<Tracker> {
    let provider = Provider::ALL.into_iter().find(|p| p.label() == form.value("Tracker"))?;
    let (site, folder) = if provider.has_board() {
        (String::new(), Some(form.value("Tickets folder").trim_matches('/').to_string()))
    } else if provider.site_from_remote() {
        // Read from the project's remotes as it's saved (`github_site`).
        (String::new(), None)
    } else {
        (provider.site_from_input(form.value("Site")), None)
    };
    Some(Tracker {
        provider,
        site,
        folder,
        key: form.value("Ticket prefix").to_uppercase(),
        account: None,
    })
}

const BESIDE: &str = "Beside the repo";
const INSIDE: &str = "Inside, like Claude";
const CREATE_FOR_NEW: &str = "Create one";

/// Hints for the project settings' Worktrees fields: an example of each, or what's wrong.
pub(super) fn worktree_hints(form: &mut Form, repo: &Path) {
    let tracker = tracker_from_form(form);
    let key = example_key(tracker.as_ref());
    let provider = tracker.as_ref().map(|t| t.provider);
    let branch = form.value("Branch");
    let inside = form.value("Folder") == INSIDE;
    let branch_hint = match worktree::check_format(&branch) {
        Err(e) => e,
        Ok(()) => {
            let own_name = provider.map(|p| p.example_branch(&key)).unwrap_or_default();
            let example = worktree::branch_name(&branch, provider, &key, "Fix login", &own_name);
            let own = if branch.contains(worktree::TRACKER) {
                format!(" ({}'s own)", provider.map_or("tracker", |p| p.label()))
            } else {
                String::new()
            };
            format!("e.g. {example}{own} · ←/→ presets")
        }
    };
    let format = if inside { worktree::INSIDE } else { worktree::BESIDE };
    let ignored = if inside { ", ignored locally" } else { "" };
    let folder_hint =
        format!("e.g. {}{ignored}", store::tilde(&worktree::folder_path(format, repo, &key)));
    let new_hint = "the Add form's default; change it per ticket there".to_string();
    for (label, hint) in
        [("New tickets", new_hint), ("Branch", branch_hint), ("Folder", folder_hint)]
    {
        if let Some(f) = form.fields.iter_mut().find(|f| f.label == label) {
            f.hint = hint;
        }
    }
}

/// The Worktrees section of a project's settings, keeping only what differs from the defaults.
fn worktree_prefs_from_form(form: &Form) -> Result<WorktreePrefs, String> {
    let branch = form.value("Branch");
    worktree::check_format(&branch)?;
    let create = form.value("New tickets") != NO_WORKTREE;
    Ok(WorktreePrefs {
        create: (!create).then_some(false),
        branch: (branch != worktree::TRACKER).then_some(branch),
        inside: form.value("Folder") == INSIDE,
    })
}

fn is_prefix(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The repository a GitHub Issues tracker saves as its site: the one gh uses in the folder, read
/// from its remotes; or why there's none.
fn github_site(repo: &Path) -> Result<String, String> {
    match github::default_repo(repo) {
        Some(r) => Ok(crate::github_issues::Repo::site(&r.host, &r.name)),
        None if github::main_checkout(repo).is_none() => Err(
            "GitHub Issues needs a git repository with a GitHub remote, and this folder isn't a git repository; pick another tracker".into(),
        ),
        None => Err(
            "GitHub Issues reads the repository from the project's GitHub remote, and it has none; add one (git remote add origin URL) or pick another tracker".into(),
        ),
    }
}

/// The folder a project is for: the main checkout of the repository `path` is in, else `path`
/// itself when it's a folder.
fn project_folder(path: &Path) -> Option<PathBuf> {
    github::main_checkout(path).or_else(|| path.is_dir().then(|| crate::status::real(path)))
}

pub(super) fn expand_tilde(s: &str) -> PathBuf {
    match s.strip_prefix("~") {
        Some(rest) => store::home().join(rest.trim_start_matches('/')),
        None => PathBuf::from(s),
    }
}

impl App {
    pub(super) fn new_project_form(&self) -> Box<Form> {
        let repo = std::env::current_dir()
            .ok()
            .and_then(|d| github::main_checkout(&d))
            .filter(|r| !self.projects.iter().any(|p| &p.repo == r))
            .map(|p| store::tilde(&p))
            .unwrap_or_default();
        let mut fields =
            vec![Field::text("Path", repo, "the folder you work in: a repository, or any folder")];
        fields.extend(tracker_fields());
        let mut form = Form::new(FormKind::Project, "New project", fields);
        self.refresh_accounts(&mut form, None);
        show_project_rows(&mut form, false);
        Box::new(form)
    }

    pub(super) fn project_settings_form(&self, p: usize) -> Box<Form> {
        let proj = &self.projects[p];
        let mut fields = vec![Field::text("Name", proj.name.clone(), "shown in the sidebar")];
        fields.extend(tracker_fields());
        let wt = &proj.worktrees;
        let create = if wt.creates() { CREATE_FOR_NEW } else { NO_WORKTREE };
        fields.push(
            Field::choice(
                "New tickets",
                vec![CREATE_FOR_NEW.into(), NO_WORKTREE.into()],
                create,
                "",
            )
            .under("Worktrees"),
        );
        fields.push(Field::presets("Branch", &worktree::BRANCH_PRESETS, wt.branch_format()));
        let folder = if wt.inside { INSIDE } else { BESIDE };
        fields.push(Field::choice("Folder", vec![BESIDE.into(), INSIDE.into()], folder, ""));
        let mut form = Form::new(FormKind::ProjectSettings(p), "Project", fields);
        let n = self.tickets.iter().filter(|tv| tv.t.repo == proj.repo).count();
        let gh = self.gh(&proj.repo);
        let pr = match gh {
            // Read on every open, so a `gh repo set-default` since shows.
            github::Gh::Ready => match github::default_repo(&proj.repo) {
                Some(r) if r.several => {
                    format!(
                        "PR panel: on · {}, through gh\nseveral remotes; run gh repo set-default in the repo to change",
                        r.name
                    )
                }
                Some(r) => format!("PR panel: on · {}, through gh", r.name),
                None => "PR panel: on, through gh".to_string(),
            },
            gh => format!("No PR panel: {}", gh.why()),
        };
        form.note = Some(format!(
            "{} · {n} session{}\n{pr}",
            store::tilde(&proj.repo),
            if n == 1 { "" } else { "s" }
        ));
        let current = proj.tracker.clone();
        match &current {
            Some(t) => {
                form.set("Tracker", t.provider.label());
                if t.provider.has_board() {
                    form.set("Tickets folder", t.folder.clone().unwrap_or_default());
                } else {
                    form.set("Site", t.provider.site_for_input(&t.site));
                }
                form.set("Ticket prefix", t.key.clone());
            }
            None => form.set("Tracker", "None"),
        }
        self.refresh_accounts(&mut form, current.as_ref());
        show_project_rows(&mut form, gh != github::Gh::NotGit);
        worktree_hints(&mut form, &proj.repo);
        Box::new(form)
    }

    /// Fills the Account picker with the accounts for the chosen tracker, selecting the one
    /// `current` uses, or the one it would pick by default.
    pub(super) fn refresh_accounts(&self, form: &mut Form, current: Option<&Tracker>) {
        let Some(f) = form.fields.iter().position(|f| f.label == "Account") else { return };
        let Some(t) = tracker_from_form(form).filter(|t| t.provider.needs_account()) else {
            let f = &mut form.fields[f];
            f.choices = vec!["—".into()];
            f.ids = Vec::new();
            f.value = "—".into();
            f.hint = "no account needed".into();
            return;
        };
        let accounts: Vec<&store::Account> =
            self.config.accounts.iter().filter(|a| a.provider == t.provider).collect();
        let mut choices = store::account_labels(&accounts);
        choices.push(NEW_ACCOUNT.into());
        let ids: Vec<String> = accounts.iter().map(|a| a.id.clone()).collect();
        // Keep what was picked when the tracker changes back and forth, else pick the default.
        let keep = form.fields[f].chosen_id().map(str::to_string);
        let picked = current
            .filter(|c| c.provider == t.provider)
            .and_then(|c| self.config.account_for(c))
            .or_else(|| self.config.account_for(&t))
            .map(|i| self.config.accounts[i].id.clone());
        let value = keep
            .filter(|id| ids.contains(id))
            .or(picked)
            .and_then(|id| ids.iter().position(|i| *i == id))
            .map_or_else(|| NEW_ACCOUNT.to_string(), |i| choices[i].clone());
        let f = &mut form.fields[f];
        f.choices = choices;
        f.ids = ids;
        f.value = value;
        f.cut_middle = true;
        f.hint = "←/→ to pick, or + new account".into();
    }

    /// Adds a project (`existing` None) or saves the settings of project `existing`.
    pub(super) fn submit_project(
        &mut self,
        form: &mut Form,
        existing: Option<usize>,
    ) -> Result<(), String> {
        let (repo, name) = match existing {
            Some(p) => {
                let name = form.value("Name");
                if name.is_empty() {
                    return Err("Enter a name".into());
                }
                if self.projects.iter().enumerate().any(|(i, x)| i != p && x.name == name) {
                    return Err(format!("Another project is already called {name}"));
                }
                (self.projects[p].repo.clone(), name)
            }
            None => {
                let input = form.value("Path");
                if input.is_empty() {
                    return Err("Enter the folder's path".into());
                }
                let path = expand_tilde(&input);
                let Some(repo) = project_folder(&path) else {
                    return Err(format!("{} isn't a folder", path.display()));
                };
                if let Some(p) = self.projects.iter().find(|p| p.repo == repo) {
                    return Err(format!(
                        "{} is already a project ({})",
                        store::tilde(&repo),
                        p.name
                    ));
                }
                let mut name = store::project_name(&repo);
                if self.projects.iter().any(|p| p.name == name) {
                    name = format!("{name} ({})", store::tilde(repo.parent().unwrap_or(&repo)));
                }
                (repo, name)
            }
        };

        let worktrees = match existing {
            Some(_) => worktree_prefs_from_form(form)?,
            None => WorktreePrefs::default(),
        };

        let mut tracker = tracker_from_form(form);
        if let Some(t) = &mut tracker
            && t.provider.has_board()
        {
            if t.folder.as_ref().is_none_or(String::is_empty) {
                t.folder = Some(local::FOLDER.into());
            }
            if t.key.is_empty() {
                return Err(
                    "Enter a ticket prefix: new tickets are numbered after it, like WP-3".into()
                );
            }
        }
        if let Some(t) = &mut tracker
            && t.provider.site_from_remote()
        {
            t.site = github_site(&repo)?;
            if t.key.is_empty() {
                t.key = local::default_prefix(t.site.rsplit('/').next().unwrap_or_default());
            }
        }
        if let Some(t) = &mut tracker {
            if t.site.is_empty() && !t.provider.has_board() {
                return Err(format!("Enter the {} site", t.provider.label()));
            }
            if !t.key.is_empty() && !is_prefix(&t.key) {
                return Err(format!(
                    "{} isn't a ticket prefix: letters and digits, starting with a letter (like ENG)",
                    t.key
                ));
            }
            let picked =
                form.fields.iter().find(|f| f.label == "Account").and_then(Field::chosen_id);
            t.account = self
                .config
                .accounts
                .iter()
                .find(|a| a.provider == t.provider && Some(a.id.as_str()) == picked)
                .map(|a| a.id.clone());
            // Check the prefix names a real Jira project; a second Enter saves anyway.
            if t.provider.checks_prefix()
                && !t.key.is_empty()
                && !form.save_anyway
                && let Some(i) = t
                    .account
                    .as_ref()
                    .and_then(|id| self.config.accounts.iter().position(|a| &a.id == id))
                && let Some(token) = self.trackers.token(&self.config.accounts[i].token)
                && let Some(client) = Client::new(t, &repo, Some((&self.config.accounts[i], token)))
            {
                match form.checked.clone() {
                    Some(Check::Project(Ok(false))) => {
                        form.save_anyway = true;
                        return Err(format!(
                            "{} has no Jira project {}. It's the letters before the number in ticket keys (ENG in ENG-12). Fix it, or press Enter again to save anyway.",
                            store::site_host(&t.site),
                            t.key
                        ));
                    }
                    Some(Check::Project(_)) => {}
                    _ => {
                        let key = t.key.clone();
                        self.start_check(form, move |_| {
                            Check::Project(client.project_exists(&key))
                        });
                        return Ok(());
                    }
                }
            }
        }

        let collapsed = existing.is_some_and(|p| self.projects[p].collapsed);
        let closed_agents =
            existing.map(|p| self.projects[p].closed_agents.clone()).unwrap_or_default();
        let project =
            Project { name, repo, tracker: tracker.clone(), collapsed, closed_agents, worktrees };
        if let Some(board) = tracker.as_ref().and_then(|t| t.board(&project.repo)) {
            local::ensure_columns(&board)
                .map_err(|e| format!("Couldn't create {}: {e}", store::tilde(&board)))?;
        }
        match tracker {
            Some(t) if t.account.is_none() && t.provider.needs_account() => {
                let then = match existing {
                    Some(p) => AfterAccount::Update(p, Box::new(project)),
                    None => AfterAccount::Add(Box::new(project)),
                };
                let site = t.provider.site_for_input(&t.site);
                let mut form = self.new_account_form(t.provider, site, Some(then));
                form.note = Some(format!(
                    "Add the {} account this project should use.\n{}",
                    t.provider.label(),
                    form.note.take().unwrap_or_default()
                ));
                self.settings_show(SettingsItem::NewAccount, form, true);
            }
            _ => match existing {
                Some(p) => self.update_project(p, project),
                None => self.add_project(project),
            },
        }
        Ok(())
    }

    pub(super) fn add_project(&mut self, project: Project) {
        self.check_github(vec![project.repo.clone()]);
        self.projects.push(project);
        self.save();
        self.rebuild_clients();
        let p = self.projects.len() - 1;
        self.toast(format!("Added project {}", self.projects[p].name));
        self.open_ticket_form(Some(p));
    }

    pub(super) fn update_project(&mut self, p: usize, project: Project) {
        let tracker_changed = self.projects[p].tracker != project.tracker;
        if tracker_changed {
            self.open_issues.remove(&project.repo);
        }
        self.projects[p] = project;
        self.save();
        self.rebuild_clients();
        // Refetch the tracker panel of this project's tickets; drop what the old tracker showed.
        for tv in &mut self.tickets {
            if tv.t.repo == self.projects[p].repo {
                if tracker_changed {
                    tv.jira = Remote::new();
                }
                tv.jira.request(true);
            }
        }
        self.toast(format!("Saved {}", self.projects[p].name));
        self.settings_show(SettingsItem::Project(p), self.project_settings_form(p), false);
    }

    /// Removes a project and its tickets (their Claude sessions stop; worktrees and branches stay).
    /// Saved before the sessions stop and their claims go: while the state lists them, `taskman
    /// resume` finds them. Nothing changes when it can't be saved, and the toast says so.
    pub(super) fn delete_project(&mut self, p: usize) {
        let repo = self.projects[p].repo.clone();
        let sel = self.sel;
        let selected = self.tickets.get(self.sel).map(|tv| (tv.t.repo.clone(), tv.t.key.clone()));
        // Numbered, so a failed save can put them back in order.
        let (gone, kept): (Vec<_>, Vec<_>) =
            self.tickets.drain(..).enumerate().partition(|(_, tv)| tv.t.repo == repo);
        let places: Vec<usize> = kept.iter().map(|(n, _)| *n).collect();
        self.tickets = kept.into_iter().map(|(_, tv)| tv).collect();
        self.sel = selected
            .and_then(|(r, k)| self.tickets.iter().position(|tv| tv.t.is(&r, &k)))
            .unwrap_or(0)
            .min(self.tickets.len().saturating_sub(1));
        let project = self.projects.remove(p);
        if let Err(e) = self.try_save() {
            self.projects.insert(p, project);
            let mut all: Vec<_> =
                places.into_iter().zip(self.tickets.drain(..)).chain(gone).collect();
            all.sort_by_key(|(n, _)| *n);
            self.tickets = all.into_iter().map(|(_, tv)| tv).collect();
            self.sel = sel;
            return self.toast(format!("Kept project {}: {e}", self.projects[p].name));
        }
        // A list still loading for it is dropped when it lands (`Listing`).
        self.open_issues.remove(&repo);
        for (_, mut tv) in gone {
            tv.stop_session();
            tv.hold = None;
            remove_status_files(&tv.t);
            // Its CLI may still be stopping: a quit waits for it.
            self.keep_reaper((tv.t.repo.clone(), tv.t.key.clone()), tv.take_reaper());
        }
        self.rebuild_clients();
        self.toast(format!("Deleted project {}", project.name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_tracker_and_worktree_rows_only_when_they_apply() {
        let mut fields = super::tracker_fields();
        fields.push(Field::text("Branch", "{tracker}", ""));
        let mut form = Form::new(FormKind::ProjectSettings(0), "Project", fields);
        let shown =
            |f: &Form| f.fields.iter().filter(|x| !x.hidden).map(|x| x.label).collect::<Vec<_>>();
        super::show_project_rows(&mut form, true);
        assert_eq!(shown(&form), ["Tracker"]);
        form.set("Tracker", "Linear");
        super::show_project_rows(&mut form, true);
        assert_eq!(shown(&form), ["Tracker", "Site", "Ticket prefix", "Account", "Branch"]);
        // No worktrees outside git; hidden rows are skipped by Tab but keep their values.
        super::show_project_rows(&mut form, false);
        assert_eq!(shown(&form), ["Tracker", "Site", "Ticket prefix", "Account"]);
        form.focus_on("Account");
        assert_eq!(form.next_focus(1), 0);
        assert_eq!(form.value("Branch"), "{tracker}");
        // Local files: a folder instead of a site, and no account.
        form.set("Tracker", "Local files");
        super::show_project_rows(&mut form, true);
        assert_eq!(shown(&form), ["Tracker", "Tickets folder", "Ticket prefix", "Branch"]);
        let t = super::tracker_from_form(&form).unwrap();
        assert_eq!((t.provider, t.folder.as_deref()), (Provider::Local, Some("workplan")));
    }

    #[test]
    fn keeps_only_worktree_settings_that_differ() {
        let form = |create: &str, branch: &str, folder: &str| {
            let fields = vec![
                Field::text("New tickets", create, ""),
                Field::text("Branch", branch, ""),
                Field::text("Folder", folder, ""),
            ];
            Form::new(FormKind::Project, "", fields)
        };
        let prefs =
            |create, branch, folder| worktree_prefs_from_form(&form(create, branch, folder));
        // "Create one" is the default everywhere, so it isn't saved; "Don't create" is.
        assert_eq!(
            prefs("Create one", "{tracker}", "Beside the repo"),
            Ok(WorktreePrefs::default())
        );
        assert!(WorktreePrefs::default().creates());
        let off = prefs("Don't create", "{tracker}", "Beside the repo").unwrap();
        assert_eq!(off.create, Some(false));
        assert!(!off.creates());
        let custom = prefs("Don't create", "feature/{KEY}-{slug}", "Inside, like Claude").unwrap();
        assert_eq!((custom.branch.as_deref(), custom.inside), (Some("feature/{KEY}-{slug}"), true));
        assert!(prefs("Create one", "feature/{slug}", "Beside the repo").is_err());
    }
}
