//! Projects in Settings: the new-project form and a project's settings (its tracker, the account
//! it signs in with, the project or team its tickets are in, and how its tickets' worktrees are
//! made), saving them, and adding, updating and deleting a project. Both forms sign in first: a new
//! account's fields are part of them, and once the account can sign in, the form lists what it can
//! see to pick the ticket prefix from (`Choices`).

use super::accounts::{account_defaults, not_saved, offers_file, stored_error, takes_pasted};
use super::add_form::{NO_WORKTREE, example_key};
use super::forms::{Check, List};
use super::poll::remove_status_files;
use super::sync::Msg;
use super::{App, Field, Form, FormKind, Mode, Remote, SettingsItem};
use crate::keychain::{self, Keychain};
use crate::store::{self, Account, Config, Project, Provider, Tracker, WorktreePrefs};
use crate::tracker::{self, Choice, Client};
use crate::{github, local, ticket_key, untrusted, worktree};
use std::path::{Path, PathBuf};

/// The Account picker's entry for creating one.
const NEW_ACCOUNT: &str = "+ new account";

/// The fields a sign-in is typed in: the list waits until focus leaves them, so it isn't asked
/// for on every key.
const SIGN_IN_FIELDS: [&str; 6] = ["Site", "Workspace", "Email", "Keep in", "Token", "Read from"];

/// Choices listed under the Project or Team field while it's focused.
const CHOICES_SHOWN: usize = 5;

/// What a project form's account can see to pick the ticket prefix from: Jira's projects or
/// Linear's teams, listed in the background once it can sign in.
pub(super) struct Choices {
    /// The sign-in they were listed with; any change to it drops them.
    asked: SignIn,
    /// Matches the listing's reply (`Msg::Choices`) to it, so a late one is dropped.
    id: u64,
    /// None while they're being listed.
    got: Option<Result<Vec<Choice>, String>>,
    /// The keys the field's list shows, in its order: what ↑/↓ highlights.
    shown: Vec<String>,
}

/// A project form's sign-in: its tracker's site and the account's credentials, saved or typed.
#[derive(Clone, PartialEq)]
struct SignIn {
    provider: Provider,
    /// As saved: Jira's `https://host`, Linear's workspace.
    site: String,
    email: String,
    /// Where the token is read from, or empty when it was pasted (`pasted`).
    source: String,
    /// A token pasted into the form, not in the keychain yet.
    pasted: Option<String>,
}

/// Tracker, its site, account and ticket prefix: the fields the add-project and settings forms
/// share. A new account's own fields are among them, shown while the account is "+ new account".
fn tracker_fields(app: &App) -> Vec<Field> {
    let trackers = std::iter::once("None")
        .chain(Provider::ALL.map(Provider::label))
        .map(String::from)
        .collect();
    let mut fields = vec![
        Field::choice("Tracker", trackers, "None", ""),
        Field::text("Site", "", ""),
        Field::text("Workspace", "", ""),
        Field::text(
            "Tickets folder",
            local::FOLDER,
            "in the project folder; each subfolder is a column, made if missing",
        ),
        Field::choice("Account", vec![NEW_ACCOUNT.into()], NEW_ACCOUNT, ""),
    ];
    fields.extend(app.new_account_fields());
    fields.extend(["Project", "Team", "Ticket prefix"].map(|label| Field::text(label, "", "")));
    fields
}

/// Shows a project form's tracker fields once a tracker is picked: its site, its account (with a
/// new one's own fields), and its prefix; and its worktree settings only when it has tickets to
/// make them for (a tracker) in a `git` repository. Hidden fields keep their values, so picking
/// None and back loses nothing.
pub(super) fn show_project_rows(
    form: &mut Form,
    git: bool,
    config: &Config,
    keychain: Option<&Keychain>,
) {
    let provider = tracker_from_form(form).map(|t| t.provider);
    let tracker = provider.is_some();
    let board = provider.is_some_and(Provider::has_board);
    let site_field = provider.and_then(Provider::site_field);
    let prefix_field = provider.map(Provider::prefix_field);
    let account = provider.is_some_and(Provider::needs_account);
    let new = account && form.field("Account").is_some_and(|f| f.chosen_id().is_none());
    let saved = form.field("Account").is_some_and(|f| f.choices.len() > 1);
    let file = offers_file(form);
    for f in &mut form.fields {
        f.hidden = match f.label {
            "Site" | "Workspace" => site_field != Some(f.label),
            "Account" => !account,
            "Email" | "Token" | "Read from" => !new,
            // One choice while the keychain check hasn't answered: nothing to pick.
            "Keep in" => !new || f.choices.len() < 2,
            "Tickets folder" => !board,
            "Project" | "Team" | "Ticket prefix" => prefix_field != Some(f.label),
            "New tickets" | "Branch" | "Folder" => !(tracker && git),
            _ => false,
        };
        match f.label {
            "Tracker" => {
                f.hint = provider
                    .map_or(
                        "none: agents only · ←/→ to connect a tracker (Jira, Linear, Local files, GitHub Issues) for tickets",
                        Provider::hint,
                    )
                    .into();
            }
            "Site" => f.hint = Provider::Jira.site_hint().into(),
            "Workspace" => f.hint = Provider::Linear.site_hint().into(),
            "Ticket prefix" => {
                // Hidden unless it's typed.
                f.hint = provider.unwrap_or(Provider::Local).prefix_hint().into();
            }
            "Account" if new && saved => {
                f.hint = format!("{} ←/→ for a saved account.", not_saved(file));
            }
            "Account" if new => f.hint = not_saved(file).into(),
            "Account" => f.hint = "←/→ to pick, or + new account".into(),
            _ => {}
        }
    }
    if let Some(p) = provider.filter(|_| new) {
        let site = form.value(site_field.unwrap_or("Site"));
        account_defaults(form, p, &site, config, keychain);
    }
}

/// The tracker the form describes, without its account; None when Tracker is None.
pub(super) fn tracker_from_form(form: &Form) -> Option<Tracker> {
    let provider = Provider::ALL.into_iter().find(|p| p.label() == form.value("Tracker"))?;
    let (site, folder) = if provider.has_board() {
        (String::new(), Some(form.value("Tickets folder").trim_matches('/').to_string()))
    } else if let Some(field) = provider.site_field() {
        (provider.site_from_input(form.value(field)), None)
    } else {
        // Read from the project's remotes as it's saved (`github_site`).
        (String::new(), None)
    };
    Some(Tracker {
        provider,
        site,
        folder,
        key: form.value(provider.prefix_field()).to_uppercase(),
        account: None,
        extra: Default::default(),
    })
}

/// The choices listed for the form's sign-in, once they're in; an empty list too.
fn listed(form: &Form) -> Option<&[Choice]> {
    match &form.choices.as_ref()?.got {
        Some(Ok(list)) => Some(list),
        _ => None,
    }
}

/// The choice the prefix field picks: the one highlighted in its list, the one whose key is typed,
/// or the only one what's typed matches.
fn picked(form: &Form, field: &Field) -> Option<String> {
    let list = listed(form)?;
    if let Some(at) = field.list.at {
        return form.choices.as_ref()?.shown.get(at).cloned();
    }
    let typed = field.value.trim();
    if typed.is_empty() || list.iter().any(|c| c.key.eq_ignore_ascii_case(typed)) {
        return None;
    }
    match tracker::choosing(list, typed)[..] {
        [one] => Some(one.key.clone()),
        _ => None,
    }
}

/// Puts the choice the prefix field at `i` picks into it, as its value, before the form is saved;
/// with `highlighted`, only one highlighted, as focus leaves it: leaving picks nothing by itself.
fn take_pick(form: &mut Form, i: usize, highlighted: bool) {
    if highlighted && form.fields[i].list.at.is_none() {
        return;
    }
    if let Some(key) = picked(form, &form.fields[i]) {
        let f = &mut form.fields[i];
        f.value = key;
        (f.list.at, f.list.top) = (None, 0);
    }
}

/// The prefix field's hint, and its list of what the account can see while it's focused: the
/// choices what's typed matches, with the highlighted one, or what the listing is doing.
/// `signs_in` says the form has a sign-in to list them with, before they're asked for.
fn choice_rows(form: &mut Form, signs_in: bool) {
    let Some(provider) = tracker_from_form(form).map(|t| t.provider) else { return };
    if !provider.lists_choices() {
        return;
    }
    let label = provider.prefix_field();
    let Some(i) = form.fields.iter().position(|f| f.label == label) else { return };
    let focused = form.focus == i;
    let what = format!("{}s", label.to_lowercase());
    let typed = form.fields[i].value.trim().to_uppercase();
    let example = "ENG for ENG-12";
    let mut list =
        List { at: form.fields[i].list.at, top: form.fields[i].list.top, ..List::default() };
    let mut shown = Vec::new();
    let hint = match form.choices.as_ref().map(|c| &c.got) {
        None if signs_in => format!("the {} its tickets are in ({example})", label.to_lowercase()),
        None => format!("listed once taskman can sign in; or type its key ({example})"),
        Some(None) => {
            list.lines = vec![format!("Listing the {what} this account can see…")];
            format!("the {} its tickets are in; or type its key ({example})", label.to_lowercase())
        }
        Some(Some(Err(e))) => format!(
            "Couldn't list the {what}: {}. Type its key ({example}) to save without the list, or fix the sign-in above.",
            untrusted::line(e)
        ),
        Some(Some(Ok(all))) if all.is_empty() => {
            format!("This account can't see any {what}: type its key ({example})")
        }
        Some(Some(Ok(all))) => {
            // A key that's listed shows them all, to pick another from.
            let exact = all.iter().any(|c| c.key == typed);
            let found = tracker::choosing(all, if exact { "" } else { &typed });
            shown = found.iter().map(|c| c.key.clone()).collect();
            if focused && !found.is_empty() {
                let key_w = found.iter().map(|c| c.key.chars().count()).max().unwrap_or(0);
                list.picks = found.len();
                list.at = list.at.filter(|&a| a < found.len());
                list.scroll(CHOICES_SHOWN);
                let window = &found[list.top..(list.top + CHOICES_SHOWN).min(found.len())];
                list.lines = window
                    .iter()
                    .map(|c| format!("{:key_w$}  {}", c.key, untrusted::line(&c.name)))
                    .collect();
                let below = found.len() - list.top - window.len();
                let more: Vec<String> = [("↑", list.top), ("↓", below)]
                    .iter()
                    .filter(|(_, n)| *n > 0)
                    .map(|(arrow, n)| format!("{arrow} {n} more"))
                    .collect();
                if !more.is_empty() {
                    list.lines.push(format!("{}; type to narrow", more.join(" · ")));
                }
            }
            match all.iter().find(|c| c.key == typed) {
                Some(c) if focused => format!("{} · ↑/↓ for another", untrusted::line(&c.name)),
                Some(c) => untrusted::line(&c.name),
                None if typed.is_empty() => format!(
                    "↑/↓ to pick one of the {} {what} this account can see, or type to narrow",
                    all.len()
                ),
                None if found.is_empty() => {
                    format!("none of the {what} this account can see; Enter saves {typed} as typed")
                }
                None => "↑/↓ to pick, or type to narrow; its key is the ticket prefix".into(),
            }
        }
    };
    if let Some(c) = form.choices.as_mut() {
        c.shown = shown;
    }
    let f = &mut form.fields[i];
    f.hint = hint;
    f.list = list;
}

/// Why the prefix typed isn't one the account can see, when the list says so; None when it's
/// listed, empty, or there's no list to go by.
fn unlisted(form: &Form, t: &Tracker) -> Option<String> {
    let all = listed(form).filter(|l| !l.is_empty())?;
    if t.key.is_empty() || all.iter().any(|c| c.key == t.key) {
        return None;
    }
    let what = format!("{}s", t.provider.prefix_field().to_lowercase());
    Some(format!(
        "{} isn't one of the {} {what} this account can see. Pick one from the list, or press Enter again to save anyway.",
        t.key,
        all.len()
    ))
}

/// A Jira site with no project whose key is the prefix.
fn no_project(t: &Tracker) -> String {
    format!(
        "{} has no {} project {}. It's the letters before the number in ticket keys (ENG in ENG-12). Fix it, or press Enter again to save anyway.",
        store::site_host(&t.site),
        t.provider.label(),
        t.key
    )
}

/// `Jira's`, but `Local files'`.
fn possessive(name: &str) -> String {
    if name.ends_with('s') { format!("{name}'") } else { format!("{name}'s") }
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
                format!(" ({} own)", possessive(provider.map_or("tracker", |p| p.label())))
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
        extra: Default::default(),
    })
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
        fields.extend(tracker_fields(self));
        let mut form = Form::new(FormKind::Project, "New project", fields);
        self.refresh_accounts(&mut form, None);
        show_project_rows(&mut form, false, &self.config, self.keychain.as_ref());
        Box::new(form)
    }

    pub(super) fn project_settings_form(&self, p: usize) -> Box<Form> {
        let proj = &self.projects[p];
        let mut fields = vec![Field::text("Name", proj.name.clone(), "shown in the sidebar")];
        fields.extend(tracker_fields(self));
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
                } else if let Some(field) = t.provider.site_field() {
                    form.set(field, t.provider.site_for_input(&t.site));
                }
                form.set(t.provider.prefix_field(), t.key.clone());
            }
            None => form.set("Tracker", "None"),
        }
        self.refresh_accounts(&mut form, current.as_ref());
        show_project_rows(
            &mut form,
            gh != github::Gh::NotGit,
            &self.config,
            self.keychain.as_ref(),
        );
        worktree_hints(&mut form, &proj.repo);
        let signs_in = self.sign_in(&form).is_some();
        choice_rows(&mut form, signs_in);
        Box::new(form)
    }

    /// The folder a project form is for, and the project's name: the one being edited, or the
    /// folder typed.
    fn form_folder(&self, form: &Form) -> (PathBuf, String) {
        match form.kind {
            FormKind::ProjectSettings(p) => {
                (self.projects[p].repo.clone(), self.projects[p].name.clone())
            }
            _ => {
                let path = expand_tilde(&form.value("Path"));
                let repo = project_folder(&path).unwrap_or(path);
                let name = store::project_name(&repo);
                (repo, name)
            }
        }
    }

    /// After a key in a project form, where `label` was focused before it and `before` is its
    /// place: the accounts for the tracker and site, a suggested prefix, the rows shown, and the
    /// list of what the account can see, asked for once it can sign in.
    pub(super) fn project_form_changed(&mut self, form: &mut Form, label: &str, before: usize) {
        // Leaving the prefix field takes what it picks.
        let prefix = tracker_from_form(form).map(|t| t.provider.prefix_field());
        if before != form.focus && form.fields.get(before).map(|f| f.label) == prefix {
            take_pick(form, before, true);
        }
        if matches!(label, "Path" | "Tracker" | "Site" | "Workspace") {
            self.refresh_accounts(form, None);
        }
        // A saved account picked brings its site, when none is typed yet.
        if label == "Account"
            && let Some(t) = tracker_from_form(form)
            && let Some(field) = t.provider.site_field()
            && form.value(field).is_empty()
            && let Some(id) = form.field("Account").and_then(Field::chosen_id)
            && let Some(a) = self.config.accounts.iter().find(|a| a.id == id)
        {
            form.set(field, t.provider.site_for_input(&a.site));
        }
        if matches!(label, "Tracker" | "Tickets folder") {
            self.suggest_prefix(form);
        }
        let git = match form.kind {
            FormKind::ProjectSettings(p) => self.gh(&self.projects[p].repo) != github::Gh::NotGit,
            _ => false,
        };
        show_project_rows(form, git, &self.config, self.keychain.as_ref());
        if let FormKind::ProjectSettings(p) = form.kind {
            worktree_hints(form, &self.projects[p].repo);
        }
        self.list_choices(form);
        let signs_in = self.sign_in(form).is_some();
        choice_rows(form, signs_in);
    }

    /// The active project form, once keys go to it: lists what its account can see, if it can
    /// sign in already (a saved account's).
    pub(super) fn project_form_opened(&mut self) {
        let form = match &mut self.mode {
            Mode::Settings(s) => &mut s.form,
            Mode::Form(f) => f,
            _ => return,
        };
        if !matches!(form.kind, FormKind::Project | FormKind::ProjectSettings(_)) {
            return;
        }
        let mut f = std::mem::replace(form, Box::new(Form::new(FormKind::Ticket, "", vec![])));
        self.list_choices(&mut f);
        let signs_in = self.sign_in(&f).is_some();
        choice_rows(&mut f, signs_in);
        match &mut self.mode {
            Mode::Settings(s) => s.form = f,
            Mode::Form(x) => *x = f,
            _ => {}
        }
    }

    /// Fills in the Ticket prefix of a tracker that has nothing to list it from, while it's empty
    /// or still the last suggestion: the prefix a board's tickets use, or one from the GitHub
    /// repository's or the project's name.
    fn suggest_prefix(&self, form: &mut Form) {
        let Some(t) = tracker_from_form(form).filter(|t| !t.provider.lists_choices()) else {
            return;
        };
        let typed = form.value("Ticket prefix");
        if !typed.is_empty() && form.suggested.as_deref() != Some(typed.as_str()) {
            return;
        }
        let (repo, name) = self.form_folder(form);
        let prefix = t.suggested_prefix(&repo).unwrap_or_else(|| local::default_prefix(&name));
        form.set("Ticket prefix", prefix.clone());
        form.suggested = Some(prefix);
    }

    /// The form's sign-in, once it has one: a site, and a saved account or a new one's
    /// credentials. None for a tracker with nothing to list.
    fn sign_in(&self, form: &Form) -> Option<SignIn> {
        let t = tracker_from_form(form).filter(|t| t.provider.lists_choices())?;
        if t.site.is_empty() {
            return None;
        }
        let (provider, site) = (t.provider, t.site);
        if let Some(id) = form.field("Account")?.chosen_id() {
            let a = self.config.accounts.iter().find(|a| a.id == id)?;
            let (email, source) = (a.email.clone(), a.token.clone());
            return Some(SignIn { provider, site, email, source, pasted: None });
        }
        let email =
            if provider.signs_in_with_email() { form.value("Email") } else { String::new() };
        if provider.signs_in_with_email() && email.is_empty() {
            return None;
        }
        let (source, pasted) = if takes_pasted(form) {
            match form.value("Token") {
                t if t.is_empty() => (form.stored.clone()?, None),
                t => {
                    keychain::check_token(&t).ok()?;
                    (String::new(), Some(t))
                }
            }
        } else {
            let source = store::env_source_from_shell(&form.value("Read from"));
            (store::is_token_source(&source).then_some(source)?, None)
        };
        Some(SignIn { provider, site, email, source, pasted })
    }

    /// Lists what the form's account can see in the background, once it can sign in and focus is
    /// off the fields it's typed in; drops a list for another sign-in.
    fn list_choices(&mut self, form: &mut Form) {
        let wanted = self.sign_in(form);
        if form.choices.as_ref().is_some_and(|c| Some(&c.asked) != wanted.as_ref()) {
            form.choices = None;
            if let Some(f) = form.fields.iter_mut().find(|f| f.list.picks > 0) {
                f.list = List::default();
            }
        }
        let Some(asked) = wanted else { return };
        if form.choices.is_some() || SIGN_IN_FIELDS.contains(&form.fields[form.focus].label) {
            return;
        }
        self.checks += 1;
        let id = self.checks;
        form.choices =
            Some(Box::new(Choices { asked: asked.clone(), id, got: None, shown: vec![] }));
        // A saved account's token the clients read already: no password manager asked again.
        let known =
            asked.pasted.clone().or_else(|| self.trackers.token(&asked.source).map(String::from));
        let k = self.keychain.clone().unwrap_or_else(Keychain::os);
        let what = "listing what the account can see".to_string();
        let work = move || {
            let SignIn { provider, site, email, source, .. } = asked;
            let a = Account {
                id: String::new(),
                provider,
                site: site.clone(),
                email,
                token: source,
                extra: Default::default(),
            };
            let token = match known {
                Some(token) => Ok(token),
                // The keychain the form saves to: a unit test's stub.
                None => match keychain::name_of(&a.token) {
                    Some(name) => k.read(name),
                    None => a.resolve_token(),
                },
            };
            let t = Tracker {
                provider,
                site,
                folder: None,
                key: String::new(),
                account: None,
                extra: Default::default(),
            };
            let got = token.and_then(|token| {
                Client::new(&t, Path::new(""), Some((&a, &token)))
                    .and_then(|c| c.choices())
                    .unwrap_or_else(|| Ok(Vec::new()))
            });
            Msg::Choices(id, got)
        };
        self.reporter().spawn(what, work, move |e| Msg::Choices(id, Err(e)));
    }

    /// What the form's account can see came in: the prefix field lists it, and the only one is
    /// picked when none is typed.
    pub(super) fn choices_listed(&mut self, id: u64, got: Result<Vec<Choice>, String>) {
        let form = match &mut self.mode {
            Mode::Settings(s) => &mut s.form,
            Mode::Form(f) => f,
            _ => return,
        };
        let Some(c) = form.choices.as_mut().filter(|c| c.id == id) else { return };
        let field = c.asked.provider.prefix_field();
        if let Ok([only]) = got.as_deref()
            && form.value(field).is_empty()
        {
            let key = only.key.clone();
            form.set(field, key);
        }
        if let Some(c) = form.choices.as_mut() {
            c.got = Some(got);
        }
        choice_rows(form, true);
    }

    /// Fills the Account picker with the accounts for the chosen tracker, selecting the one
    /// `current` uses, else one that fits its site, else the one it would pick by default.
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
        // Keep what was picked when the tracker changes back and forth, unless another account
        // fits the site; a Linear key only fits its own workspace.
        let fits = accounts.iter().find(|a| a.matches(&t)).map(|a| a.id.clone());
        let keep = form.fields[f].chosen_id().map(str::to_string).filter(|id| {
            accounts.iter().any(|a| {
                &a.id == id
                    && (a.matches(&t) || (t.provider.account_spans_sites() && fits.is_none()))
            })
        });
        let picked = current
            .filter(|c| c.provider == t.provider)
            .and_then(|c| self.config.account_for(c))
            .map(|i| self.config.accounts[i].id.clone())
            .or(keep)
            .or(fits)
            .or_else(|| self.config.account_for(&t).map(|i| self.config.accounts[i].id.clone()));
        let value = picked
            .and_then(|id| ids.iter().position(|i| *i == id))
            .map_or_else(|| NEW_ACCOUNT.to_string(), |i| choices[i].clone());
        let f = &mut form.fields[f];
        f.choices = choices;
        f.ids = ids;
        f.value = value;
        f.cut_middle = true;
    }

    /// Adds a project (`existing` None) or saves the settings of project `existing`. A new account
    /// filled in on the form is checked and saved with it.
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
            Some(p) => WorktreePrefs {
                extra: self.projects[p].worktrees.extra.clone(),
                ..worktree_prefs_from_form(form)?
            },
            None => WorktreePrefs::default(),
        };

        if let Some(p) = tracker_from_form(form).map(|t| t.provider)
            && let Some(i) = form.fields.iter().position(|f| f.label == p.prefix_field())
        {
            take_pick(form, i, false);
        }
        let mut tracker = tracker_from_form(form);
        // What a newer taskman saved about the tracker stays while it's the same kind.
        if let Some(t) = &mut tracker
            && let Some(old) = existing.and_then(|p| self.projects[p].tracker.as_ref())
            && old.provider == t.provider
        {
            t.extra = old.extra.clone();
        }
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
        // What the new account's connection test found, for the toast.
        let mut connected = None;
        if let Some(t) = &mut tracker {
            if t.site.is_empty() && !t.provider.has_board() {
                return Err(t.provider.site_missing().into());
            }
            if !t.key.is_empty() && !ticket_key::is_prefix(&t.key) {
                return Err(format!(
                    "{} isn't a ticket prefix: letters and digits, starting with a letter (like ENG)",
                    t.key
                ));
            }
            if t.provider.needs_account() {
                let picked =
                    form.fields.iter().find(|f| f.label == "Account").and_then(Field::chosen_id);
                let saved = self
                    .config
                    .accounts
                    .iter()
                    .position(|a| a.provider == t.provider && Some(a.id.as_str()) == picked);
                match saved {
                    Some(i) => {
                        t.account = Some(self.config.accounts[i].id.clone());
                        if !self.prefix_found(form, t, i)? {
                            return Ok(());
                        }
                    }
                    None => match self.new_account(form, t)? {
                        Some(toast) => connected = Some(toast),
                        None => return Ok(()),
                    },
                }
            }
        }

        let collapsed = existing.is_some_and(|p| self.projects[p].collapsed);
        let closed_agents =
            existing.map(|p| self.projects[p].closed_agents.clone()).unwrap_or_default();
        let project = Project {
            name,
            repo,
            tracker: tracker.clone(),
            collapsed,
            closed_agents,
            worktrees,
            extra: existing.map(|p| self.projects[p].extra.clone()).unwrap_or_default(),
        };
        if let Some(board) = tracker.as_ref().and_then(|t| t.board(&project.repo)) {
            local::ensure_columns(&board)
                .map_err(|e| format!("Couldn't create {}: {e}", store::tilde(&board)))?;
        }
        let name = project.name.clone();
        match existing {
            Some(p) => self.update_project(p, project),
            None => self.add_project(project),
        }
        if let Some(toast) = connected {
            let done = if existing.is_some() { "Saved" } else { "Added project" };
            self.toast(format!("{done} {name} · {toast}"));
        }
        Ok(())
    }

    /// Whether the prefix of `t`, whose account is the saved one at `i`, is one it can see: in the
    /// list when there is one, else checked with Jira in the background (Ok(false) while that
    /// runs). Err once, when it isn't; Enter again saves anyway.
    fn prefix_found(&mut self, form: &mut Form, t: &Tracker, i: usize) -> Result<bool, String> {
        if form.save_anyway || t.key.is_empty() {
            return Ok(true);
        }
        if let Some(e) = unlisted(form, t) {
            form.save_anyway = true;
            return Err(e);
        }
        // An account that sees no project can still use one: Jira says whether it's there.
        if listed(form).is_some_and(|l| !l.is_empty()) || !t.provider.checks_prefix() {
            return Ok(true);
        }
        let a = &self.config.accounts[i];
        let Some(client) = self
            .trackers
            .token(&a.token)
            .and_then(|token| Client::new(t, Path::new(""), Some((a, token))))
        else {
            return Ok(true);
        };
        match form.checked.clone() {
            Some(Check::Project(Ok(false))) => {
                form.save_anyway = true;
                Err(no_project(t))
            }
            Some(Check::Project(_)) => Ok(true),
            _ => {
                let key = t.key.clone();
                self.start_check(form, move |_| Check::Project(client.project_exists(&key)));
                Ok(false)
            }
        }
    }

    /// The account filled in on the form for `t`: put in the keychain if a token was pasted,
    /// tested, and saved, with `t` set to use it; returns the toast saying it connected. Ok(None)
    /// while the check runs in the background; Err when it failed, after which Enter again saves
    /// anyway.
    fn new_account(&mut self, form: &mut Form, t: &mut Tracker) -> Result<Option<String>, String> {
        let site = form.value(t.provider.site_field().unwrap_or("Site"));
        let (account, write) = self.draft_account(form, t.provider, site, None)?;
        let (verdict, project) = match form.checked.clone() {
            Some(Check::Stored(Err(e))) => {
                form.checked = None;
                return Err(stored_error(&e));
            }
            Some(Check::Account(v)) => (v, None),
            Some(Check::Signed(v, p)) => (v, Some(p)),
            _ => {
                // A Jira prefix typed with no list to go by is checked with the connection.
                let unlisted = listed(form).is_none_or(<[Choice]>::is_empty);
                let project = (t.provider.checks_prefix() && !t.key.is_empty() && unlisted)
                    .then(|| t.key.clone());
                self.check_account(form, &account, Some(t.site.clone()), true, write, project);
                return Ok(None);
            }
        };
        self.stored(form, &account, write.is_some());
        let toast = self.account_verdict(form, &account, Some(&t.site), verdict)?;
        if !form.save_anyway {
            if let Some(Ok(false)) = project {
                form.save_anyway = true;
                return Err(no_project(t));
            }
            if let Some(e) = unlisted(form, t) {
                form.save_anyway = true;
                return Err(e);
            }
        }
        let i = self.keep_account(account, None);
        t.account = Some(self.config.accounts[i].id.clone());
        Ok(Some(toast))
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
            return self.error(format!("Kept project {}: {e}", self.projects[p].name));
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
    fn shows_tracker_account_and_worktree_rows_only_when_they_apply() {
        let app = super::super::test_support::offline(store::Saved::default());
        let mut fields = super::tracker_fields(&app);
        fields.push(Field::text("Branch", "{tracker}", ""));
        let mut form = Form::new(FormKind::ProjectSettings(0), "Project", fields);
        let shown =
            |f: &Form| f.fields.iter().filter(|x| !x.hidden).map(|x| x.label).collect::<Vec<_>>();
        let rows = |form: &mut Form, git, keychain: Option<&Keychain>| {
            super::show_project_rows(form, git, &app.config, keychain);
            shown(form)
        };
        assert_eq!(rows(&mut form, true, None), ["Tracker"]);
        // Linear, with no saved account: its workspace, a new account's fields and the team; with
        // no keychain, the token goes in a file.
        form.set("Tracker", "Linear");
        assert_eq!(form.value("Keep in"), "File");
        assert_eq!(
            rows(&mut form, true, None),
            ["Tracker", "Workspace", "Account", "Keep in", "Token", "Team", "Branch"]
        );
        form.set("Keep in", "Variable or command");
        assert_eq!(
            rows(&mut form, true, None),
            ["Tracker", "Workspace", "Account", "Keep in", "Read from", "Team", "Branch"]
        );
        // Jira asks for an email too; with a keychain, the token goes there.
        form.set("Tracker", "Jira");
        let keychain = Keychain::os();
        form.set("Keep in", "Keychain");
        assert_eq!(
            rows(&mut form, true, Some(&keychain)),
            ["Tracker", "Site", "Account", "Email", "Keep in", "Token", "Project", "Branch"]
        );
        // A saved account has no fields of its own here.
        let account = form.field_mut("Account").unwrap();
        (account.choices, account.ids) = (vec!["you@example.com".into()], vec!["jira-1".into()]);
        account.value = "you@example.com".into();
        assert_eq!(
            rows(&mut form, true, None),
            ["Tracker", "Site", "Account", "Project", "Branch"]
        );
        // No worktrees outside git; hidden rows are skipped by Tab but keep their values.
        assert_eq!(rows(&mut form, false, None), ["Tracker", "Site", "Account", "Project"]);
        form.focus_on("Project");
        assert_eq!(form.next_focus(1), 0);
        assert_eq!(form.value("Branch"), "{tracker}");
        // Local files: a folder instead of a site, a prefix typed, and no account.
        form.set("Tracker", "Local files");
        assert_eq!(
            rows(&mut form, true, None),
            ["Tracker", "Tickets folder", "Ticket prefix", "Branch"]
        );
        let t = super::tracker_from_form(&form).unwrap();
        assert_eq!((t.provider, t.folder.as_deref()), (Provider::Local, Some("workplan")));
    }

    #[test]
    fn picks_the_choice_highlighted_typed_or_the_only_one_matching() {
        let mut form = Form::new(FormKind::Project, "", vec![Field::text("Project", "", "")]);
        let choice = |key: &str, name: &str| Choice { key: key.into(), name: name.into() };
        let asked = SignIn {
            provider: Provider::Jira,
            site: "https://example.atlassian.net".into(),
            email: "you@example.com".into(),
            source: "env:JIRA_API_TOKEN".into(),
            pasted: None,
        };
        let all = vec![
            choice("ENG", "Engineering"),
            choice("OPS", "Operations"),
            choice("WEB", "Website"),
        ];
        form.choices =
            Some(Box::new(Choices { asked, id: 1, got: Some(Ok(all)), shown: Vec::new() }));
        let pick = |form: &mut Form, typed: &str, at: Option<usize>| {
            form.fields[0].value = typed.into();
            form.fields[0].list.at = at;
            // The list as the field shows it, then what it picks.
            form.choices.as_mut().unwrap().shown = tracker::choosing(listed(form).unwrap(), typed)
                .iter()
                .map(|c| c.key.clone())
                .collect();
            take_pick(form, 0, false);
            form.fields[0].value.clone()
        };
        assert_eq!(pick(&mut form, "", Some(2)), "WEB");
        assert_eq!(pick(&mut form, "o", Some(0)), "OPS");
        assert_eq!(pick(&mut form, "opera", None), "OPS");
        // A key typed stays as typed, listed or not; several matches pick none.
        assert_eq!(pick(&mut form, "eng", None), "eng");
        assert_eq!(pick(&mut form, "QA", None), "QA");
        assert_eq!(pick(&mut form, "e", None), "e");
        let t = |key: &str| Tracker {
            provider: Provider::Jira,
            site: "https://example.atlassian.net".into(),
            folder: None,
            key: key.into(),
            account: None,
            extra: Default::default(),
        };
        assert_eq!(unlisted(&form, &t("ENG")), None);
        assert_eq!(unlisted(&form, &t("")), None);
        assert_eq!(
            unlisted(&form, &t("QA")).as_deref(),
            Some(
                "QA isn't one of the 3 projects this account can see. Pick one from the list, or press Enter again to save anyway."
            )
        );
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
