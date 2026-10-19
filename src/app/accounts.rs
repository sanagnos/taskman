//! Accounts: the fields a new one is filled in with (in Settings' account forms, and inline in the
//! project forms), putting a pasted token in the keychain or, where there's none, a private file,
//! testing the connection before saving, and deleting one no project uses.

use super::forms::Check;
use super::sync::Msg;
use super::{App, Field, Form, FormKind, SettingsItem};
use crate::keychain::{self, Keychain};
use crate::store::{self, Account, Config, Provider};
use crate::{token_file, tracker};

/// The Keep in choices: the OS keychain, or where there's none a private file (`token_file`); or a
/// source the user gives.
const KEYCHAIN: &str = "Keychain";
const FILE: &str = "File";
const ELSEWHERE: &str = "Variable or command";

/// The note atop a new account's form, and under a project form's Account when it adds one: with
/// a keychain, and without one, where File puts a token in a file of taskman's.
pub(super) fn not_saved(keychain: Option<&Keychain>) -> &'static str {
    match keychain {
        Some(_) => "taskman never saves your token in its files, only where to read it from.",
        None => "taskman never saves your token in config.json, only where to read it from.",
    }
}

/// The guide's ways of keeping a token, for a form with no keychain or one that failed.
const GUIDE: &str =
    concat!(env!("CARGO_PKG_REPOSITORY"), "/blob/main/docs/guide.md#storing-a-token");

/// Whether the account form takes a pasted token, for the keychain or a file: Keep in is shown
/// and says so.
pub(super) fn takes_pasted(form: &Form) -> bool {
    form.field("Keep in").is_some_and(|f| !f.hidden && [KEYCHAIN, FILE].contains(&&*f.value))
}

/// Whether the account form puts a pasted token in a file rather than the keychain.
fn in_file(form: &Form) -> bool {
    form.value("Keep in") == FILE
}

/// The keychain item or token file `source` reads, in the place the form picked (`file`).
fn pasted_name(source: &str, file: bool) -> Option<&str> {
    if file { token_file::name_of(source) } else { keychain::name_of(source) }
}

/// The provider an account form is for.
pub(super) fn form_provider(form: &Form) -> Provider {
    match form.value("Provider").as_str() {
        "Linear" => Provider::Linear,
        _ => Provider::Jira,
    }
}

/// The environment variable a new account's token is suggested in. A second Linear workspace gets
/// its own, so two workspaces don't silently share a key.
fn default_var(provider: Provider, site: &str, config: &Config) -> String {
    if provider.account_spans_sites() {
        return provider.token_var().into();
    }
    let var = provider.token_var();
    let first = format!("env:{var}");
    if !config.accounts.iter().any(|a| a.provider == provider && a.token == first) {
        return var.into();
    }
    let ws: String = site
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
        .collect();
    if ws.is_empty() { format!("{var}_2") } else { format!("{var}_{ws}") }
}

/// A new account's defaults, after a change to its provider, site or Keep in: `site` is the Jira
/// site or Linear workspace typed. Picking a variable or command fills in the suggested variable;
/// one the user typed stays.
pub(super) fn account_defaults(
    form: &mut Form,
    provider: Provider,
    site: &str,
    config: &Config,
    keychain: Option<&Keychain>,
) {
    let source = form.value("Read from");
    let is_default = source.is_empty()
        || source == format!("env:{}", Provider::Jira.token_var())
        || source.starts_with(&format!("env:{}", Provider::Linear.token_var()));
    let picked = form.field("Keep in").is_some_and(|f| !f.hidden && f.value == ELSEWHERE);
    // A project form comes here on every key: a field cleared to type in stays clear.
    let typing = form.fields.get(form.focus).is_some_and(|f| f.label == "Read from");
    let var = default_var(provider, site, config);
    if is_default && picked && !typing {
        form.set("Read from", format!("env:{var}"));
    }
    account_rows(form, provider, site, None, keychain);
    if let Some(f) = form.field_mut("Read from") {
        f.hint = read_from_hint(&var);
    }
}

fn read_from_hint(var: &str) -> String {
    format!("env:{var} reads a variable you set; cmd: runs a command that prints the token")
}

/// What to type in Read from when it holds a token. Never repeats the value.
fn raw_token_hint(provider: Provider) -> String {
    let var = provider.token_var();
    format!(
        "Enter where to read the token from, like env:{var} or cmd:op read …, not the token itself"
    )
}

/// Shows an account's rows for its provider and Keep in, with their hints: in an account form, or
/// a project form adding one. `site` is the Jira site or Linear workspace typed; `account` the one
/// being edited, if any; `keychain` the one found, which names itself in the hints.
pub(super) fn account_rows(
    form: &mut Form,
    provider: Provider,
    site: &str,
    account: Option<&Account>,
    keychain: Option<&Keychain>,
) {
    // A form opened before the keychain was found offers it from then on.
    if let Some(f) = form.field_mut("Keep in")
        && keychain.is_some()
        && f.choices[0] == FILE
    {
        f.choices[0] = KEYCHAIN.into();
        if f.value == FILE {
            f.value = KEYCHAIN.into();
        }
    }
    // A project form's site field is its own, with its own hint.
    let account_form = matches!(form.kind, FormKind::Account | FormKind::AccountSettings(_));
    let label = keychain.map_or_else(|| Keychain::os().label(), Keychain::label);
    let pasted = takes_pasted(form);
    let file = in_file(form);
    let saved_there = account.is_some_and(|a| pasted_name(&a.token, file).is_some());
    let files = token_file::shown_dir();
    // The URL shown whole, for terminals that don't open links but spot URLs (see `form_lines`).
    let page = provider.token_page(site).map(|url| ("Get a token:".to_string(), url.clone(), url));
    let guide = ("Other ways: ".to_string(), "Storing a token".to_string(), GUIDE.to_string());
    let var = provider.token_var();
    for f in &mut form.fields {
        match f.label {
            "Site" if account_form && provider == Provider::Linear => {
                f.hint = "workspace: the part after linear.app/".into();
            }
            "Site" if account_form => {
                f.hint = "where to test the token, like your-site.atlassian.net".into();
            }
            "Email" => {
                f.hidden = !provider.signs_in_with_email();
                f.hint = "the email you sign in to Atlassian with".into();
            }
            "Keep in" if f.choices.iter().any(|c| c == FILE) => {
                f.hint = "←/→: a file only you can read, or a variable or command of yours".into();
            }
            "Keep in" => {
                f.hint = format!("←/→: your {label}, or a variable or command of yours");
            }
            "Token" => {
                f.hidden = !pasted;
                f.hint = match (file, saved_there) {
                    (true, true) => format!("in a file in {files}; paste a new one to replace it"),
                    (true, false) => {
                        format!(
                            "paste it here; taskman saves it in a file only you can read, in {files}"
                        )
                    }
                    (false, true) => format!("in your {label}; paste a new one to replace it"),
                    (false, false) => format!("paste it here; taskman puts it in your {label}"),
                };
                f.links = page.clone().into_iter().collect();
            }
            "Read from" => {
                f.hidden = pasted;
                f.hint = read_from_hint(var);
                f.links = page.clone().into_iter().chain([guide.clone()]).collect();
            }
            _ => {}
        }
    }
}

impl App {
    /// Names of the projects that use account `i`.
    pub(super) fn account_users(&self, i: usize) -> Vec<String> {
        self.projects
            .iter()
            .filter(|p| p.tracker.as_ref().and_then(|t| self.config.account_for(t)) == Some(i))
            .map(|p| p.name.clone())
            .collect()
    }

    /// The keychain item account `i` reads its token from, if no other account reads it too.
    pub(crate) fn own_keychain_item(&self, i: usize) -> Option<String> {
        let token = &self.config.accounts.get(i)?.token;
        let name = keychain::name_of(token)?;
        let shared =
            self.config.accounts.iter().enumerate().any(|(j, a)| j != i && &a.token == token);
        (!shared).then(|| name.to_string())
    }

    /// Deletes account `i`, and with `token` its keychain item, in the background.
    pub(super) fn delete_account(&mut self, i: usize, token: bool) {
        let item = self.own_keychain_item(i).filter(|_| token);
        let gone = self.config.accounts.remove(i);
        self.save_config();
        self.rebuild_clients();
        self.toast(format!("Deleted account {}", gone.label()));
        if let Some(name) = item {
            let tx = self.tx.clone();
            let k = self.keychain.clone().unwrap_or_else(Keychain::os);
            std::thread::spawn(move || {
                let _ = tx.send(Msg::Toast(match k.delete(&name) {
                    Ok(()) => format!(
                        "Deleted account {} and its token in your {}",
                        gone.label(),
                        k.label()
                    ),
                    Err(e) => format!("Deleted account {}, but {e}", gone.label()),
                }));
            });
        }
    }

    /// The Keep in field, its first choice the default: the keychain where there's one, or the
    /// account's token is in one; else a private file.
    pub(super) fn keep_in(&self, in_keychain: bool) -> Field {
        let first = if in_keychain || self.keychain.is_some() { KEYCHAIN } else { FILE };
        Field::choice("Keep in", vec![first.into(), ELSEWHERE.into()], first, "")
    }

    /// A new account's own fields: Email, Keep in, Token and Read from, shown as its provider
    /// and Keep in need (`account_rows`).
    pub(super) fn new_account_fields(&self) -> Vec<Field> {
        let email = self
            .config
            .accounts
            .iter()
            .find(|a| a.provider.signs_in_with_email() && !a.email.is_empty())
            .map(|a| a.email.clone())
            .or_else(|| std::env::var("JIRA_EMAIL").ok())
            .unwrap_or_default();
        vec![
            Field::text("Email", email, ""),
            self.keep_in(false),
            Field { masked: true, ..Field::text("Token", "", "") },
            Field::text("Read from", "", ""),
        ]
    }

    pub(super) fn new_account_form(&self, provider: Provider, site: String) -> Box<Form> {
        let providers = vec!["Jira".to_string(), "Linear".to_string()];
        let mut fields = vec![
            Field::choice("Provider", providers, provider.label(), "←/→ to change"),
            Field::text("Site", site, ""),
        ];
        fields.extend(self.new_account_fields());
        let mut form = Form::new(FormKind::Account, "New account", fields);
        form.note = Some(not_saved(self.keychain.as_ref()).into());
        let site = form.value("Site");
        account_defaults(&mut form, provider, &site, &self.config, self.keychain.as_ref());
        let token = if takes_pasted(&form) { "Token" } else { "Read from" };
        form.focus_on(if form.value("Site").is_empty() {
            "Site"
        } else if provider == Provider::Jira && form.value("Email").is_empty() {
            "Email"
        } else {
            token
        });
        Box::new(form)
    }

    pub(super) fn account_settings_form(&self, i: usize) -> Box<Form> {
        let a = &self.config.accounts[i];
        let site = a.provider.site_for_input(&a.site);
        let in_keychain = keychain::name_of(&a.token).is_some();
        let in_file = token_file::name_of(&a.token).is_some();
        let mut keep_in = self.keep_in(in_keychain);
        if !in_keychain && !(in_file && keep_in.choices.iter().any(|c| c == FILE)) {
            keep_in.value = ELSEWHERE.into();
        }
        let pasted = keep_in.value != ELSEWHERE;
        let fields = vec![
            Field::choice(
                "Provider",
                vec![a.provider.label().to_string()],
                a.provider.label(),
                "fixed; add a new account to switch",
            ),
            Field::text("Site", site, ""),
            Field::text("Email", a.email.clone(), ""),
            keep_in,
            Field { masked: true, ..Field::text("Token", "", "") },
            Field::text("Read from", if pasted { "" } else { a.token.as_str() }, ""),
        ];
        let mut form = Form::new(FormKind::AccountSettings(i), "Account", fields);
        let site = form.value("Site");
        account_rows(&mut form, a.provider, &site, Some(a), self.keychain.as_ref());
        form.focus_on("Site");
        let users = self.account_users(i);
        form.note = Some(if users.is_empty() {
            "Not used by any project".into()
        } else {
            format!("Used by {}", users.join(", "))
        });
        // Saved as typed by an older version: say so where it can be fixed.
        if !store::is_token_source(&a.token) {
            form.error =
                Some(format!("{}, so config.json no longer keeps it", raw_token_hint(a.provider)));
        }
        Box::new(form)
    }

    /// Adds an account (`existing` None) or saves the account at index `existing`.
    pub(super) fn submit_account(
        &mut self,
        form: &mut Form,
        existing: Option<usize>,
    ) -> Result<(), String> {
        let provider = match existing {
            Some(i) => self.config.accounts[i].provider,
            None => form_provider(form),
        };
        let site = form.value("Site");
        let (account, write) = self.draft_account(form, provider, site, existing)?;
        // Test the token before saving, against the account's site (a Jira account without one is
        // saved untested); a second Enter saves anyway.
        let test_site = (!account.site.is_empty()).then(|| account.site.clone());
        let tested = !provider.account_spans_sites() || test_site.is_some();
        let verdict = match form.checked.clone() {
            Some(Check::Stored(Err(e))) => {
                form.checked = None;
                return Err(stored_error(&e));
            }
            Some(Check::Account(v)) => Some(v),
            Some(_) => None,
            None if tested || write.is_some() => {
                self.check_account(form, &account, test_site, tested, write, None);
                return Ok(());
            }
            None => None,
        };
        self.stored(form, &account, write.is_some());
        let toast = match verdict {
            Some(verdict) => self.account_verdict(form, &account, test_site.as_deref(), verdict)?,
            None => {
                // Untested, so read its token again: it may be a new one behind the same source.
                self.trackers.forget(&account.token);
                format!("Saved the {} account (add a site to test it)", provider.label())
            }
        };
        let index = self.keep_account(account, existing);
        self.toast(toast);
        self.settings_show(SettingsItem::Account(index), self.account_settings_form(index), false);
        Ok(())
    }

    /// The account a form's fields describe for `provider` and `site` (the Jira site or Linear
    /// workspace typed), its id left empty; and the token pasted to put in the keychain or a file
    /// first, as its item's or file's name and the token. `existing` is the account being edited.
    pub(super) fn draft_account(
        &self,
        form: &mut Form,
        provider: Provider,
        site: String,
        existing: Option<usize>,
    ) -> Result<(Account, Option<(String, String)>), String> {
        if !provider.account_spans_sites() && site.is_empty() {
            return Err(provider.site_missing().into());
        }
        let email =
            if provider.signs_in_with_email() { form.value("Email") } else { String::new() };
        if provider.signs_in_with_email() && email.is_empty() {
            return Err(format!("Enter the email you sign in to {} with", provider.label()));
        }
        // The token to put in the keychain or a file first, if one was pasted: its name and value.
        let mut write = None;
        let file = in_file(form);
        let token = if takes_pasted(form) {
            let saved = existing.and_then(|i| pasted_name(&self.config.accounts[i].token, file));
            let pasted = form.value("Token");
            if !pasted.is_empty() {
                keychain::check_token(&pasted)?;
                if !file && self.keychain.is_none() {
                    return Err(
                        "No keychain was found: pick Variable or command under Keep in".into()
                    );
                }
                // An account keeps its item; a new one is named by the id it's about to get.
                let id = existing.map(|i| self.config.accounts[i].id.clone());
                let name = saved
                    .map(String::from)
                    .or(id.filter(|id| token_file::is_name(id)))
                    .unwrap_or_else(|| self.config.next_id(provider));
                let source = if file { token_file::source(&name) } else { keychain::source(&name) };
                write = Some((name, pasted));
                source
            } else if let Some(source) = form
                .stored
                .clone()
                // Put there before the provider changed, under the other provider's id.
                .filter(|s| {
                    pasted_name(s, file)
                        .is_some_and(|n| n.starts_with(&provider.label().to_lowercase()))
                })
                .or_else(|| {
                    saved.map(|n| if file { token_file::source(n) } else { keychain::source(n) })
                })
            {
                source
            } else {
                return Err(keychain::check_token("").unwrap_err());
            }
        } else {
            // `$NAME` becomes `env:NAME` in the field too, so the form shows what's saved.
            let token = store::env_source_from_shell(&form.value("Read from"));
            if token != form.value("Read from") {
                form.set("Read from", token.clone());
            }
            // Before any test or save: what's saved is where to read the token, never the token,
            // and "save anyway" is for a connection that failed, not for this.
            if !store::is_token_source(&token) {
                return Err(raw_token_hint(provider));
            }
            token
        };
        let site = provider.site_from_input(site);
        Ok((
            Account { id: String::new(), provider, site, email, token, extra: Default::default() },
            write,
        ))
    }

    /// Checks `account` in the background for `form`: puts a pasted token (`write`) in the
    /// keychain or its file (as the account's source says) first, then, if `tested`, reads the
    /// token as the clients will and tests it against `site` (Jira's; Linear's key knows its
    /// workspace), and with `project` checks that Jira project exists too. The clients use the
    /// token it read rather than reading it again.
    pub(super) fn check_account(
        &mut self,
        form: &mut Form,
        account: &Account,
        site: Option<String>,
        tested: bool,
        write: Option<(String, String)>,
        project: Option<String>,
    ) {
        let (a, site) = (account.clone(), site.unwrap_or_default());
        let k = self.keychain.clone().unwrap_or_else(Keychain::os);
        // Taken here, as a unit test's background thread has a scratch folder of its own.
        let files = token_file::name_of(&a.token).map(|_| token_file::dir());
        self.start_check(form, move |tx| {
            let store = |name: &str, token: &str| match &files {
                Some(dir) => token_file::store_in(dir, name, token),
                None => k.store(name, token),
            };
            if let Some((name, token)) = write
                && let Err(e) = store(&name, &token)
            {
                return Check::Stored(Err(e));
            }
            if !tested {
                return Check::Stored(Ok(()));
            }
            // The keychain just saved to: the one `resolve_token` reads, but taken from here so a
            // unit test's stub is the one asked.
            let token = match keychain::name_of(&a.token) {
                Some(name) => k.read(name),
                None => a.resolve_token(),
            };
            let _ = tx.send(Msg::Token(a.token.clone(), token.clone()));
            let who = token.clone().and_then(|tok| tracker::whoami(&a, &site, &tok));
            match (project, token) {
                (Some(key), Ok(tok)) if who.is_ok() => {
                    let jira =
                        crate::jira::JiraConfig::new(&store::jira_base(&site), &a.email, &tok);
                    Check::Signed(who, jira.project_exists(&key))
                }
                _ => Check::Account(who),
            }
        });
    }

    /// After a check that got past the keychain or file: a token it put there is out of the form,
    /// which saves without it.
    pub(super) fn stored(&self, form: &mut Form, account: &Account, wrote: bool) {
        if wrote {
            form.set("Token", "");
            form.stored = Some(account.token.clone());
        }
    }

    /// What a connection test found, as the toast once the account is saved; or the error that
    /// sends the form back, after which Enter again saves anyway. `site` is the one it was tested
    /// against (Jira's).
    pub(super) fn account_verdict(
        &mut self,
        form: &mut Form,
        account: &Account,
        site: Option<&str>,
        verdict: Result<(String, String), String>,
    ) -> Result<String, String> {
        // A variable that isn't set: the keychain, or else a file, is the way that needs no
        // restart.
        let offer = match &self.keychain {
            _ if !account.token.starts_with("env:") => "",
            Some(_) => " Or pick Keychain under Keep in and paste the token there.",
            None => " Or pick File under Keep in and paste the token there.",
        };
        // A Linear key must belong to the workspace entered.
        let problem = match &verdict {
            Ok((_, ws))
                if !account.provider.account_spans_sites()
                    && !ws.is_empty()
                    && !ws.eq_ignore_ascii_case(&account.site) =>
            {
                Some(format!("This key belongs to the workspace {ws}, not {}", account.site))
            }
            Ok(_) => None,
            Err(e) => Some(e.clone()),
        };
        match (problem, verdict) {
            (None, Ok((name, _))) => {
                Ok(match site.filter(|_| account.provider.account_spans_sites()) {
                    Some(site) => format!("✓ Connected to {} as {name}", store::site_host(site)),
                    None => format!(
                        "✓ Connected to {} {} as {name}",
                        account.provider.label(),
                        account.site
                    ),
                })
            }
            (Some(p), _) if !form.save_anyway => {
                form.save_anyway = true;
                Err(format!("{p}.{offer} Fix it, or press Enter again to save anyway."))
            }
            _ => Ok(format!(
                "Saved the account without a working connection; check {}",
                store::config_path().display()
            )),
        }
    }

    /// Saves `account` as the one at `existing`, or as a new one unless an identical one is there
    /// already; returns its index. Its id is set.
    pub(super) fn keep_account(&mut self, mut account: Account, existing: Option<usize>) -> usize {
        let same = |a: &Account| {
            a.provider == account.provider
                && a.email == account.email
                && a.token == account.token
                && store::same_site(&a.site, &account.site)
        };
        let index = match existing {
            Some(i) => {
                let old = &mut self.config.accounts[i];
                account.id = old.id.clone();
                account.extra = std::mem::take(&mut old.extra);
                self.config.accounts[i] = account;
                self.save_config();
                i
            }
            None => match self.config.accounts.iter().position(same) {
                Some(i) => i,
                None => {
                    account.id = self.config.next_id(account.provider);
                    self.config.accounts.push(account);
                    self.save_config();
                    self.config.accounts.len() - 1
                }
            },
        };
        self.rebuild_clients();
        for tv in &mut self.tickets {
            tv.jira.request(false);
        }
        index
    }
}

/// A keychain or file that couldn't take the token: no "save anyway" gets past it.
pub(super) fn stored_error(e: &str) -> String {
    format!("{e}. Fix it, or pick Variable or command under Keep in.")
}
