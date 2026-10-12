//! The Settings popover (`prefix ,`): its list of General, projects and accounts, the keys that
//! move through it, and the General form (the prefix key, updates). Also the tip, once, when tmux
//! or GNU screen has taskman's prefix too.

use super::sync::Msg;
use super::{App, Field, Form, FormKind, Mode};
use crate::keys;
use crate::store::Provider;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;

/// One row of the Settings list.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SettingsItem {
    General,
    Project(usize),
    NewProject,
    Account(usize),
    NewAccount,
}

/// The Settings popover: updates, projects and accounts on the left, the selected one's form on
/// the right.
pub struct Settings {
    pub items: Vec<SettingsItem>,
    pub cursor: usize,
    pub form: Box<Form>,
    /// Keys go to the form (true) or move through the list (false).
    pub editing: bool,
}

/// The tip for a user whose multiplexer has taskman's prefix too: `tmux` is tmux's `prefix`
/// option, `screen` whether taskman runs in GNU screen, whose escape key is ctrl+a unless changed
/// (which can't be asked).
pub(super) fn prefix_tip(prefix: keys::Prefix, tmux: Option<&str>, screen: bool) -> Option<String> {
    let host = if tmux.and_then(keys::Prefix::parse) == Some(prefix) {
        "tmux"
    } else if screen && prefix == keys::Prefix::CtrlA {
        "GNU screen"
    } else {
        return None;
    };
    Some(format!(
        "{host}'s prefix is {prefix} too: pick another for taskman in Settings ({prefix} ,)"
    ))
}

/// What else the prefix key is used for, under its field in Settings.
pub(super) fn prefix_hint(p: keys::Prefix) -> String {
    format!("{}. {p} a sends {p} itself to the session", p.conflict())
}

const AUTO_INSTALL: &str = "Install automatically";
const NOTIFY_ONLY: &str = "Only notify";

impl App {
    /// Asks, once, whether the tmux or GNU screen taskman runs in has its prefix too; tmux off the
    /// UI thread. Sessions don't see `TMUX`, but taskman's own environment has it.
    pub fn check_prefix_tip(&self) {
        let (tmux, screen) =
            (std::env::var_os("TMUX").is_some(), std::env::var_os("STY").is_some());
        if self.prefs.prefix_tip_shown || !(tmux || screen) {
            return;
        }
        let (prefix, tx) = (self.config.prefix, self.tx.clone());
        std::thread::spawn(move || {
            let mut show = std::process::Command::new("tmux");
            show.args(["show", "-gv", "prefix"]);
            let tmux_prefix = tmux
                .then(|| crate::proc::run(&mut show, Duration::from_secs(2)).ok())
                .flatten()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
            if let Some(tip) = prefix_tip(prefix, tmux_prefix.as_deref(), screen) {
                let _ = tx.send(Msg::PrefixTip(tip));
            }
        });
    }

    fn settings_items(&self) -> Vec<SettingsItem> {
        let mut v = vec![SettingsItem::General];
        v.extend((0..self.projects.len()).map(SettingsItem::Project));
        v.push(SettingsItem::NewProject);
        v.extend((0..self.config.accounts.len()).map(SettingsItem::Account));
        v.push(SettingsItem::NewAccount);
        v
    }

    fn form_for(&self, item: SettingsItem) -> Box<Form> {
        match item {
            SettingsItem::General => self.general_form(),
            SettingsItem::Project(p) => self.project_settings_form(p),
            SettingsItem::NewProject => self.new_project_form(),
            SettingsItem::Account(i) => self.account_settings_form(i),
            SettingsItem::NewAccount => self.new_account_form(Provider::Jira, String::new()),
        }
    }

    /// Opens Settings on `item` (a project or account that is gone falls back to adding one);
    /// `editing` sends keys to its form.
    pub fn open_settings(&mut self, item: SettingsItem, editing: bool) {
        let item = match item {
            SettingsItem::Project(p) if p >= self.projects.len() => SettingsItem::NewProject,
            SettingsItem::Account(i) if i >= self.config.accounts.len() => SettingsItem::NewAccount,
            _ => item,
        };
        self.settings_show(item, self.form_for(item), editing);
        if editing {
            self.project_form_opened();
        }
    }

    /// Shows `form` in Settings with `item` selected in the list.
    pub(super) fn settings_show(&mut self, item: SettingsItem, form: Box<Form>, editing: bool) {
        let items = self.settings_items();
        let cursor = items.iter().position(|x| *x == item).unwrap_or(0);
        self.mode = Mode::Settings(Box::new(Settings { items, cursor, form, editing }));
    }

    pub(super) fn settings_key(&mut self, key: KeyEvent) {
        let Mode::Settings(s) = &mut self.mode else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if s.editing {
            match key.code {
                KeyCode::Esc => {
                    s.editing = false;
                    s.form.error = None;
                    s.form.checking = None;
                }
                KeyCode::Char('d') if ctrl => self.delete_selected_setting(),
                _ => self.form_key(key),
            }
            return;
        }
        let step = |s: &Settings, d: isize| {
            (s.cursor.cast_signed() + d).rem_euclid(s.items.len().cast_signed()) as usize
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                let up = matches!(key.code, KeyCode::Up | KeyCode::Char('k'));
                let cursor = step(s, if up { -1 } else { 1 });
                let item = s.items[cursor];
                let form = self.form_for(item);
                if let Mode::Settings(s) = &mut self.mode {
                    s.cursor = cursor;
                    s.form = form;
                }
            }
            KeyCode::Enter | KeyCode::Tab | KeyCode::Right | KeyCode::Char('e') => {
                s.editing = true;
                self.project_form_opened();
            }
            KeyCode::Char('d') if ctrl => self.delete_selected_setting(),
            _ => {}
        }
    }

    fn delete_selected_setting(&mut self) {
        let Mode::Settings(s) = &self.mode else { return };
        match s.items[s.cursor] {
            SettingsItem::Project(p) => self.mode = Mode::ConfirmDeleteProject(p),
            SettingsItem::Account(i) => {
                let users = self.account_users(i);
                if users.is_empty() {
                    self.mode = Mode::ConfirmDeleteAccount(i);
                } else if let Mode::Settings(s) = &mut self.mode {
                    s.form.error = Some(format!(
                        "Used by {}. Pick another account in their settings first.",
                        users.join(", ")
                    ));
                }
            }
            _ => {}
        }
    }

    fn general_form(&self) -> Box<Form> {
        let value = if self.config.auto_update { AUTO_INSTALL } else { NOTIFY_ONLY };
        let hint = "checks at start and every hour; an update applies when taskman restarts";
        let prefix = self.config.prefix;
        let fields = vec![
            Field::choice(
                "Prefix key",
                keys::Prefix::ALL.iter().map(|p| p.name().to_string()).collect(),
                prefix.name(),
                prefix_hint(prefix),
            ),
            Field::choice("Updates", vec![AUTO_INSTALL.into(), NOTIFY_ONLY.into()], value, hint),
        ];
        let mut form = Form::new(FormKind::General, "General", fields);
        form.note = Some(self.updater.describe());
        Box::new(form)
    }

    pub(super) fn submit_general(&mut self, form: &Form) -> Result<(), String> {
        self.config.auto_update = form.value("Updates") == AUTO_INSTALL;
        let prefix = keys::Prefix::parse(&form.value("Prefix key")).unwrap_or_default();
        let changed = prefix != self.config.prefix;
        self.config.prefix = prefix;
        keys::set_prefix(prefix);
        self.save_config();
        self.toast(if changed {
            format!("Saved · commands now start with {prefix}")
        } else {
            "Saved".into()
        });
        self.settings_show(SettingsItem::General, self.general_form(), false);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tips_a_prefix_shared_with_tmux_or_screen() {
        use crate::keys::Prefix::{CtrlA, CtrlB, CtrlBracket};
        let tip = prefix_tip(CtrlA, Some("C-a"), false).unwrap();
        assert_eq!(
            tip,
            "tmux's prefix is ctrl+a too: pick another for taskman in Settings (ctrl+a ,)"
        );
        assert!(
            prefix_tip(CtrlB, Some("C-b"), false).unwrap().starts_with("tmux's prefix is ctrl+b")
        );
        // tmux's default prefix, or one taskman doesn't offer, is no clash.
        assert_eq!(prefix_tip(CtrlA, Some("C-b"), false), None);
        assert_eq!(prefix_tip(CtrlA, Some("M-x"), false), None);
        assert_eq!(prefix_tip(CtrlA, None, false), None);
        // GNU screen's escape is ctrl+a unless changed, which taskman can't ask.
        assert!(
            prefix_tip(CtrlA, None, true).unwrap().starts_with("GNU screen's prefix is ctrl+a")
        );
        assert!(prefix_tip(CtrlA, Some("C-b"), true).unwrap().starts_with("GNU screen's"));
        assert_eq!(prefix_tip(CtrlBracket, None, true), None);
    }
}
