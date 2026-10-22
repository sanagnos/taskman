//! Keys and paste: every key goes to the selected session except after the prefix, in a dialog,
//! form or list (`Mode`), or while text is selected. The prefix commands, the links list
//! (`prefix l`) and opening the session's history (`prefix v`).

use super::forms::paste_into;
use super::sync::Msg;
use super::ticket::merged_head;
use super::tickets::{Closed, left_open};
use super::{ASK_ARMS, AgentState, App, Away, FormKind, HelpView, LinkList, Mode, SettingsItem};
use crate::selection;
use crate::store::{self, UiPrefs};
use crate::ui;
use crate::{close, keys, links, local, untrusted, util};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::Rect;
use std::path::Path;

/// Said when what was typed or pasted is dropped, as more than `session::QUEUE` would then wait
/// for the session's CLI to read it.
pub(super) const NOT_SENT: &str =
    "Not sent: that would leave over 4 MB waiting for the session to read";

/// Writes a file only its owner can read: a session's history can hold anything it printed.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())
}

/// Where `prefix prefix` goes from ticket `sel`, through tickets `all` in sidebar order starting
/// after it and wrapping: the first that needs the user, else the first that wants a look
/// (`TicketView::needs_attention`). `sel` itself comes last, and only if it wants a look.
fn next_attention_in(
    all: &[usize],
    sel: usize,
    needs: impl Fn(usize) -> bool,
    attention: impl Fn(usize) -> bool,
) -> Option<usize> {
    let n = all.len();
    let here = all.iter().position(|&i| i == sel).unwrap_or(0);
    let order: Vec<usize> = (1..=n).map(|d| all[(here + d) % n]).collect();
    let pick = order
        .iter()
        .find(|&&i| needs(i))
        .or_else(|| order.iter().find(|&&i| attention(i)))
        .copied();
    pick.filter(|&i| i != sel || attention(i))
}

impl App {
    /// After `j` or `g`: says why a panel turned on doesn't show for the selected ticket.
    fn explain_panel(&mut self, tracker: bool) {
        let Some(tv) = self.selected().filter(|tv| tv.t.agent.is_none()) else { return };
        let on = if tracker { self.prefs.show_jira } else { self.prefs.show_gh };
        if !on || if tracker { self.panels().0 } else { self.panels().1 } {
            return;
        }
        let msg = if tracker {
            format!(
                "This project has no tracker; connect one in its settings ({} ,)",
                keys::prefix()
            )
        } else if !self.github.contains_key(&tv.t.repo) {
            "The PR panel shows once gh is checked, in a moment".to_string()
        } else {
            format!("No PR panel: {}", self.gh(&tv.t.repo).why())
        };
        self.warn(msg);
    }

    /// Says why the sidebar (or the panels) just turned on doesn't show: the screen is too narrow
    /// for it beside the session.
    fn explain_narrow(&mut self, panels: bool) {
        let f = self.areas.footer;
        let squeezed = |a: &ui::Areas| if panels { a.panels_hidden } else { a.sidebar_folded };
        let at = |w: u16| ui::layout(Rect::new(0, 0, w, f.bottom()), &self.prefs, self.panels());
        if (!panels && self.prefs.sidebar_collapsed) || !squeezed(&at(f.width)) {
            return;
        }
        let what = if panels { "The panels show" } else { "The sidebar unfolds" };
        if let Some(w) = (f.width..=1000).find(|&w| !squeezed(&at(w))) {
            self.warn(format!(
                "{what} beside the session from {w} columns wide; this window is {}",
                f.width
            ));
        }
    }

    fn next_attention(&mut self) {
        let all = self.ordered(false);
        let needs = |i: usize| self.tickets[i].status.state == AgentState::Needs;
        match next_attention_in(&all, self.sel, needs, |i| self.tickets[i].needs_attention()) {
            Some(i) => self.select(i),
            None => self.toast("Nothing is waiting on you"),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.selection_key(&key) {
            return;
        }
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.dismiss_error();
        // A key ends a drag under way; Esc is all it does.
        if self.cancel_drag(key.code == KeyCode::Esc) {
            return;
        }
        // `d` deletes an account's keychain item with it, when it has one of its own.
        let delete_item = matches!(self.mode, Mode::ConfirmDeleteAccount(i)
            if key.code == KeyCode::Char('d') && self.own_keychain_item(i).is_some());
        match &mut self.mode {
            Mode::Normal => {}
            Mode::Help(h) => {
                // Keys scroll Help when it doesn't fit; any other key closes it.
                let top = match key.code {
                    _ if h.max == 0 => None,
                    KeyCode::Up | KeyCode::Char('k') => Some(h.top.saturating_sub(1)),
                    KeyCode::Down | KeyCode::Char('j') => Some(h.top + 1),
                    KeyCode::PageUp => Some(h.top.saturating_sub(h.page)),
                    KeyCode::PageDown | KeyCode::Char(' ') => Some(h.top + h.page),
                    KeyCode::Home => Some(0),
                    KeyCode::End => Some(h.max),
                    _ => None,
                };
                match top {
                    Some(top) => h.top = top.min(h.max),
                    None => self.mode = Mode::Normal,
                }
                return;
            }
            Mode::Log(v) => {
                // Keys scroll the log when it doesn't fit; any other key closes it.
                if !v.key(key.code) {
                    self.mode = Mode::Normal;
                }
                return;
            }
            Mode::Form(_) => {
                self.form_key(key);
                return;
            }
            Mode::Settings(_) => {
                self.settings_key(key);
                return;
            }
            // `r` only when nothing would be lost (see `confirm_close`); `d` when only ignored
            // files would, which it deletes, as the dialog says. Each key only where it's offered.
            Mode::ConfirmClose
                if self.close_check.as_ref().is_some_and(|c| match key.code {
                    KeyCode::Char('r') => c.safe(),
                    KeyCode::Char('d') => c.safe_but_ignored(),
                    _ => false,
                }) =>
            {
                let check = self.close_check.take();
                let tv = self.tickets.get(self.sel);
                let name = tv.map(|tv| tv.t.display().to_string()).unwrap_or_default();
                let id = tv.map(|tv| (tv.t.repo.clone(), tv.t.key.clone()));
                let merged = tv.and_then(merged_head);
                // Closing stops the session first, and `remove` checks again: it may have
                // committed, or written ignored files the dialog didn't list, while it was open.
                match (check, id, self.close_at(self.sel)) {
                    (_, _, Err(o)) => self.warn(left_open(&name, &o)),
                    (Some(check), Some((repo, _)), Ok(Closed { hold: Ok(hold), reaper })) => {
                        let tx = self.tx.clone();
                        let removal = std::thread::spawn(move || {
                            // The CLI ran in the folder: gone first, then git deletes it.
                            if let Some(reaper) = reaper {
                                reaper.wait();
                            }
                            let res = close::remove(&repo, &check, merged.as_deref(), &hold);
                            // Nothing could start the session until now: see `close_at`.
                            drop(hold);
                            let _ = tx.send(Msg::Removed(res));
                        });
                        self.removing.push((vec![name], removal));
                    }
                    // Closed, but nothing reserves the folder: it stays, as `close_at` says.
                    (Some(_), Some(id), Ok(Closed { hold: Err(why), reaper })) => {
                        self.keep_reaper(id, reaper);
                        self.warn(format!("Kept the worktree: {why}"));
                    }
                    (_, Some(id), Ok(Closed { reaper, .. })) => self.keep_reaper(id, reaper),
                    (_, None, Ok(_)) => {}
                }
                self.mode = Mode::Normal;
                return;
            }
            Mode::ConfirmDeleteAccount(i) if delete_item => {
                let i = *i;
                self.delete_account(i, true);
                self.open_settings(
                    SettingsItem::Account(i.min(self.config.accounts.len().saturating_sub(1))),
                    false,
                );
                return;
            }
            // A dialog waits for Enter (yes) or Esc (cancel), so a stray key can't answer it.
            Mode::ConfirmClose
            | Mode::ConfirmDeleteProject(_)
            | Mode::ConfirmDeleteAccount(_)
            | Mode::ConfirmQuit
            | Mode::ConfirmRestart(_)
                if !matches!(key.code, KeyCode::Enter | KeyCode::Esc) =>
            {
                return;
            }
            Mode::ConfirmAgent(ask) => {
                // A role that doesn't fit scrolls at once, as scrolling answers nothing.
                if ask.view.key(key.code)
                    || !matches!(key.code, KeyCode::Enter | KeyCode::Esc)
                    || ask.shown.elapsed() < ASK_ARMS
                {
                    return;
                }
                let Mode::ConfirmAgent(ask) = std::mem::replace(&mut self.mode, Mode::Normal)
                else {
                    unreachable!()
                };
                self.answer_agent(&ask, key.code == KeyCode::Enter);
                return;
            }
            Mode::Links(_) => {
                self.links_key(key);
                return;
            }
            Mode::Jump(_) => {
                self.jump_key(key);
                return;
            }
            Mode::Queue(_) => {
                self.queue_key(key);
                return;
            }
            Mode::Move(_) => {
                self.move_key(key);
                return;
            }
            Mode::OfferTmux(_) if !matches!(key.code, KeyCode::Enter | KeyCode::Esc) => return,
            Mode::OfferTmux(_) => {
                let Mode::OfferTmux(id) = std::mem::replace(&mut self.mode, Mode::Normal) else {
                    unreachable!()
                };
                if key.code == KeyCode::Enter {
                    self.open_in_tmux(&id);
                }
                return;
            }
            Mode::ConfirmClose => {
                // Closed without removing the worktree, so the hold `close_at` returns goes now.
                if key.code == KeyCode::Enter {
                    let tv = self.tickets.get(self.sel);
                    let name = tv.map(|tv| tv.t.display().to_string()).unwrap_or_default();
                    let id = tv.map(|tv| (tv.t.repo.clone(), tv.t.key.clone()));
                    match (id, self.close_at(self.sel)) {
                        (_, Err(o)) => self.warn(left_open(&name, &o)),
                        (Some(id), Ok(closed)) => self.keep_reaper(id, closed.reaper),
                        (None, Ok(_)) => {}
                    }
                }
                self.close_check = None;
                self.close_unchecked = None;
                self.mode = Mode::Normal;
                return;
            }
            Mode::ConfirmDeleteProject(p) => {
                let p = *p;
                if key.code == KeyCode::Enter {
                    self.delete_project(p);
                    self.open_settings(
                        SettingsItem::Project(p.min(self.projects.len().saturating_sub(1))),
                        false,
                    );
                } else {
                    self.open_settings(SettingsItem::Project(p), false);
                }
                return;
            }
            Mode::ConfirmDeleteAccount(i) => {
                let i = *i;
                if key.code == KeyCode::Enter {
                    self.delete_account(i, false);
                    self.open_settings(
                        SettingsItem::Account(i.min(self.config.accounts.len().saturating_sub(1))),
                        false,
                    );
                } else {
                    self.open_settings(SettingsItem::Account(i), false);
                }
                return;
            }
            Mode::ConfirmQuit => {
                if key.code == KeyCode::Enter {
                    self.request_quit();
                }
                self.mode = Mode::Normal;
                return;
            }
            Mode::ConfirmRestart(_) => {
                let Mode::ConfirmRestart(id) = std::mem::replace(&mut self.mode, Mode::Normal)
                else {
                    unreachable!()
                };
                // By its id: the list may have changed under the dialog.
                if key.code == KeyCode::Enter
                    && let Some(i) = self.tickets.iter().position(|tv| tv.t.is(&id.0, &id.1))
                {
                    self.restart_with_toast(i);
                }
                return;
            }
        }

        let is_prefix = keys::prefix().matches(&key);
        if self.prefix {
            self.prefix = false;
            self.command(key);
            return;
        }
        if is_prefix {
            self.prefix = true;
            return;
        }
        if self.tickets.is_empty() {
            match key.code {
                KeyCode::Char('n') => self.open_ticket_form(self.current_project()),
                KeyCode::Char('P') => self.open_settings(SettingsItem::NewProject, true),
                KeyCode::Char('A') => self.open_settings(SettingsItem::NewAccount, true),
                KeyCode::Char(',') => {
                    let p = self.current_project().unwrap_or(0);
                    self.open_settings(SettingsItem::Project(p), false);
                }
                KeyCode::Char('q') => self.request_quit(),
                _ => {}
            }
            return;
        }
        let tv = &mut self.tickets[self.sel];
        if let Some(s) = tv.session.as_ref().filter(|s| !s.exited()) {
            tv.status.key(tv.t.cli, &key, || s.screen_count("Interrupted"));
        }
        match &tv.session {
            Some(s) if !s.exited() => {
                s.reset_scroll();
                if let Some(bytes) = keys::encode(&key, s.application_cursor())
                    && !s.write(&bytes)
                {
                    self.warn(NOT_SENT);
                }
            }
            _ if key.code == KeyCode::Enter && tv.t.pending_worktree.is_some() => {
                if let Some(p) = &mut tv.t.pending_worktree {
                    p.error = None;
                }
            }
            _ if key.code == KeyCode::Enter
                && matches!(tv.away, Some(Away::HandedOff | Away::Back)) =>
            {
                self.bring_back();
            }
            // Running in another terminal, or on its way there.
            _ if tv.away.is_some() => {}
            _ if key.code == KeyCode::Enter && tv.background.is_some() => {
                self.stop_background_copy();
            }
            _ if key.code == KeyCode::Enter => self.restart_session(self.sel),
            _ => {}
        }
    }

    /// What a key does to the selection (see `selection::key_action`); true when it used the key
    /// up, so it neither reaches the session nor runs a command.
    fn selection_key(&mut self, key: &KeyEvent) -> bool {
        if self.selection.is_none() {
            return false;
        }
        let action = if matches!(self.mode, Mode::Normal) && !self.prefix {
            selection::key_action(key)
        } else {
            selection::KeyAction::Forward
        };
        match action {
            selection::KeyAction::Keep => false,
            selection::KeyAction::Forward => {
                self.selection = None;
                false
            }
            selection::KeyAction::Clear => {
                self.selection = None;
                true
            }
            selection::KeyAction::Copy => {
                let text = self.selection_text();
                self.selection = None;
                if text.is_empty() {
                    self.warn("Nothing to copy: the selection is blank");
                } else {
                    self.copy_text(text);
                }
                true
            }
        }
    }

    fn command(&mut self, key: KeyEvent) {
        let prefix = keys::prefix();
        match key.code {
            _ if prefix.matches(&key) => self.next_attention(),
            KeyCode::Tab => self.next_attention(),
            // `a` whatever the prefix, as in screen: the prefix's own letter may be a command.
            KeyCode::Char('a') => {
                if let Some(s) = self.tickets.get(self.sel).and_then(|tv| tv.session.as_ref())
                    && !s.write(&[prefix.byte()])
                {
                    self.warn(NOT_SENT);
                }
            }
            KeyCode::Char('{') => self.start_move(-1),
            KeyCode::Char('}') => self.start_move(1),
            KeyCode::Up | KeyCode::Char('[') => self.step(-1),
            KeyCode::Down | KeyCode::Char(']') => self.step(1),
            KeyCode::Char('n') => self.open_ticket_form(self.current_project()),
            KeyCode::Char('/') => self.open_jump(),
            KeyCode::Char('w') => self.open_queue(),
            KeyCode::Char('P') => self.open_settings(SettingsItem::NewProject, true),
            KeyCode::Char(',') => {
                self.check_github(self.projects.iter().map(|p| p.repo.clone()).collect());
                let p = self.current_project().unwrap_or(0);
                self.open_settings(SettingsItem::Project(p), false);
            }
            KeyCode::Char('A') => self.open_settings(SettingsItem::NewAccount, true),
            KeyCode::Char('z') => {
                if let Some(p) = self.tickets.get(self.sel).and_then(|tv| self.project_of(&tv.t)) {
                    self.toggle_project(p);
                }
            }
            KeyCode::Char('x') if !self.tickets.is_empty() => self.confirm_close(),
            KeyCode::Char('b') => {
                self.toggle(|p| &mut p.sidebar_collapsed);
                self.explain_narrow(false);
            }
            KeyCode::Char(c @ ('j' | 'c' | 'g'))
                if self.selected().is_some_and(|tv| tv.t.agent.is_some()) =>
            {
                self.toggle_on_agent(c);
            }
            KeyCode::Char('j') => {
                self.toggle(|p| &mut p.show_jira);
                self.explain_panel(true);
                self.explain_narrow(true);
            }
            KeyCode::Char('c') => self.toggle(|p| &mut p.show_comments),
            KeyCode::Char('g') => {
                self.toggle(|p| &mut p.show_gh);
                self.explain_panel(false);
                self.explain_narrow(true);
            }
            KeyCode::Char('o') => self.open_ticket(),
            KeyCode::Char('p') => self.open_pr(),
            KeyCode::Char('r') => self.refresh_selected(),
            KeyCode::Char('m') => self.mark_read(),
            KeyCode::Char('M') if !self.tickets.is_empty() => self.use_main_checkout(),
            KeyCode::Char('e') => self.open_rename_form(),
            KeyCode::Char('R') if !self.tickets.is_empty() => {
                match self.tickets[self.sel].away {
                    Some(Away::Outside | Away::Leaving(_)) => {
                        self.warn("It's running in another terminal: quit it there, then press Enter here");
                    }
                    Some(Away::HandedOff | Away::Back) => self.bring_back(),
                    // A turn under way, or a question to the user, would be lost: ask first.
                    None if self.tickets[self.sel].busy() => {
                        let t = &self.tickets[self.sel].t;
                        self.mode = Mode::ConfirmRestart((t.repo.clone(), t.key.clone()));
                    }
                    None => self.restart_with_toast(self.sel),
                }
            }
            KeyCode::Char('t') if !self.tickets.is_empty() => self.hand_off(),
            KeyCode::Char('v') => self.open_history(),
            KeyCode::Char('l') => self.open_links(),
            KeyCode::Char('~') => self.open_log(),
            KeyCode::PageUp => self.page_claude(true),
            KeyCode::PageDown => self.page_claude(false),
            KeyCode::Char('?' | 'h') => self.mode = Mode::Help(HelpView::default()),
            KeyCode::Char('q') => self.mode = Mode::ConfirmQuit,
            KeyCode::Esc => {}
            other => {
                self.warn(format!("{prefix} {other} isn't a command · {prefix} ? lists them"));
            }
        }
    }

    /// When the selected session is an agent, which has no ticket, PR, comments or panels of its
    /// own, says it has no `what` and returns true.
    fn agent_has_no(&mut self, what: &str) -> bool {
        let agent = self.selected().is_some_and(|tv| tv.t.agent.is_some());
        if agent {
            self.warn(format!("Agents have no {what}"));
        }
        agent
    }

    /// `j`, `c` or `g` on an agent: they change the panels tickets show, and the board shows in
    /// an agent's while either panel is on. Without a board, they'd change nothing in view.
    fn toggle_on_agent(&mut self, c: char) {
        let Some(tv) = self.selected() else { return };
        if !self.agent_board(tv) {
            self.agent_has_no("panels");
            return;
        }
        let board = |p: &UiPrefs| p.show_jira || p.show_gh;
        let before = board(&self.prefs);
        let (what, on) = match c {
            'j' => {
                self.toggle(|p| &mut p.show_jira);
                ("Tracker panels", self.prefs.show_jira)
            }
            'g' => {
                self.toggle(|p| &mut p.show_gh);
                ("PR panels", self.prefs.show_gh)
            }
            _ => {
                self.toggle(|p| &mut p.show_comments);
                ("Comments in tracker panels", self.prefs.show_comments)
            }
        };
        let now = board(&self.prefs);
        let after = match (before, now) {
            (true, false) => "; the board hides while both are off",
            (false, true) => "; the board shows here",
            _ => "",
        };
        self.toast(format!("{what} {} for tickets{after}", if on { "on" } else { "off" }));
        if !before && now {
            self.explain_narrow(true);
        }
    }

    fn mark_read(&mut self) {
        if self.agent_has_no("comments to mark as read") {
            return;
        }
        let prefs = self.prefs.clone();
        let Some(tv) = self.tickets.get_mut(self.sel) else { return };
        let before = tv.new_count();
        tv.mark_seen(&prefs);
        let left = tv.new_count();
        self.save();
        match (before, left) {
            (0, _) => self.toast("Nothing new on this ticket"),
            (_, 0) => self.toast("Marked as read"),
            (_, n) => self.toast(format!("{n} still new in hidden panels")),
        }
    }

    fn toggle(&mut self, f: impl Fn(&mut UiPrefs) -> &mut bool) {
        let v = f(&mut self.prefs);
        *v = !*v;
        self.save();
    }

    fn page_claude(&mut self, up: bool) {
        let half = i32::from(self.areas.claude.height) / 2;
        let Some(s) = self.tickets.get(self.sel).and_then(|tv| tv.session.as_ref()) else { return };
        if s.wants_mouse() {
            if !s.write(if up { b"\x1b[5~" } else { b"\x1b[6~" }) {
                self.warn(NOT_SENT);
            }
        } else {
            s.scroll(if up { half } else { -half });
        }
    }

    fn open_ticket(&mut self) {
        if self.agent_has_no("ticket to open") {
            return;
        }
        let Some(tv) = self.tickets.get(self.sel) else { return };
        if let Some(board) = self.access(&tv.t).board() {
            match local::find(board, &tv.t.key) {
                Ok(file) => self.edit = Some(file.path),
                Err(e) => self.error(e),
            }
            return;
        }
        match self.issue_url(&tv.t) {
            Some(url) => {
                if !(self.open_url)(&url) {
                    self.warn(format!("Could not open {url}"));
                }
            }
            None => self.warn("This ticket's project has no tracker set"),
        }
    }

    fn open_pr(&mut self) {
        if self.agent_has_no("PR to open") {
            return;
        }
        let Some(tv) = self.tickets.get(self.sel) else { return };
        match tv.pr.data.as_ref().and_then(|p| p.as_ref()) {
            Some(pr) => {
                let url = pr.url.clone();
                if !(self.open_url)(&url) {
                    self.warn(format!("Could not open {url}"));
                }
            }
            None => self.warn("No PR found for this ticket yet"),
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        match &mut self.mode {
            Mode::Form(form) if matches!(form.kind, FormKind::Ticket) => {
                self.paste_into_add_form(text);
            }
            Mode::Form(form) => paste_into(form, text),
            Mode::Settings(s) if s.editing => paste_into(&mut s.form, text),
            Mode::Normal if !self.prefix => {
                // Like a key, a paste ends the selection.
                self.selection = None;
                if let Some(s) = self.tickets.get(self.sel).and_then(|tv| tv.session.as_ref()) {
                    s.reset_scroll();
                    if !s.paste(text) {
                        self.warn(NOT_SENT);
                    }
                }
            }
            _ => {}
        }
    }

    /// Lists the ticket's links (the issue, its PR, checks, reviews, URLs in their text) and the
    /// session's (`prefix l`): for terminals that don't open the links drawn in the panes, links
    /// scrolled away, and the keyboard.
    fn open_links(&mut self) {
        let Some(tv) = self.selected() else { return };
        let mut links = match tv.t.agent {
            Some(_) => Vec::new(),
            None => {
                let issue_url = self.issue_url(&tv.t);
                let pr = tv.pr.data.as_ref().and_then(Option::as_ref);
                links::panel(&tv.t.key, issue_url.as_deref(), tv.jira.data.as_ref(), pr)
            }
        };
        let panel = links.len();
        if let Some(s) = &tv.session {
            for link in links::list(crate::session::lock(&s.parser).screen_mut()) {
                if !links[..panel].iter().any(|l| l.url == link.url) {
                    links.push(link);
                }
            }
        }
        match (links.is_empty(), &tv.session) {
            (true, None) => self.warn("No links yet, and no session: press Enter to start it"),
            (true, Some(_)) => self.warn("No links in this session"),
            (false, _) => {
                self.mode = Mode::Links(Box::new(LinkList { links, sel: 0, panel, top: 0 }));
            }
        }
    }

    fn links_key(&mut self, key: KeyEvent) {
        let Mode::Links(list) = &mut self.mode else { return };
        let last = list.links.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => list.sel = list.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => list.sel = (list.sel + 1).min(last),
            KeyCode::Home => list.sel = 0,
            KeyCode::End => list.sel = last,
            KeyCode::Enter => {
                let url = list.links[list.sel].url.clone();
                if !links::openable(&url) {
                    self.warn("Only http(s) links open: c copies it");
                    return;
                }
                self.mode = Mode::Normal;
                if (self.open_url)(&url) {
                    self.toast(format!("Opened {}", util::truncate(&untrusted::line(&url), 80)));
                } else {
                    self.warn("No browser found: set $BROWSER, or c copies the link");
                }
            }
            KeyCode::Char('c') => {
                let url = untrusted::line(&list.links[list.sel].url);
                self.mode = Mode::Normal;
                self.copy_as(url, Some("the link".into()));
            }
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
    }

    /// Opens the session's history and screen in `$EDITOR`, for text too long to select or kept
    /// out of reach on a full-screen CLI's own screen.
    fn open_history(&mut self) {
        let Some(tv) = self.selected() else { return };
        let Some(s) = tv.session.as_ref() else {
            self.warn("No session to open: press Enter to start it");
            return;
        };
        let (text, alternate) = {
            let mut p = crate::session::lock(&s.parser);
            (selection::session_text(p.screen_mut()), p.screen().alternate_screen())
        };
        let path = store::history_file(&tv.t.repo, &tv.t.key);
        let cli = tv.t.cli.label();
        match write_private(&path, &text) {
            Ok(()) => {
                self.edit = Some(path);
                if alternate {
                    self.toast(format!(
                        "Only the screen: {cli} keeps its history itself in full-screen mode"
                    ));
                }
            }
            Err(e) => self.error(format!("Couldn't write {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goes_to_what_waits_on_the_user_next() {
        let all = [3, 0, 1, 2];
        let these = |set: &'static [usize]| move |i: usize| set.contains(&i);
        // From 0, in sidebar order after it and wrapping: what needs you first, then what wants a
        // look.
        assert_eq!(next_attention_in(&all, 0, these(&[3]), these(&[1, 3])), Some(3));
        assert_eq!(next_attention_in(&all, 0, these(&[]), these(&[2, 3])), Some(2));
        assert_eq!(next_attention_in(&all, 2, these(&[]), these(&[0, 1])), Some(0));
        // The selected ticket comes last, and only while it wants a look.
        assert_eq!(next_attention_in(&all, 0, these(&[0]), these(&[0])), Some(0));
        assert_eq!(next_attention_in(&all, 0, these(&[0]), these(&[])), None);
        assert_eq!(next_attention_in(&all, 0, these(&[]), these(&[])), None);
        assert_eq!(next_attention_in(&[], 0, these(&[]), these(&[])), None);
    }
}
