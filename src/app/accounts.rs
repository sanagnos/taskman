//! Accounts in Settings: the new and edit forms, testing the connection before saving, and
//! deleting one no project uses. A project that needs an account it doesn't have waits for this
//! form (`AfterAccount`).

use super::forms::Check;
use super::sync::Msg;
use super::{App, Field, Form, FormKind, SettingsItem};
use crate::keychain::{self, Keychain};
use crate::store::{self, Account, Config, Project, Provider};
use crate::tracker;

/// What to do with a project once the account it needs is saved.
pub enum AfterAccount {
    Add(Box<Project>),
    Update(usize, Box<Project>),
}

/// The Keep in choices: the OS keychain, or a source the user gives.
const KEYCHAIN: &str = "Keychain";
const ELSEWHERE: &str = "Variable or command";

/// The note atop a new account's form.
const NOT_SAVED: &str = "taskman never saves your token in its files, only where to read it from.";

/// The guide's ways of keeping a token, for a form with no keychain or one that failed.
const GUIDE: &str =
    concat!(env!("CARGO_PKG_REPOSITORY"), "/blob/main/docs/guide.md#storing-a-token");

/// Whether the account form keeps its token in the keychain: Keep in is shown and says so.
fn keeps_in_keychain(form: &Form) -> bool {
    form.field("Keep in").is_some_and(|f| !f.hidden && f.value == KEYCHAIN)
}

/// The environment variable a new account's token is suggested in. A second Linear workspace gets
/// its own, so two workspaces don't silently share a key.
fn default_var(form: &Form, config: &Config) -> String {
    if form.value("Provider") != "Linear" {
        return Provider::Jira.token_var().into();
    }
    let var = Provider::Linear.token_var();
    let first = format!("env:{var}");
    if !config.accounts.iter().any(|a| a.provider == Provider::Linear && a.token == first) {
        return var.into();
    }
    let ws: String = form
        .value("Site")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
        .collect();
    if ws.is_empty() { format!("{var}_2") } else { format!("{var}_{ws}") }
}

/// The new account form's defaults, after a change to its provider, site or Keep in. Picking a
/// variable or command fills in the suggested variable; one the user typed stays.
pub(super) fn account_defaults(form: &mut Form, config: &Config, keychain: Option<&Keychain>) {
    let source = form.value("Read from");
    let is_default = source.is_empty()
        || source == format!("env:{}", Provider::Jira.token_var())
        || source.starts_with(&format!("env:{}", Provider::Linear.token_var()));
    // Without a keychain the field starts empty, so a value there was typed.
    let picked = form.field("Keep in").is_some_and(|f| !f.hidden && f.value == ELSEWHERE);
    if is_default && picked {
        form.set("Read from", format!("env:{}", default_var(form, config)));
    }
    let var = default_var(form, config);
    account_rows(form, None, keychain);
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

/// Shows the account form's rows for its provider and Keep in, with their hints. `account` is the
/// one being edited, if any; `keychain` the one found, which names itself in the hints.
pub(super) fn account_rows(
    form: &mut Form,
    account: Option<&Account>,
    keychain: Option<&Keychain>,
) {
    let provider = match form.value("Provider").as_str() {
        "Linear" => Provider::Linear,
        _ => Provider::Jira,
    };
    let label = keychain.map_or_else(|| Keychain::os().label(), Keychain::label);
    let keychain = keeps_in_keychain(form);
    let saved_there = account.is_some_and(|a| keychain::name_of(&a.token).is_some());
    let page = provider
        .token_page()
        .map(|(way, url)| ("Get a token: ".to_string(), way.to_string(), url.to_string()));
    let guide = ("Other ways: ".to_string(), "Storing a token".to_string(), GUIDE.to_string());
    let var = provider.token_var();
    for f in &mut form.fields {
        match f.label {
            "Site" if provider == Provider::Linear => {
                f.hint = "workspace: the part after linear.app/".into();
            }
            "Site" => f.hint = "where to test the token, like your-site.atlassian.net".into(),
            "Email" => {
                f.hidden = provider == Provider::Linear;
                f.hint = "the email you sign in to Atlassian with".into();
            }
            "Keep in" => {
                f.hint = format!("←/→: your {label}, or a variable or command of yours");
            }
            "Token" => {
                f.hidden = !keychain;
                f.hint = if saved_there {
                    format!("in your {label}; paste a new one to replace it")
                } else {
                    format!("paste it here; taskman puts it in your {label}")
                };
                f.links = page.clone().into_iter().collect();
            }
            "Read from" => {
                f.hidden = keychain;
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

    /// The Keep in field: shown where there's a keychain, or the account's token is in one.
    fn keep_in(&self, in_keychain: bool) -> Field {
        let mut f = Field::choice(
            "Keep in",
            vec![KEYCHAIN.into(), ELSEWHERE.into()],
            if in_keychain || self.keychain.is_some() { KEYCHAIN } else { ELSEWHERE },
            "",
        );
        f.hidden = self.keychain.is_none() && !in_keychain;
        f
    }

    pub(super) fn new_account_form(
        &self,
        provider: Provider,
        site: String,
        then: Option<AfterAccount>,
    ) -> Box<Form> {
        let providers = vec!["Jira".to_string(), "Linear".to_string()];
        let email = self
            .config
            .accounts
            .iter()
            .find(|a| a.provider == Provider::Jira && !a.email.is_empty())
            .map(|a| a.email.clone())
            .or_else(|| std::env::var("JIRA_EMAIL").ok())
            .unwrap_or_default();
        let mut form = Form::new(
            FormKind::Account { then },
            "New account",
            vec![
                Field::choice("Provider", providers, provider.label(), "←/→ to change"),
                Field::text("Site", site, ""),
                Field::text("Email", email, ""),
                self.keep_in(false),
                Field { masked: true, ..Field::text("Token", "", "") },
                Field::text("Read from", "", ""),
            ],
        );
        form.note = Some(NOT_SAVED.into());
        account_defaults(&mut form, &self.config, self.keychain.as_ref());
        let token = if keeps_in_keychain(&form) { "Token" } else { "Read from" };
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
        let mut keep_in = self.keep_in(in_keychain);
        if !in_keychain {
            keep_in.value = ELSEWHERE.into();
        }
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
            Field::text("Read from", if in_keychain { "" } else { a.token.as_str() }, ""),
        ];
        let mut form = Form::new(FormKind::AccountSettings(i), "Account", fields);
        account_rows(&mut form, Some(a), self.keychain.as_ref());
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
            None if form.value("Provider") == "Linear" => Provider::Linear,
            None => Provider::Jira,
        };
        let site = form.value("Site");
        if provider == Provider::Linear && site.is_empty() {
            return Err("Enter the Linear workspace (the part after linear.app/)".into());
        }
        let email = if provider == Provider::Jira { form.value("Email") } else { String::new() };
        if provider == Provider::Jira && email.is_empty() {
            return Err("Enter the email you sign in to Jira with".into());
        }
        // The token to put in the keychain first, if one was pasted: its keychain, item and value.
        let mut write = None;
        let token = if keeps_in_keychain(form) {
            let saved = existing.and_then(|i| keychain::name_of(&self.config.accounts[i].token));
            let pasted = form.value("Token");
            if !pasted.is_empty() {
                keychain::check_token(&pasted)?;
                if self.keychain.is_none() {
                    return Err(
                        "No keychain was found: pick Variable or command under Keep in".into()
                    );
                }
                // An account keeps its item; a new one is named by the id it's about to get.
                let id = existing.map(|i| self.config.accounts[i].id.clone());
                let name = saved
                    .map(String::from)
                    .or(id.filter(|id| keychain::is_name(id)))
                    .unwrap_or_else(|| self.config.next_id(provider));
                let source = keychain::source(&name);
                write = Some((name, pasted));
                source
            } else if let Some(source) = form
                .stored
                .clone()
                // Put there before the provider changed, under the other provider's id.
                .filter(|s| s.starts_with(&keychain::source(&provider.label().to_lowercase())))
                .or_else(|| saved.map(keychain::source))
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
        let mut account = Account { id: String::new(), provider, site, email, token };

        // Test the token before saving, against the account's site or the waiting project's (a
        // Jira account with neither is saved untested); a second Enter saves anyway.
        let test_site = match (&account.site, &form.kind) {
            (s, _) if !s.is_empty() => Some(s.clone()),
            (
                _,
                FormKind::Account { then: Some(AfterAccount::Add(p) | AfterAccount::Update(_, p)) },
            ) => p.tracker.as_ref().map(|t| t.site.clone()),
            _ => None,
        };
        let tested = provider == Provider::Linear || test_site.is_some();
        // The check puts a pasted token in the keychain, then reads the token as the clients will
        // and tests it; the clients use what it read rather than reading it again.
        let verdict = match form.checked.clone() {
            Some(Check::Stored(Err(e))) => {
                form.checked = None;
                return Err(format!("{e}. Fix it, or pick Variable or command under Keep in."));
            }
            Some(Check::Account(v)) => Some(v),
            Some(_) => None,
            None if tested || write.is_some() => {
                let (a, site) = (account.clone(), test_site.clone().unwrap_or_default());
                let k = self.keychain.clone().unwrap_or_else(Keychain::os);
                self.start_check(form, move |tx| {
                    if let Some((name, token)) = write
                        && let Err(e) = k.store(&name, &token)
                    {
                        return Check::Stored(Err(e));
                    }
                    if !tested {
                        return Check::Stored(Ok(()));
                    }
                    // The keychain just saved to: the one `resolve_token` reads, but taken from
                    // here so a unit test's stub is the one asked.
                    let token = match keychain::name_of(&a.token) {
                        Some(name) => k.read(name),
                        None => a.resolve_token(),
                    };
                    let _ = tx.send(Msg::Token(a.token.clone(), token.clone()));
                    Check::Account(token.and_then(|tok| tracker::whoami(&a, &site, &tok)))
                });
                return Ok(());
            }
            None => None,
        };
        // In the keychain now: out of the form, which saves without it.
        if write.is_some() {
            form.set("Token", "");
            form.stored = Some(account.token.clone());
        }
        // A variable that isn't set: the keychain is the way that needs no restart.
        let offer = if account.token.starts_with("env:") && self.keychain.is_some() {
            " Or pick Keychain under Keep in and paste the token there."
        } else {
            ""
        };
        let toast = match (provider, test_site, verdict) {
            (Provider::Linear, _, Some(verdict)) => {
                // The key must belong to the workspace entered.
                let problem = match &verdict {
                    Ok((_, ws)) if !ws.is_empty() && !ws.eq_ignore_ascii_case(&account.site) => {
                        Some(format!(
                            "This key belongs to the workspace {ws}, not {}",
                            account.site
                        ))
                    }
                    Ok(_) => None,
                    Err(e) => Some(e.clone()),
                };
                match (problem, verdict) {
                    (None, Ok((name, _))) => {
                        format!("✓ Connected to Linear {} as {name}", account.site)
                    }
                    (Some(p), _) if !form.save_anyway => {
                        form.save_anyway = true;
                        return Err(format!(
                            "{p}.{offer} Fix it, or press Enter again to save anyway."
                        ));
                    }
                    _ => format!(
                        "Saved the account without a working connection; check {}",
                        store::config_path().display()
                    ),
                }
            }
            (Provider::Jira, Some(site), Some(verdict)) => match verdict {
                Ok((name, _)) => format!("✓ Connected to {} as {name}", store::site_host(&site)),
                Err(e) if !form.save_anyway => {
                    form.save_anyway = true;
                    return Err(format!(
                        "{e}.{offer} Fix it, or press Enter again to save anyway."
                    ));
                }
                Err(_) => {
                    format!(
                        "Saved the account without a working connection; check {}",
                        store::config_path().display()
                    )
                }
            },
            (Provider::Jira, _, _) => {
                // Untested, so read its token again: it may be a new one behind the same source.
                self.trackers.forget(&account.token);
                "Saved the Jira account (add a site to test it)".to_string()
            }
            // The account form doesn't offer Local files, which needs no account; Linear always
            // has a verdict.
            _ => "Saved".to_string(),
        };

        // Edit in place, or reuse an identical account rather than adding a duplicate.
        let same = |a: &Account| {
            a.provider == account.provider
                && a.email == account.email
                && a.token == account.token
                && store::same_site(&a.site, &account.site)
        };
        let index = match existing {
            Some(i) => {
                account.id = self.config.accounts[i].id.clone();
                self.config.accounts[i] = account.clone();
                self.save_config();
                i
            }
            None => match self.config.accounts.iter().position(same) {
                Some(i) => {
                    account.id = self.config.accounts[i].id.clone();
                    i
                }
                None => {
                    account.id = self.config.next_id(provider);
                    self.config.accounts.push(account.clone());
                    self.save_config();
                    self.config.accounts.len() - 1
                }
            },
        };
        self.rebuild_clients();
        for tv in &mut self.tickets {
            tv.jira.request(false);
        }
        self.toast(toast);
        let after = match &mut form.kind {
            FormKind::Account { then } => then.take(),
            _ => None,
        };
        let Some(after) = after else {
            self.settings_show(
                SettingsItem::Account(index),
                self.account_settings_form(index),
                false,
            );
            return Ok(());
        };
        {
            match after {
                AfterAccount::Add(mut project) => {
                    if let Some(t) = &mut project.tracker {
                        t.account = Some(account.id.clone());
                    }
                    self.add_project(*project);
                }
                AfterAccount::Update(p, mut project) => {
                    if let Some(t) = &mut project.tracker {
                        t.account = Some(account.id.clone());
                    }
                    self.update_project(p, *project);
                }
            }
        }
        Ok(())
    }
}
