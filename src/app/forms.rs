//! Forms: their fields (`Field`, reached by label), keys and paste in the active form, and
//! submitting it, with a background check first where one is needed (a connection test, say). The
//! forms themselves are in `add_form.rs`, `settings.rs`, `projects.rs` and `accounts.rs`.

use super::accounts::{account_defaults, account_rows, form_provider};
use super::projects::Choices;
use super::settings::prefix_hint;
use super::sync::Msg;
use super::{App, Mode};
use crate::keys;
use crate::store::Provider;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc;

/// A form field: free text, or a choice cycled with ←/→ when `choices` is non-empty. An
/// `editable` choice is text with presets: ←/→ cycles them, typing edits.
pub struct Field {
    pub label: &'static str,
    pub value: String,
    pub hint: String,
    pub choices: Vec<String>,
    /// What each choice stands for, when that isn't its text: an account's id.
    pub ids: Vec<String>,
    /// A value too wide for the form is cut in the middle rather than at the end.
    pub cut_middle: bool,
    /// Sentences rather than a name: wraps within the value column instead of being cut.
    pub prose: bool,
    pub editable: bool,
    /// A heading drawn above the field, starting a group.
    pub section: Option<&'static str>,
    /// Not shown or focused, but keeps its value: a project's tracker fields while it has none.
    pub hidden: bool,
    /// Lines listed under the hint, each cut to fit: the Add form's open issues.
    pub list: List,
    /// A secret being typed (a token for the keychain): drawn as dots, never as itself.
    pub masked: bool,
    /// Lines under the hint that link somewhere: the text before the link, and the link's text
    /// and URL.
    pub links: Vec<(String, String, String)>,
}

/// Lines listed under a field's hint: the Add form's open issues, which ↑/↓ move a highlight
/// through while the field is focused.
#[derive(Clone, Debug, Default)]
pub struct List {
    pub lines: Vec<String>,
    /// How many issues the lines list, in a window from `top`, to pick with ↑/↓; 0 when the lines
    /// are a note instead.
    pub picks: usize,
    /// The highlighted issue, once ↑/↓ was pressed; changing a value drops it.
    pub at: Option<usize>,
    /// The first issue in the window.
    pub top: usize,
}

impl List {
    /// The line highlighted.
    pub fn highlighted(&self) -> Option<usize> {
        self.at.and_then(|a| a.checked_sub(self.top))
    }

    /// Moves the highlight; the first press of ↓ highlights the first issue, of ↑ the last.
    fn step(&mut self, down: bool) {
        let last = self.picks.saturating_sub(1);
        self.at = Some(match (self.at, down) {
            (None, true) => 0,
            (None, false) => last,
            (Some(a), true) => (a + 1).min(last),
            (Some(a), false) => a.saturating_sub(1),
        });
    }

    /// Scrolls the window of `shown` issues to keep the highlight in it.
    pub(super) fn scroll(&mut self, shown: usize) {
        if let Some(a) = self.at {
            self.top = self.top.min(a).max((a + 1).saturating_sub(shown));
        }
        self.top = self.top.min(self.picks.saturating_sub(shown));
    }
}

impl Field {
    pub(crate) fn text(
        label: &'static str,
        value: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            label,
            value: value.into(),
            hint: hint.into(),
            choices: Vec::new(),
            ids: Vec::new(),
            cut_middle: false,
            prose: false,
            editable: false,
            section: None,
            hidden: false,
            list: List::default(),
            masked: false,
            links: Vec::new(),
        }
    }

    pub(crate) fn choice(
        label: &'static str,
        choices: Vec<String>,
        value: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self { choices, ..Self::text(label, value, hint) }
    }

    pub(super) fn presets(label: &'static str, presets: &[&str], value: impl Into<String>) -> Self {
        Self {
            choices: presets.iter().map(|p| p.to_string()).collect(),
            editable: true,
            ..Self::text(label, value, "")
        }
    }

    pub(super) fn under(self, section: &'static str) -> Self {
        Self { section: Some(section), ..self }
    }

    /// Whether typing edits the value.
    pub fn is_text(&self) -> bool {
        self.choices.is_empty() || self.editable
    }

    fn cycle(&mut self, step: isize) {
        if self.choices.is_empty() {
            return;
        }
        let n = self.choices.len().cast_signed();
        // An edited value isn't among the presets: → goes to the first, ← to the last.
        let i = match self.choices.iter().position(|c| *c == self.value) {
            Some(i) => i.cast_signed(),
            None if step > 0 => -1,
            None => n,
        };
        self.value = self.choices[((i + step).rem_euclid(n)) as usize].clone();
    }

    /// The id behind the chosen value, if it has one.
    pub(super) fn chosen_id(&self) -> Option<&str> {
        let i = self.choices.iter().position(|c| *c == self.value)?;
        self.ids.get(i).map(String::as_str)
    }
}

pub enum FormKind {
    Ticket,
    Project,
    /// Settings of the project at this index.
    ProjectSettings(usize),
    /// Adding an account on its own; a project form adds one inline.
    Account,
    /// Editing the account at this index.
    AccountSettings(usize),
    /// Renaming the agent at this ticket index.
    RenameAgent(usize),
    /// Settings that aren't about a project or an account.
    General,
}

pub struct Form {
    pub kind: FormKind,
    pub title: &'static str,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
    pub note: Option<String>,
    /// Set after a failed connection test: Enter again saves the account anyway.
    pub(super) save_anyway: bool,
    /// The id of a check running in the background on Enter (a connection test, say); its result
    /// submits the form again. Editing a value drops it.
    pub checking: Option<u64>,
    /// What that check found, kept until a value changes: "Enter again to save anyway" uses it
    /// rather than checking again.
    pub(super) checked: Option<Check>,
    /// The source of a token this form put in the keychain and cleared from its Token field, so
    /// the form can save without it being pasted again.
    pub(super) stored: Option<String>,
    /// A project form's list of what its account can see, to pick the ticket prefix from.
    pub(super) choices: Option<Box<Choices>>,
    /// The ticket prefix a project form filled in itself, which it may change again until the user
    /// types another.
    pub(super) suggested: Option<String>,
}

impl Form {
    pub(crate) fn new(kind: FormKind, title: &'static str, fields: Vec<Field>) -> Self {
        Self {
            kind,
            title,
            fields,
            focus: 0,
            error: None,
            note: None,
            save_anyway: false,
            checking: None,
            checked: None,
            stored: None,
            choices: None,
            suggested: None,
        }
    }

    /// The values changed: a check of the old ones no longer applies.
    fn edited(&mut self) {
        self.checking = None;
        self.checked = None;
        // The list is narrowed again: a highlight could be on an issue it no longer has.
        for f in &mut self.fields {
            (f.list.at, f.list.top) = (None, 0);
        }
    }

    pub(super) fn value(&self, label: &str) -> String {
        self.field(label).map(|f| f.value.trim().to_string()).unwrap_or_default()
    }

    pub(super) fn set(&mut self, label: &str, value: impl Into<String>) {
        if let Some(f) = self.field_mut(label) {
            f.value = value.into();
        }
    }

    /// Fields are reached by label, never by place: forms change shape (the Add form's rows
    /// follow its Type), and a place would silently name another field.
    pub(super) fn field(&self, label: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.label == label)
    }

    pub(super) fn field_mut(&mut self, label: &str) -> Option<&mut Field> {
        self.fields.iter_mut().find(|f| f.label == label)
    }

    /// Focuses the field labelled `label`, if the form has it.
    pub(super) fn focus_on(&mut self, label: &str) {
        if let Some(i) = self.fields.iter().position(|f| f.label == label) {
            self.focus = i;
        }
    }

    /// The next shown field after the focused one, `step` ±1, wrapping.
    pub(super) fn next_focus(&self, step: isize) -> usize {
        let n = self.fields.len().cast_signed();
        let mut i = self.focus.cast_signed();
        for _ in 0..n {
            i = (i + step).rem_euclid(n);
            if !self.fields[i as usize].hidden {
                break;
            }
        }
        i as usize
    }
}

/// What a form's background check found.
#[derive(Clone)]
pub(super) enum Check {
    /// Whether the project's Jira project exists; Err when that couldn't be told (offline, say).
    Project(Result<bool, String>),
    /// Who an account's token belongs to, and for Linear its workspace; or why that failed.
    Account(Result<(String, String), String>),
    /// A new account's check for a project form, with a Jira prefix typed by hand: who the token
    /// belongs to, and whether that project exists.
    Signed(Result<(String, String), String>, Result<bool, String>),
    /// A token was put in the keychain, and not tested (a Jira account with no site); or why it
    /// couldn't be, which no "save anyway" gets past.
    Stored(Result<(), String>),
}

pub(super) fn paste_into(form: &mut Form, text: &str) {
    let field = &mut form.fields[form.focus];
    if field.is_text() {
        field.value.push_str(text.trim());
        form.edited();
    }
}

impl App {
    pub(super) fn form_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let form = match &mut self.mode {
            Mode::Form(f) => f,
            Mode::Settings(s) => &mut s.form,
            _ => return,
        };
        let before = form.focus;
        let is_choice = !form.fields[form.focus].choices.is_empty();
        let is_text = form.fields[form.focus].is_text();
        let values: Vec<String> = form.fields.iter().map(|f| f.value.clone()).collect();
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return;
            }
            // Enter again while the check runs would start a second one.
            KeyCode::Enter if form.checking.is_some() => return,
            // ↑/↓ pick from the focused field's list, if it has one; otherwise they move between
            // fields.
            KeyCode::Down | KeyCode::Up if form.fields[form.focus].list.picks > 0 => {
                form.fields[form.focus].list.step(key.code == KeyCode::Down);
            }
            KeyCode::Tab | KeyCode::Down => form.focus = form.next_focus(1),
            KeyCode::BackTab | KeyCode::Up => form.focus = form.next_focus(-1),
            KeyCode::Left if is_choice => form.fields[form.focus].cycle(-1),
            KeyCode::Right | KeyCode::Char(' ') if is_choice && !is_text => {
                form.fields[form.focus].cycle(1);
            }
            KeyCode::Right if is_choice => form.fields[form.focus].cycle(1),
            KeyCode::Backspace if is_text => {
                form.fields[form.focus].value.pop();
            }
            KeyCode::Char('u') if ctrl && is_text => form.fields[form.focus].value.clear(),
            KeyCode::Char(c) if !ctrl && is_text => form.fields[form.focus].value.push(c),
            KeyCode::Enter => {
                self.submit_form();
                return;
            }
            _ => {}
        }
        form.save_anyway = false;
        if form.fields.iter().map(|f| &f.value).ne(values.iter()) {
            form.edited();
        }
        // Changing provider resets defaults.
        let label = form.fields[before].label;
        if label == "Prefix key"
            && let Some(f) = form.fields.iter_mut().find(|f| f.label == label)
        {
            f.hint = prefix_hint(keys::Prefix::parse(&f.value).unwrap_or_default());
        }
        if matches!(label, "Provider" | "Site" | "Keep in") {
            match form.kind {
                FormKind::Account => {
                    let (provider, site) = (form_provider(form), form.value("Site"));
                    account_defaults(form, provider, &site, &self.config, self.keychain.as_ref());
                }
                FormKind::AccountSettings(i) => {
                    let account = self.config.accounts.get(i);
                    let provider = account.map_or(Provider::Jira, |a| a.provider);
                    account_rows(form, provider, account, self.keychain.as_ref());
                }
                _ => {}
            }
        }
        let project_form = matches!(form.kind, FormKind::Project | FormKind::ProjectSettings(_));
        let ticket_form = matches!(form.kind, FormKind::Ticket);
        if ticket_form
            && matches!(label, "Project" | "Type" | "CLI" | "Ticket" | "Name" | "Worktree" | "Base")
        {
            let mut f = std::mem::replace(form, Box::new(Form::new(FormKind::Ticket, "", vec![])));
            if label == "Project"
                && let Some(p) = self.projects.iter().position(|p| p.name == f.value("Project"))
            {
                self.list_open_issues(p);
            }
            self.update_ticket_hint(&mut f);
            self.update_worktree_field(&mut f, label == "Project");
            self.put_form(f);
        } else if project_form {
            let mut f = std::mem::replace(form, Box::new(Form::new(FormKind::Ticket, "", vec![])));
            self.project_form_changed(&mut f, label, before);
            self.put_form(f);
        }
    }

    /// Puts a form back where the active one lives: the popup form or the Settings pane.
    fn put_form(&mut self, f: Box<Form>) {
        match &mut self.mode {
            Mode::Form(x) => *x = f,
            Mode::Settings(s) => s.form = f,
            _ => self.mode = Mode::Form(f),
        }
    }

    /// Submits the active form. On success the submit sets what shows next; on failure the
    /// form comes back with its error.
    pub(super) fn submit_form(&mut self) {
        let blank = || Box::new(Form::new(FormKind::Ticket, "", vec![]));
        let mut form = match &mut self.mode {
            Mode::Form(f) => std::mem::replace(f, blank()),
            Mode::Settings(s) => std::mem::replace(&mut s.form, blank()),
            _ => return,
        };
        let result = match &form.kind {
            FormKind::Ticket => self.submit_ticket(&form),
            FormKind::Project => self.submit_project(&mut form, None),
            FormKind::ProjectSettings(p) => {
                let p = *p;
                self.submit_project(&mut form, Some(p))
            }
            FormKind::Account => self.submit_account(&mut form, None),
            FormKind::AccountSettings(i) => {
                let i = *i;
                self.submit_account(&mut form, Some(i))
            }
            FormKind::RenameAgent(i) => {
                let i = *i;
                self.rename_agent(i, &form.value("Name"))
            }
            FormKind::General => self.submit_general(&form),
        };
        match result {
            Err(e) => {
                form.error = Some(e);
                self.put_form(form);
            }
            Ok(()) if form.checking.is_some() => self.put_form(form),
            Ok(()) => {}
        }
    }

    /// Runs `check` in the background for `form`, which shows it's checking; the result submits
    /// the form again with `checked` set, if it's still open and unchanged.
    pub(super) fn start_check(
        &mut self,
        form: &mut Form,
        check: impl FnOnce(&mpsc::Sender<Msg>) -> Check + Send + 'static,
    ) {
        self.checks += 1;
        let (id, tx) = (self.checks, self.tx.clone());
        form.checking = Some(id);
        form.checked = None;
        form.error = None;
        std::thread::spawn(move || {
            let found = check(&tx);
            let _ = tx.send(Msg::Checked(id, found));
        });
    }
}
