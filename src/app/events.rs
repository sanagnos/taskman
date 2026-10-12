//! The App driven as the event loop drives it: keys, mouse reports, pastes and the messages
//! threads send, on the mock project of `test_support::acme_app`, with sessions that run nothing
//! (`Session::fake`). Each test checks what the user would see or what reaches a session: the mode,
//! the selection, the bytes sent, the dialogs, the toasts and what was saved.

use super::sessions::HAND_OFF_WAIT;
use super::test_support::{
    acme_app, ago, answer_choices, attach, attach_slow, ctrl, draw, key, lay_out, long_sidebar,
    offline, reaped, sent, settle, toast,
};
use super::{AgentState, App, Away, Hit, Mode, sync::Msg};
use crate::store::{self, Saved};
use crate::{add, resume};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

const SCREEN: &str = "✻ Welcome to Claude Code\n\n> Add the magic link sign-in\n\n> ";

fn index(app: &App, key: &str) -> usize {
    app.tickets.iter().position(|tv| tv.t.key == key).unwrap()
}

fn selected(app: &App) -> &str {
    &app.tickets[app.sel].t.key
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    app.on_mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
}

fn prefix(app: &mut App, code: KeyCode) {
    app.on_key(ctrl('a'));
    app.on_key(key(code));
}

/// tickets.json as `save` last wrote it.
fn saved() -> Saved {
    store::read_json(&store::state_dir().join("tickets.json")).unwrap()
}

fn decoded(clipboard: &[String]) -> Vec<String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    clipboard.iter().map(|c| String::from_utf8(b64.decode(c).unwrap()).unwrap()).collect()
}

/// ENG-52 on an 80 by 24 screen, its session showing `SCREEN`.
fn at_eng_52() -> (App, crate::session::fake::Sent) {
    let mut app = acme_app();
    draw(&mut app, 80, 24);
    let sent = attach(&mut app, "ENG-52", SCREEN);
    draw(&mut app, 80, 24);
    (app, sent)
}

/// Drags across the first `cells` cells of the Claude pane's top row.
fn select_top_row(app: &mut App, cells: u16) {
    let c = app.areas.claude;
    mouse(app, MouseEventKind::Down(MouseButton::Left), c.x, c.y);
    mouse(app, MouseEventKind::Drag(MouseButton::Left), c.x + cells - 1, c.y);
    mouse(app, MouseEventKind::Up(MouseButton::Left), c.x + cells - 1, c.y);
}

// ---- keys and the session ------------------------------------------------------------------

#[test]
fn keys_and_pastes_reach_the_session() {
    let (mut app, to_session) = at_eng_52();
    app.on_key(key(KeyCode::Char('y')));
    app.on_key(key(KeyCode::Enter));
    app.on_key(ctrl('c'));
    app.on_key(key(KeyCode::Esc));
    // The prefix's own letter after it sends the prefix itself.
    prefix(&mut app, KeyCode::Char('a'));
    app.on_paste("one\ntwo");
    assert_eq!(sent(&to_session), "y\r\x03\x1b\x01one\rtwo");
    assert!(matches!(app.mode, Mode::Normal));

    // A CLI that asked for bracketed paste gets the text as it is, marked.
    let s = app.tickets[app.sel].session.as_ref().unwrap();
    s.output(b"\x1b[?2004h");
    app.on_paste("one\ntwo");
    assert!(sent(&to_session).ends_with("\x1b[200~one\ntwo\x1b[201~"));
}

#[test]
fn prefix_commands_never_reach_the_session() {
    let (mut app, to_session) = at_eng_52();
    prefix(&mut app, KeyCode::Char('4'));
    assert_eq!(selected(&app), "ENG-53");
    prefix(&mut app, KeyCode::Char(']'));
    assert_eq!(selected(&app), "ENG-61");
    prefix(&mut app, KeyCode::Char('['));
    prefix(&mut app, KeyCode::Char('['));
    assert_eq!(selected(&app), "ENG-52");
    prefix(&mut app, KeyCode::Char('?'));
    assert!(matches!(app.mode, Mode::Help(_)));
    app.on_key(key(KeyCode::Char('x')));
    assert!(matches!(app.mode, Mode::Normal));
    prefix(&mut app, KeyCode::Char('y'));
    assert_eq!(toast(&app), Some("ctrl+a y isn't a command · ctrl+a ? lists them"));
    assert_eq!(sent(&to_session), "");
    // Switching tickets saves which one is selected.
    assert_eq!(saved().selected, index(&app, "ENG-52"));
}

#[test]
fn the_prefix_twice_goes_to_what_waits_on_the_user() {
    let mut app = acme_app();
    // ENG-53 and notes' writer need the user; ENG-53 comes first after ENG-52.
    app.on_key(ctrl('a'));
    app.on_key(ctrl('a'));
    assert_eq!(selected(&app), "ENG-53");
    app.on_key(ctrl('a'));
    app.on_key(key(KeyCode::Tab));
    assert_eq!(selected(&app), "notes-writer");
    // Selecting a ticket in a folded project unfolds it.
    assert!(!app.projects[1].collapsed);
}

#[test]
fn says_why_a_sidebar_turned_on_stays_folded() {
    let mut app = acme_app();
    app.prefs.sidebar_collapsed = true;
    draw(&mut app, 80, 24);
    prefix(&mut app, KeyCode::Char('b'));
    assert!(!app.prefs.sidebar_collapsed);
    assert_eq!(
        toast(&app),
        Some("The sidebar unfolds beside the session from 91 columns wide; this window is 80")
    );
    assert!(!saved().ui.sidebar_collapsed);
}

#[test]
fn help_scrolls_and_any_other_key_closes_it() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('?'));
    draw(&mut app, 80, 24);
    let Mode::Help(h) = app.mode else { panic!("no help") };
    assert!(h.max > 0, "help fits 80 by 24");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::End));
    let Mode::Help(h) = app.mode else { panic!("help closed") };
    assert_eq!(h.top, h.max);
    mouse(&mut app, MouseEventKind::ScrollUp, 40, 10);
    let Mode::Help(scrolled) = app.mode else { panic!("help closed") };
    assert_eq!(scrolled.top, h.max - 3);
    app.on_key(key(KeyCode::Char('q')));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn quitting_asks_first_and_saves() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('q'));
    assert!(matches!(app.mode, Mode::ConfirmQuit));
    // A stray key doesn't answer it.
    app.on_key(key(KeyCode::Char('y')));
    assert!(matches!(app.mode, Mode::ConfirmQuit) && !app.quit);
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal) && !app.quit);
    prefix(&mut app, KeyCode::Char('q'));
    app.on_key(key(KeyCode::Enter));
    assert!(app.quit && app.may_quit());
    assert_eq!(saved().tickets.len(), app.tickets.len());
}

/// A signal, or the terminal going, quits through the same path as the dialog's Enter.
#[test]
fn a_quit_requested_without_the_dialog_saves_and_stops_the_sessions() {
    let (mut app, _sent) = at_eng_52();
    app.request_quit();
    assert!(app.quit && app.may_quit());
    // What was on screen counts as seen, as after the dialog, and that is what's saved.
    assert_eq!(app.tickets[app.sel].new_count(), 0);
    assert_eq!(saved().tickets[app.sel].seen_gh, app.tickets[app.sel].t.seen_gh);
    assert!(app.tickets[app.sel].session.is_some());
    app.stop_sessions();
    assert!(app.tickets.iter().all(|tv| tv.session.is_none()));
    // Asked again, nothing more happens.
    app.request_quit();
    assert!(app.quit);
}

// ---- selecting text ------------------------------------------------------------------------

#[test]
fn a_selection_is_copied_once_the_mouse_is_released() {
    let (mut app, to_session) = at_eng_52();
    select_top_row(&mut app, 9);
    assert!(app.selection.as_ref().is_some_and(|s| s.released));
    // Drawing the frame copies it.
    draw(&mut app, 80, 24);
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["✻ Welcome"]);
    assert!(toast(&app).is_some_and(|t| t.contains("9 characters")), "{:?}", toast(&app));
    assert!(app.selection.is_some(), "the selection stays after copying");
    assert_eq!(sent(&to_session), "");
}

#[test]
fn ctrl_c_copies_a_selection_without_interrupting() {
    let (mut app, to_session) = at_eng_52();
    select_top_row(&mut app, 3);
    app.on_key(ctrl('c'));
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["✻ W"]);
    assert!(app.selection.is_none());
    assert_eq!(sent(&to_session), "", "ctrl+c reached the session");
    // With nothing selected, it interrupts again.
    app.on_key(ctrl('c'));
    assert_eq!(sent(&to_session), "\x03");
}

#[test]
fn esc_drops_a_selection_and_other_keys_go_through() {
    let (mut app, to_session) = at_eng_52();
    select_top_row(&mut app, 3);
    app.on_key(key(KeyCode::Esc));
    assert!(app.selection.is_none());
    assert_eq!(sent(&to_session), "");
    select_top_row(&mut app, 3);
    app.on_key(key(KeyCode::Char('k')));
    assert!(app.selection.is_none());
    assert_eq!(sent(&to_session), "k");
    // A plain click selects nothing.
    let c = app.areas.claude;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), c.x + 2, c.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), c.x + 2, c.y);
    assert!(app.selection.is_none());
}

// ---- the mouse -----------------------------------------------------------------------------

#[test]
fn clicks_in_the_sidebar_select_tickets_and_fold_projects() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    let at =
        |app: &App, hit: super::Hit| app.sidebar_hits.iter().find(|(_, h)| *h == hit).unwrap().0;
    let card = at(&app, super::Hit::Ticket(index(&app, "ENG-61")));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), card.x + 3, card.y + 1);
    assert_eq!(selected(&app), "ENG-61");
    // The wheel over a panel scrolls it.
    let jira = app.areas.jira.unwrap();
    mouse(&mut app, MouseEventKind::ScrollDown, jira.x + 3, jira.y + 3);
    assert_eq!(app.tickets[app.sel].jira_scroll, 2);
    draw(&mut app, 160, 40);
    let notes = at(&app, super::Hit::Project(1));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), notes.x + 1, notes.y);
    assert!(!app.projects[1].collapsed);
    assert!(!saved().projects[1].collapsed);
}

/// Where the sidebar draws `key`'s card, if it does.
fn card(app: &App, key: &str) -> Option<ratatui::layout::Rect> {
    let hit = Hit::Ticket(index(app, key));
    app.sidebar_hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r)
}

/// The sidebar's list sits under its title and leaves the last row blank.
fn list_rows(app: &App) -> std::ops::Range<u16> {
    let s = app.areas.sidebar;
    s.y + 2..s.bottom() - 1
}

#[test]
fn selecting_a_card_below_the_fold_scrolls_it_into_view() {
    let mut app = long_sidebar(20);
    let screen = draw(&mut app, 160, 40);
    assert!(card(&app, "ENG-90").is_none(), "below the fold");
    assert!(screen.contains("↓ "), "the last row says more is below");
    assert_eq!(app.sidebar_scroll.top, 0);

    app.select(index(&app, "ENG-90"));
    let screen = draw(&mut app, 160, 40);
    let rows = list_rows(&app);
    let r = card(&app, "ENG-90").expect("scrolled into view");
    assert!(rows.contains(&r.y) && r.bottom() <= rows.end && r.height == 3, "shown whole: {r:?}");
    assert!(screen.contains("↑ "), "the top row says more is above");
    let top = app.sidebar_scroll.top;
    assert!(top > 0);
    // Stepping to the card above, in view already, moves nothing.
    prefix(&mut app, KeyCode::Char('['));
    draw(&mut app, 160, 40);
    assert_eq!(selected(&app), "ENG-89");
    assert_eq!(app.sidebar_scroll.top, top);
    // Stepping up past the top edge brings each card in by its own rows, not to the header:
    // ENG-82 had two rows hidden, ENG-81 all four, blank row included.
    for _ in 0..7 {
        prefix(&mut app, KeyCode::Char('['));
    }
    draw(&mut app, 160, 40);
    assert_eq!(selected(&app), "ENG-82");
    assert_eq!(app.sidebar_scroll.top, top - 2);
    prefix(&mut app, KeyCode::Char('['));
    draw(&mut app, 160, 40);
    assert_eq!(selected(&app), "ENG-81");
    assert_eq!(app.sidebar_scroll.top, top - 6);
    // The first card: back to the top, header and all.
    app.select(index(&app, "acme-lead"));
    let screen = draw(&mut app, 160, 40);
    assert_eq!(app.sidebar_scroll.top, 0);
    assert!(!screen.contains("↑ "));
}

#[test]
fn the_wheel_scrolls_the_sidebar_and_clicks_land_where_cards_are_drawn() {
    let mut app = long_sidebar(20);
    draw(&mut app, 160, 40);
    let s = app.areas.sidebar;
    mouse(&mut app, MouseEventKind::ScrollDown, s.x + 2, s.y + 5);
    assert_eq!(app.sidebar_scroll.top, 2);
    assert_eq!(selected(&app), "ENG-52", "the wheel leaves the selection where it is");
    for _ in 0..10 {
        mouse(&mut app, MouseEventKind::ScrollDown, s.x + 2, s.y + 5);
    }
    let screen = draw(&mut app, 160, 40);
    assert_eq!(app.sidebar_scroll.top, 22);
    assert_eq!(selected(&app), "ENG-52");
    assert!(card(&app, "ENG-52").is_none(), "scrolled out of view, and stays selected");
    // Row 22 is ENG-71's first row, under the "more" row: its hit is the two rows drawn.
    let rows = list_rows(&app);
    let partial = card(&app, "ENG-71").unwrap();
    assert_eq!((partial.y, partial.height), (rows.start + 1, 2));
    // ENG-75's card is four cards further down, where the screen shows it.
    let r = card(&app, "ENG-75").unwrap();
    assert_eq!((r.y, r.height), (rows.start + 16, 3));
    assert!(screen.lines().nth(r.y as usize).unwrap().contains("ENG-75"));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 3, r.y + 1);
    assert_eq!(selected(&app), "ENG-75");
    draw(&mut app, 160, 40);
    assert_eq!(app.sidebar_scroll.top, 22, "in view: nothing moves");
    // A click on the card cut off at the top selects it, and it comes into view whole: by the
    // one row that was hidden, not up to its project's header.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), partial.x + 3, partial.y);
    assert_eq!(selected(&app), "ENG-71");
    draw(&mut app, 160, 40);
    assert_eq!(app.sidebar_scroll.top, 21);
    let r = card(&app, "ENG-71").unwrap();
    assert_eq!((r.y, r.height), (rows.start + 1, 3));
}

/// Scrolled out of view with the wheel, the selected card comes back when it's selected again,
/// by its number or a click on what shows of it; a redraw alone keeps the wheel's scroll.
#[test]
fn selecting_the_selected_card_again_brings_it_back_into_view() {
    let mut app = long_sidebar(20);
    draw(&mut app, 160, 40);
    let s = app.areas.sidebar;
    for _ in 0..11 {
        mouse(&mut app, MouseEventKind::ScrollDown, s.x + 2, s.y + 5);
    }
    draw(&mut app, 160, 40);
    assert_eq!((app.sidebar_scroll.top, selected(&app)), (22, "ENG-52"));
    assert!(card(&app, "ENG-52").is_none(), "scrolled out of view");
    draw(&mut app, 160, 40);
    assert_eq!(app.sidebar_scroll.top, 22, "a redraw keeps the wheel's scroll");
    // Its number: the list scrolls back just enough to show it, under the "more" row.
    prefix(&mut app, KeyCode::Char('3'));
    assert_eq!(selected(&app), "ENG-52");
    draw(&mut app, 160, 40);
    let rows = list_rows(&app);
    let r = card(&app, "ENG-52").expect("back in view");
    assert!(rows.contains(&r.y) && r.bottom() <= rows.end && r.height == 3, "shown whole: {r:?}");
    assert_eq!(r.y, rows.start + 1);
    let top = app.sidebar_scroll.top;
    // One notch hides its first rows; a click on what's left shows it whole again.
    mouse(&mut app, MouseEventKind::ScrollDown, s.x + 2, s.y + 5);
    draw(&mut app, 160, 40);
    let partial = card(&app, "ENG-52").expect("partly in view");
    assert_eq!((app.sidebar_scroll.top, partial.height), (top + 2, 1));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), partial.x + 3, partial.y);
    assert_eq!(selected(&app), "ENG-52");
    draw(&mut app, 160, 40);
    assert_eq!(app.sidebar_scroll.top, top);
    assert_eq!(card(&app, "ENG-52").unwrap().height, 3);
}

#[test]
fn sidebar_scrolling_stops_at_both_ends() {
    let mut app = long_sidebar(20);
    draw(&mut app, 160, 40);
    let s = app.areas.sidebar;
    mouse(&mut app, MouseEventKind::ScrollUp, s.x + 2, s.y + 5);
    draw(&mut app, 160, 40);
    assert_eq!(app.sidebar_scroll.top, 0);
    for _ in 0..100 {
        mouse(&mut app, MouseEventKind::ScrollDown, s.x + 2, s.y + 5);
    }
    let screen = draw(&mut app, 160, 40);
    let end = app.sidebar_scroll.top;
    let rows = list_rows(&app);
    let last = card(&app, "ENG-90").unwrap();
    assert_eq!((last.bottom(), last.height), (rows.end - 2, 3), "the list ends on the last row");
    assert!(screen.contains("↑ ") && !screen.contains("↓ "), "{screen}");
    mouse(&mut app, MouseEventKind::ScrollDown, s.x + 2, s.y + 5);
    draw(&mut app, 160, 40);
    assert_eq!(app.sidebar_scroll.top, end);
    // A taller screen fits more: the position follows, so no room opens at the bottom.
    draw(&mut app, 160, 60);
    assert!(app.sidebar_scroll.top < end);
    assert_eq!(card(&app, "ENG-90").unwrap().bottom(), list_rows(&app).end - 2);
    // Folding the project leaves a list that fits.
    prefix(&mut app, KeyCode::Char('z'));
    let screen = draw(&mut app, 160, 60);
    assert_eq!(app.sidebar_scroll.top, 0);
    assert!(!screen.contains("↑ ") && !screen.contains("↓ "));
}

// ---- closing a ticket ----------------------------------------------------------------------

#[test]
fn closing_asks_and_keeps_a_worktree_it_can_not_check() {
    let (mut app, _) = at_eng_52();
    prefix(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    // No git worktree there to check: nothing is offered for removal, and `r` does nothing.
    assert!(app.close_check.is_none());
    app.on_key(key(KeyCode::Char('r')));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.tickets.len(), 7);

    let before = app.sel;
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    assert_eq!(app.sel, before, "the next ticket takes its place");
    assert!(!saved().tickets.iter().any(|t| t.key == "ENG-52"));
    // Nothing holds its session any more.
    assert!(matches!(
        resume::peek(&resume::owner_file("/src/acme".as_ref(), "ENG-52")),
        resume::Owner::Free
    ));
}

/// The dialog says when the worktree couldn't be checked (git failed), and keeps it, as `taskman
/// close` says why in its reply.
#[test]
fn closing_says_when_the_worktree_could_not_be_checked() {
    // A worktree whose repo isn't a git repository: `git worktree list` fails there.
    let root = store::scratch::dir().join("nogit");
    let (repo, wt) = (root.join("acme"), root.join("acme-ENG-52"));
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    let (mut app, _) = at_eng_52();
    let tv = &mut app.tickets[app.sel];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo, wt.clone(), true);
    prefix(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    assert!(app.close_check.is_none());
    assert_eq!(
        app.close_unchecked.as_deref(),
        Some("git worktree failed: not a git repository (or any of the parent directories): .git")
    );
    let screen = draw(&mut app, 80, 24);
    assert!(
        screen.contains("couldn't be checked (git worktree failed: not a git repository"),
        "{screen}"
    );
    // Nothing is offered: `r` does nothing, Enter closes and keeps the folder.
    app.on_key(key(KeyCode::Char('r')));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.close_unchecked.is_none());
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    assert!(wt.exists());
}

/// Runs `cmd` in `dir` with a throwaway git identity.
fn sh(dir: &std::path::Path, cmd: &str) {
    let ok = std::process::Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .unwrap()
        .success();
    assert!(ok, "{cmd}");
}

#[test]
fn closing_deletes_ignored_files_only_when_asked() {
    // ENG-52 in a worktree of a real repo: pushed and merged, with nothing but an ignored .env.
    let root = store::scratch::dir().join("git");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    sh(&root, "git init -q --bare -b main origin.git && git clone -q origin.git acme 2>/dev/null");
    let (repo, wt) = (root.join("acme"), root.join("acme-ENG-52"));
    sh(
        &repo,
        "echo .env > .gitignore && git add .gitignore && git commit -q -m one && git push -q origin main \
         && git remote set-head origin main && git worktree add -q -b eng-52 ../acme-ENG-52 origin/main",
    );
    sh(&wt, "echo SECRET=1 > .env");
    let (mut app, _) = at_eng_52();
    let tv = &mut app.tickets[app.sel];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo.clone(), wt.clone(), true);

    // The dialog lists it; `r`, which removes only what loses nothing, isn't offered.
    prefix(&mut app, KeyCode::Char('x'));
    let check = app.close_check.as_ref().unwrap();
    assert_eq!((check.ignored.as_slice(), check.safe_but_ignored()), (&[".env".into()][..], true));
    app.on_key(key(KeyCode::Char('r')));
    assert!(matches!(app.mode, Mode::ConfirmClose));
    app.on_key(key(KeyCode::Esc));

    // `d` closes it and removes the worktree with its .env, and the merged branch.
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    settle(&mut app);
    assert_eq!(
        toast(&app).map(String::from),
        Some(format!("Removed {} and branch eng-52", store::tilde(&wt)))
    );
    assert!(!wt.exists());
}

/// A clone `acme` of a bare origin in the test's scratch folder, with ENG-52's worktree beside it
/// on a branch at origin's main: nothing to lose, so closing may remove it. Returns both folders.
fn merged_worktree() -> (std::path::PathBuf, std::path::PathBuf) {
    let root = store::scratch::dir().join("git");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    sh(&root, "git init -q --bare -b main origin.git && git clone -q origin.git acme 2>/dev/null");
    let (repo, wt) = (root.join("acme"), root.join("acme-ENG-52"));
    sh(
        &repo,
        "git commit -q --allow-empty -m one && git push -q origin main && git remote set-head origin main \
         && git worktree add -q -b eng-52 ../acme-ENG-52 origin/main",
    );
    (repo, wt)
}

/// Makes `key`'s owner lock in the project at `repo` impossible to take, as a state folder that
/// can't be written would: a file sits where the lock's folder should be.
fn break_lock(repo: &std::path::Path, key: &str) {
    let owner = resume::owner_file(repo, key);
    let dir = owner.parent().unwrap();
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    std::fs::write(dir, "").unwrap();
}

/// Removal holds the ticket's owner lock, which keeps a new session out of the folder while git
/// deletes it, or doesn't happen: when the lock can't be taken the ticket still closes, but the
/// worktree stays and the toast says why.
#[test]
fn closing_keeps_a_worktree_when_its_session_can_not_be_claimed() {
    let (repo, wt) = merged_worktree();
    let (mut app, _) = at_eng_52();
    let tv = &mut app.tickets[app.sel];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo.clone(), wt.clone(), true);
    break_lock(&repo, "ENG-52");
    prefix(&mut app, KeyCode::Char('x'));
    assert!(app.close_check.as_ref().is_some_and(|c| c.safe()));
    app.on_key(key(KeyCode::Char('r')));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    assert!(app.removing.is_empty(), "removal started without the lock");
    assert!(wt.exists());
    assert!(
        toast(&app).is_some_and(|t| t.starts_with("Kept the worktree: couldn't claim its session")),
        "{:?}",
        toast(&app)
    );
}

/// The same from `taskman close`: the reply names the folder and says why it stays.
#[test]
fn a_close_request_keeps_a_worktree_when_its_session_can_not_be_claimed() {
    let (repo, wt) = merged_worktree();
    let mut app = acme_app();
    app.projects[0].repo = repo.clone();
    let i = index(&app, "ENG-52");
    let tv = &mut app.tickets[i];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo.clone(), repo.clone(), false);
    request(
        "close-ENG-52",
        &serde_json::json!({
            "repo": repo, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-52" }], "close": {},
        }),
    );
    app.take_inbox();
    // The worktree becomes taskman's once the request is queued, so no PR lookup ran for it
    // (that would need gh); closing reads the ticket as it is then.
    let tv = &mut app.tickets[i];
    (tv.t.cwd, tv.t.created_worktree) = (wt.clone(), true);
    break_lock(&repo, "ENG-52");
    app.close_next();
    app.drain_messages();
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    assert!(app.removing.is_empty(), "removal started without the lock");
    assert!(wt.exists());
    let lines = reply("close-ENG-52");
    let expected = format!("Closed ENG-52. Kept {}: couldn't claim its session", store::tilde(&wt));
    assert!(lines[0].ok && lines[0].text.starts_with(&expected), "{}", lines[0].text);
}

#[test]
fn closing_an_agent_remembers_its_conversation() {
    let mut app = acme_app();
    let i = index(&app, "acme-lead");
    app.tickets[i].t.session_id = Some("sid-lead".into());
    app.select(i);
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Enter));
    let closed = &saved().projects[0].closed_agents;
    assert_eq!(closed.len(), 1);
    assert_eq!((closed[0].name.as_str(), closed[0].session_id.as_str()), ("lead", "sid-lead"));
    assert_eq!(closed[0].role.as_deref(), Some("Coordinate the tickets"));
}

#[test]
fn a_ticket_running_in_another_terminal_does_not_close() {
    let mut app = acme_app();
    // `taskman resume` holds the claim on ENG-52's session.
    let outside =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-52")).unwrap().unwrap();
    prefix(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(toast(&app).is_some_and(|t| t.contains("another terminal")), "{:?}", toast(&app));
    assert_eq!(app.tickets.len(), 7);
    drop(outside);
}

/// Claims the pane's session of agent `key`, as a session the pane started holds it.
fn claimed(app: &mut App, key: &str) {
    let hold = resume::take(&resume::owner_file("/src/acme".as_ref(), key)).unwrap().unwrap();
    let i = index(app, key);
    app.tickets[i].hold = Some(hold.taskman());
}

/// A close that can't be saved leaves the ticket open: until the state no longer lists it, `taskman
/// resume` finds it, so its session stays running and claimed.
#[test]
fn a_close_that_can_not_be_saved_leaves_the_ticket_open() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let i = index(&app, "acme-lead");
    app.tickets[i].t.session_id = Some("sid-lead".into());
    app.select(i);
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    claimed(&mut app, "acme-lead");
    let blocked = block_saves();
    prefix(&mut app, KeyCode::Char('x'));
    app.on_key(key(KeyCode::Enter));
    let error = toast(&app).unwrap_or_default();
    assert!(error.starts_with("lead stays open: couldn't save "), "{error}");
    assert_eq!((app.tickets.len(), selected(&app)), (7, "acme-lead"));
    let tv = &app.tickets[app.sel];
    assert!(tv.session.is_some() && !tv.reaping() && tv.hold.is_some(), "the session was stopped");
    assert_eq!(owner("acme-lead"), resume::Owner::Taskman(std::process::id()));
    assert!(app.projects[0].closed_agents.is_empty());
    std::fs::remove_dir_all(&blocked).unwrap();
}

/// The same from `taskman close`: the reply says it stays open, and why.
#[test]
fn a_close_request_that_can_not_be_saved_leaves_the_ticket_open() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    claimed(&mut app, "acme-lead");
    request(
        "close-lead",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "agent", "name": "lead" }], "close": {},
        }),
    );
    app.take_inbox();
    let blocked = block_saves();
    app.close_next();
    app.drain_messages();
    let lines = reply("close-lead");
    assert!(!lines[0].ok, "{}", lines[0].text);
    assert!(lines[0].text.starts_with("✦ lead stays open: couldn't save "), "{}", lines[0].text);
    let tv = &app.tickets[index(&app, "acme-lead")];
    assert!(tv.session.is_some() && tv.hold.is_some(), "the session was stopped");
    assert_eq!(owner("acme-lead"), resume::Owner::Taskman(std::process::id()));
    std::fs::remove_dir_all(&blocked).unwrap();
}

// ---- handing a session off, bringing it back, restarting it --------------------------------
//
// Nothing here may start a real CLI: a session that would start finds its claim held by the test
// (`held_outside`), as `taskman resume` in another terminal would hold it. tests/launch.rs runs the
// same with the binary and a stub CLI.

/// Holds ticket `key`'s claim as `taskman resume` does while it runs the session elsewhere.
fn held_outside(key: &str) -> resume::Hold {
    let mut hold = resume::take(&resume::owner_file("/src/acme".as_ref(), key)).unwrap().unwrap();
    hold.write("outside 4242");
    hold
}

fn owner(key: &str) -> resume::Owner {
    resume::peek(&resume::owner_file("/src/acme".as_ref(), key))
}

/// Answers the tmux window that's offered inside tmux, without opening one.
fn decline_tmux(app: &mut App) {
    if std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()) {
        assert!(matches!(app.mode, Mode::OfferTmux(_)));
        app.on_key(key(KeyCode::Esc));
    }
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn handing_off_stops_the_session_and_copies_what_continues_it() {
    let (mut app, to_session) = at_eng_52();
    let i = app.sel;
    prefix(&mut app, KeyCode::Char('t'));
    // The CLI is asked to quit and given time to; the pane waits for it.
    assert!(matches!(app.tickets[i].away, Some(Away::Leaving(_))));
    prefix(&mut app, KeyCode::Char('t'));
    assert!(matches!(app.tickets[i].away, Some(Away::Leaving(_))), "asked once");
    assert_eq!(sent(&to_session), "", "hung up, not typed at");

    // It didn't quit in time, so it's stopped.
    app.tickets[i].away = Some(Away::Leaving(ago(HAND_OFF_WAIT.as_secs_f32() + 1.0).unwrap()));
    app.poll_status();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none());
    assert_eq!((tv.away, tv.status.state), (Some(Away::HandedOff), AgentState::NotStarted));
    // The owner file keeps it out of the pane until the user brings it back, after a restart too.
    // The claim goes on the poll after the CLI is reaped, which the fake's reaper does at once,
    // on a thread.
    reaped(&mut app);
    app.poll_status();
    assert_eq!(owner("ENG-52"), resume::Owner::Back);
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["taskman resume ENG-52"]);
    assert!(
        toast(&app)
            .is_some_and(|t| t.contains("`taskman resume ENG-52`: run it in another terminal")),
        "{:?}",
        toast(&app)
    );
    decline_tmux(&mut app);
    app.ensure_selected_session();
    assert!(app.tickets[i].session.is_none(), "started again in the pane");
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("ENG-52 is handed off"), "{screen}");

    // ctrl+a t again offers the command again.
    prefix(&mut app, KeyCode::Char('t'));
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["taskman resume ENG-52"; 2]);
    decline_tmux(&mut app);
}

/// Handed off, the claim goes only once the pane's CLI is gone: `taskman resume` can't start a
/// second copy on the conversation the CLI is still ending, and meanwhile the owner file says
/// so (`Owner::Stopping`) rather than that the session runs elsewhere.
#[test]
fn a_hand_off_keeps_the_claim_until_the_old_cli_is_gone() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let i = app.sel;
    let (_sent, release) = attach_slow(&mut app, "ENG-52");
    prefix(&mut app, KeyCode::Char('t'));
    // It didn't quit in time, so it's stopped: hung up and reaped on a thread, which the CLI
    // holds up by going on for a while.
    app.tickets[i].away = Some(Away::Leaving(ago(HAND_OFF_WAIT.as_secs_f32() + 1.0).unwrap()));
    app.poll_status();
    let tv = &app.tickets[i];
    assert_eq!((tv.away, tv.status.state), (Some(Away::HandedOff), AgentState::NotStarted));
    assert!(tv.session.is_none() && tv.reaping() && tv.hold.is_some());
    assert_eq!(owner("ENG-52"), resume::Owner::Stopping);
    assert!(!resume::Owner::Stopping.elsewhere(), "closing the ticket is still allowed");
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["taskman resume ENG-52"]);
    decline_tmux(&mut app);
    // Polled again while the CLI is still there: still claimed, still handed off.
    app.poll_status();
    assert!(app.tickets[i].hold.is_some());
    assert_eq!(
        (app.tickets[i].away, owner("ENG-52")),
        (Some(Away::HandedOff), resume::Owner::Stopping)
    );
    // The CLI is gone: the next poll lets the claim go, and the ticket stays handed off.
    drop(release);
    reaped(&mut app);
    app.poll_status();
    assert!(app.tickets[i].hold.is_none(), "still claimed after the CLI was gone");
    assert_eq!(
        (app.tickets[i].away, owner("ENG-52")),
        (Some(Away::HandedOff), resume::Owner::Back)
    );
}

/// `prefix R` starts the new session only once the old CLI is gone: the lock is taskman's, not
/// the CLI's, so it wouldn't keep a second copy off the same conversation.
#[test]
fn a_restart_waits_until_the_old_cli_is_gone() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let i = app.sel;
    let (_sent, release) = attach_slow(&mut app, "ENG-52");
    let hold = resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-52")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(toast(&app), Some("Restarting Claude session…"));
    assert!(app.tickets[i].session.is_none() && app.tickets[i].reaping());
    // The tick that would start it again finds the CLI still stopping: nothing starts, nothing
    // is said, and the claim is kept for the start to come.
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.away.is_none(), "started over the old CLI");
    assert!(tv.hold.is_some() && tv.status.state == AgentState::NotStarted);
    assert_eq!(owner("ENG-52"), resume::Owner::Taskman(std::process::id()));
    // The CLI is gone: the next tick starts it. (The claim is handed to `taskman resume` here so
    // the test starts no real CLI: the start gets as far as taking the lock.)
    drop(release);
    reaped(&mut app);
    app.tickets[i].hold = None;
    let outside = held_outside("ENG-52");
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].away, Some(Away::Outside), "no start once the CLI was gone");
    drop(outside);
}

/// Closing with `r` removes the worktree only once the CLI that ran in it is gone.
#[test]
fn closing_removes_the_worktree_only_once_its_cli_is_gone() {
    let root = store::scratch::dir().join("git-stopping");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    sh(&root, "git init -q --bare -b main origin.git && git clone -q origin.git acme 2>/dev/null");
    let (repo, wt) = (root.join("acme"), root.join("acme-ENG-52"));
    sh(
        &repo,
        "git commit -q --allow-empty -m one && git push -q origin main && git remote set-head origin main \
         && git worktree add -q -b eng-52 ../acme-ENG-52 origin/main",
    );
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let (_sent, release) = attach_slow(&mut app, "ENG-52");
    let tv = &mut app.tickets[app.sel];
    (tv.t.repo, tv.t.cwd, tv.t.created_worktree) = (repo.clone(), wt.clone(), true);
    prefix(&mut app, KeyCode::Char('x'));
    assert!(app.close_check.as_ref().is_some_and(|c| c.safe()));
    app.on_key(key(KeyCode::Char('r')));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-52"));
    // The removal runs on a thread, waiting for the CLI: the worktree stays meanwhile.
    assert!(
        app.rx.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
        "removed while the CLI was still running in it"
    );
    assert!(wt.exists());
    drop(release);
    settle(&mut app);
    assert_eq!(
        toast(&app).map(String::from),
        Some(format!("Removed {} and branch eng-52", store::tilde(&wt)))
    );
    assert!(!wt.exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn only_a_started_session_can_be_handed_off() {
    let mut app = acme_app();
    // ENG-70's session has ended, and it never had a conversation to continue.
    app.select(index(&app, "ENG-70"));
    prefix(&mut app, KeyCode::Char('t'));
    assert_eq!(
        toast(&app),
        Some("ENG-70's session hasn't started: there's nothing to hand off yet")
    );
    assert_eq!(app.tickets[app.sel].away, None);
    // One whose worktree is still being made has nothing to hand off either.
    app.tickets[app.sel].creating = true;
    app.tickets[app.sel].t.session_id = Some("sid-70".into());
    prefix(&mut app, KeyCode::Char('t'));
    assert_eq!(app.tickets[app.sel].away, None);
    // With a conversation the CLI still has, it's handed off as it is.
    app.tickets[app.sel].creating = false;
    let transcript = store::state_dir().join(".claude/projects/-src-acme/sid-70.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(transcript, "{}\n").unwrap();
    prefix(&mut app, KeyCode::Char('t'));
    assert_eq!(app.tickets[app.sel].away, Some(Away::HandedOff));
    assert_eq!(owner("ENG-70"), resume::Owner::Back);
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["taskman resume ENG-70"]);
}

#[test]
fn a_session_running_elsewhere_is_never_started_twice() {
    let (mut app, _) = at_eng_52();
    let i = app.sel;
    // `taskman resume` runs it in another terminal.
    let outside = held_outside("ENG-52");
    app.poll_status();
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    assert!(app.tickets[i].session.is_none(), "the pane showed the old screen as live");
    app.on_key(key(KeyCode::Enter));
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(
        toast(&app),
        Some("It's running in another terminal: quit it there, then press Enter here")
    );
    prefix(&mut app, KeyCode::Char('t'));
    assert_eq!(toast(&app), Some("ENG-52 is already running in another terminal"));
    assert_eq!(app.tickets[i].away, Some(Away::Outside));

    // It ended there: Enter brings it back.
    drop(outside);
    app.poll_status();
    assert_eq!(app.tickets[i].away, Some(Away::Back));
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("ENG-52 is back"), "{screen}");
    // But it was resumed there again first: the pane starts no second copy.
    let again = held_outside("ENG-52");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    assert!(app.tickets[i].session.is_none());
    // ctrl+a R does the same as Enter on a session that's back.
    drop(again);
    app.poll_status();
    assert_eq!(app.tickets[i].away, Some(Away::Back));
    let again = held_outside("ENG-52");
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    drop(again);
}

#[test]
fn a_session_whose_worktree_is_being_removed_waits() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "ENG-70"));
    app.tickets[app.sel].status.state = AgentState::NotStarted;
    let closing = resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-70"))
        .unwrap()
        .unwrap()
        .closing();
    app.ensure_selected_session();
    assert_eq!(toast(&app).map(String::from), Some(resume::closing("ENG-70")));
    assert_eq!(app.tickets[app.sel].status.state, AgentState::Exited, "Enter tries again");
    assert!(app.tickets[app.sel].session.is_none());
    drop(closing);
}

/// The owner lock fails closed: a lock that can't be taken stops the session starting, since
/// without it a second copy could run on the same conversation.
#[test]
fn a_session_does_not_start_without_its_lock() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    let i = index(&app, "ENG-70");
    app.select(i);
    app.tickets[i].status.state = AgentState::NotStarted;
    // A plain file sits where the project's status folder goes, so the lock file can't be made.
    let owner = resume::owner_file("/src/acme".as_ref(), "ENG-70");
    let folder = owner.parent().unwrap();
    std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
    std::fs::write(folder, "").unwrap();
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.hold.is_none(), "started without the lock");
    assert_eq!(tv.status.state, AgentState::Exited);
    assert!(
        toast(&app).is_some_and(|t| t.starts_with("Couldn't take ENG-70's session lock: ")
            && t.ends_with("; fix it, then press Enter to retry")),
        "{:?}",
        toast(&app)
    );
    let screen = draw(&mut app, 80, 24);
    assert!(
        screen.contains("Couldn't take the lock that keeps ENG-70's session to one copy")
            && screen.contains("Fix it, then press Enter to retry."),
        "{screen}"
    );
    // The next tick leaves it alone; Enter tries again, and the lock is taken this time: here
    // found held by `taskman resume`, so no CLI starts in the test.
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].status.state, AgentState::Exited);
    std::fs::remove_file(folder).unwrap();
    let outside = held_outside("ENG-70");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.tickets[i].status.state, AgentState::NotStarted);
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    assert_eq!(app.tickets[i].lock_error, None);
    drop(outside);
}

/// A status file record as `taskman hook` writes it, from start `launch` of the session.
fn record(state: &str, sid: &str, launch: Option<&str>) -> String {
    let mut v = crate::hook::status_record(state, Some(sid), None, "", 1);
    if let Some(l) = launch {
        v["launch"] = l.into();
    }
    v.to_string()
}

fn write_status(key: &str, text: &str) -> std::path::PathBuf {
    let path = store::status_file("/src/acme".as_ref(), key);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

/// The hooks say which start of the session wrote each record, and the pane takes only its own
/// start's: a SessionEnd hook runs on the hangup that stops a CLI, so its record can land after
/// the restart's new session is running, or after the ticket closed and was added again.
#[test]
fn a_status_record_counts_only_from_the_sessions_own_start() {
    let (mut app, _) = at_eng_52();
    let i = app.sel;
    app.tickets[i].launch = Some("1-7".into());
    app.tickets[i].status.state = AgentState::Idle;
    let reread = |app: &mut App| {
        app.tickets[i].status.mtime = None;
        app.poll_status();
    };
    // The start before this one ends late: not this session's, and not its conversation.
    write_status("ENG-52", &record("ended", "sid-old", Some("1-6")));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::Idle);
    assert_eq!(app.tickets[i].t.session_id, None);
    // Its own start's records count, as do an older hook's, which name no start.
    write_status("ENG-52", &record("done", "sid-52", Some("1-7")));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::Done);
    assert_eq!(app.tickets[i].t.session_id.as_deref(), Some("sid-52"));
    write_status("ENG-52", &record("working", "sid-52", None));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::Working);
    // Restarted (`prefix R`), nothing runs in the pane yet: the old start's end doesn't make the
    // session that's about to start look ended.
    prefix(&mut app, KeyCode::Char('R'));
    write_status("ENG-52", &record("ended", "sid-52", Some("1-7")));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::NotStarted);
    // Closed and added again, the ticket has no session: the old one's end isn't its own, and
    // the old conversation isn't its to resume.
    app.tickets[i].hold = None;
    app.tickets[i].t.session_id = None;
    write_status("ENG-52", &record("ended", "sid-old", Some("1-6")));
    reread(&mut app);
    assert_eq!(app.tickets[i].status.state, AgentState::NotStarted);
    assert_eq!(app.tickets[i].t.session_id, None);
    // Running in another terminal, its hooks' records are the session's, whatever start they
    // name.
    let outside = held_outside("ENG-52");
    write_status("ENG-52", &record("needs", "sid-out", Some("4242-1")));
    reread(&mut app);
    assert_eq!(app.tickets[i].away, Some(Away::Outside));
    assert_eq!(app.tickets[i].status.state, AgentState::Needs);
    assert_eq!(app.tickets[i].t.session_id.as_deref(), Some("sid-out"));
    drop(outside);
}

/// Adding a ticket or agent clears what a hook of its last session wrote after it closed, so
/// at taskman's next start it can't pass for the new one's latest conversation.
#[test]
fn adding_a_ticket_or_agent_clears_what_its_last_session_left() {
    let mut app = acme_app();
    let path = write_status("ENG-80", &record("ended", "sid-80", Some("1-2")));
    std::fs::write(path.with_extension("gh"), "1").unwrap();
    app.add_ticket(0, "ENG-80".into(), store::Cli::Claude, false, None, false).unwrap();
    assert!(!path.exists() && !path.with_extension("gh").exists());
    let path = write_status("acme-tester", &record("ended", "sid-tester", Some("1-3")));
    app.add_agent(0, "tester", store::Cli::Claude, None, false).unwrap();
    assert!(!path.exists());
}

#[test]
fn restarting_stops_the_session_and_keeps_its_claim() {
    let (mut app, _) = at_eng_52();
    let i = app.sel;
    let hold = resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-52")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(toast(&app), Some("Restarting Claude session…"));
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.status.state == AgentState::NotStarted);
    // The next start takes over the pane's claim rather than finding it held, by taskman itself.
    assert!(tv.hold.is_some());
    assert_eq!(owner("ENG-52"), resume::Owner::Taskman(std::process::id()));

    // Enter on an ended session restarts it too.
    let j = index(&app, "ENG-61");
    app.select(j);
    app.tickets[j].status.state = AgentState::Exited;
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.tickets[j].status.state, AgentState::NotStarted);
    prefix(&mut app, KeyCode::Char('R'));
    assert_eq!(toast(&app), Some("Restarting Codex session…"));
}

#[test]
fn a_session_keeps_the_name_it_started_with() {
    let (mut app, _) = at_eng_52();
    let i = app.sel;
    app.tickets[i].name = Some("acme/ENG-52".into());
    assert_eq!(app.session_name(i), "acme/ENG-52", "other sessions message it by that name");
    app.tickets[i].session = None;
    assert_eq!(app.session_name(i), "ENG-52", "the name it would start with now");
}

#[test]
fn a_running_agent_starts_its_projects_claude_sessions() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    attach(&mut app, "acme-lead", SCREEN);
    app.select(index(&app, "acme-lead"));
    // ENG-61 hasn't started, but runs in Codex, which can't be messaged.
    let (eng53, eng61) = (index(&app, "ENG-53"), index(&app, "ENG-61"));
    app.tickets[eng61].status.state = AgentState::NotStarted;
    let _held61 = held_outside("ENG-61");
    app.ensure_selected_session();
    assert_eq!(app.tickets[eng61].away, None, "tried to start a Codex session for the agent");
    // ENG-53 hasn't started either: it's tried, and found running elsewhere.
    app.tickets[eng53].status.state = AgentState::NotStarted;
    let _held53 = held_outside("ENG-53");
    app.ensure_selected_session();
    assert_eq!(app.tickets[eng53].away, Some(Away::Outside));
}

// ---- agents asked for by `taskman add` -----------------------------------------------------

fn agent_request(role: &str) -> add::Request {
    serde_json::from_value(serde_json::json!({
        "repo": "/src/acme", "from": "✦ lead", "reply_by": 0,
        "items": [{ "kind": "agent", "name": "tester", "role": role }],
    }))
    .unwrap()
}

fn ask(app: &mut App, req: add::Request) {
    app.asking.push_back(req);
    app.ask_next();
    let Mode::ConfirmAgent(ask) = &mut app.mode else { panic!("no dialog") };
    // As if it had been open a while.
    ask.shown = ago(super::ASK_ARMS.as_secs_f32()).unwrap();
}

#[test]
fn an_agent_waits_for_the_users_approval() {
    let mut app = acme_app();
    app.asking.push_back(agent_request("Run the tests"));
    // Not while a prefix command is under way.
    app.on_key(ctrl('a'));
    app.ask_next();
    assert!(matches!(app.mode, Mode::Normal));
    app.on_key(key(KeyCode::Esc));
    app.ask_next();
    assert!(matches!(app.mode, Mode::ConfirmAgent(_)));
    // An Enter typed for the session just as the dialog opens doesn't answer it.
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::ConfirmAgent(_)));
    let Mode::ConfirmAgent(ask) = &mut app.mode else { unreachable!() };
    ask.shown = ago(super::ASK_ARMS.as_secs_f32()).unwrap();
    app.on_key(key(KeyCode::Char('y')));
    assert!(matches!(app.mode, Mode::ConfirmAgent(_)), "only Enter and Esc answer it");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    let tv = &app.tickets[index(&app, "acme-tester")];
    assert_eq!(tv.t.role.as_deref(), Some("Run the tests"));
    assert!(tv.start, "its session starts without being selected");
    assert!(saved().tickets.iter().any(|t| t.key == "acme-tester"));
}

#[test]
fn a_declined_agent_is_not_added() {
    let mut app = acme_app();
    ask(&mut app, agent_request("Run the tests"));
    app.on_key(key(KeyCode::Esc));
    assert_eq!(toast(&app), Some("Declined ✦ tester from ✦ lead"));
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "acme-tester"));
}

#[test]
fn a_new_role_for_an_open_agent_waits_for_approval_too() {
    let mut app = acme_app();
    ask(&mut app, agent_request("Run the tests"));
    app.on_key(key(KeyCode::Enter));
    ask(&mut app, agent_request("Review the PRs"));
    let Mode::ConfirmAgent(asked) = &app.mode else { panic!("no dialog") };
    assert!(asked.open);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("✦ tester's new role applies when its session next starts"));
    assert_eq!(app.tickets[index(&app, "acme-tester")].t.role.as_deref(), Some("Review the PRs"));
}

// ---- saving --------------------------------------------------------------------------------

/// Puts a folder where tickets.json goes, so saving fails until it's removed; returns it.
fn block_saves() -> std::path::PathBuf {
    let file = store::state_dir().join("tickets.json");
    let _ = std::fs::remove_file(&file);
    std::fs::create_dir_all(file.join("in-the-way")).unwrap();
    file
}

#[test]
fn a_failed_save_keeps_showing_until_one_works() {
    let mut app = offline(crate::app::test_support::acme());
    let file = block_saves();
    prefix(&mut app, KeyCode::Char('c'));
    let error = toast(&app).unwrap().to_string();
    assert!(error.starts_with("Couldn't save ") && error.contains("tickets.json"), "{error}");
    assert!(error.ends_with("changes are kept only until taskman quits"), "{error}");

    // Another toast hides it for a while; once that one goes, it's back.
    prefix(&mut app, KeyCode::Char('y'));
    assert_ne!(toast(&app), Some(error.as_str()));
    app.toast = None;
    app.show_save_error();
    assert_eq!(toast(&app), Some(error.as_str()));

    std::fs::remove_dir_all(&file).unwrap();
    prefix(&mut app, KeyCode::Char('c'));
    assert!(app.save_errors.iter().all(Option::is_none));
    app.toast = None;
    app.show_save_error();
    assert_eq!(toast(&app), None);
    assert!(saved().ui.show_comments, "the change made while saving failed is saved too");
}

// ---- messages from threads -----------------------------------------------------------------

/// Ticket `key` as a `Msg` names it.
fn id(app: &App, key: &str) -> super::Id {
    (app.tickets[index(app, key)].t.repo.clone(), key.to_string())
}

/// Starts a tracker fetch for ticket `key` as `schedule_fetches` does, without its thread, and
/// returns the fetch's id, which its reply carries.
fn fetching(app: &mut App, key: &str) -> u64 {
    app.fetches += 1;
    let (n, i) = (app.fetches, index(app, key));
    app.tickets[i].jira.start(n);
    n
}

/// `fetching` for a PR lookup.
fn looking_up(app: &mut App, key: &str) -> u64 {
    app.fetches += 1;
    let (n, i) = (app.fetches, index(app, key));
    app.tickets[i].pr.start(n);
    n
}

#[test]
fn fetched_comments_count_as_new_until_seen() {
    let mut app = acme_app();
    let new_comments = |app: &App| {
        let tv = &app.tickets[index(app, "ENG-61")];
        tv.jira.data.as_ref().unwrap().comments.iter().filter(|c| tv.is_new_jira(&c.id)).count()
    };
    let mut issue = crate::app::test_support::issue("ENG-61");
    issue.comments.truncate(1);
    // The first fetch counts what's there as seen.
    let i = index(&app, "ENG-61");
    app.tickets[i].t.seen_jira = None;
    let n = fetching(&mut app, "ENG-61");
    app.tx.send(Msg::Jira(n, id(&app, "ENG-61"), Ok(issue.clone()))).unwrap();
    app.drain_messages();
    assert_eq!(new_comments(&app), 0);

    let mut newer = issue;
    newer
        .comments
        .insert(0, crate::tracker::Comment { id: "c9".into(), ..newer.comments[0].clone() });
    let n = fetching(&mut app, "ENG-61");
    app.tx.send(Msg::Jira(n, id(&app, "ENG-61"), Ok(newer))).unwrap();
    app.drain_messages();
    assert_eq!(new_comments(&app), 1);

    // A failed refresh keeps what was fetched.
    let n = fetching(&mut app, "ENG-61");
    app.tx.send(Msg::Jira(n, id(&app, "ENG-61"), Err("timed out".into()))).unwrap();
    app.drain_messages();
    let tv = &app.tickets[index(&app, "ENG-61")];
    assert_eq!(tv.jira.error.as_deref(), Some("timed out"));
    assert_eq!(tv.jira.data.as_ref().unwrap().comments.len(), 2);

    // Selecting the ticket, then leaving it, marks them seen.
    app.select(index(&app, "ENG-61"));
    app.select(index(&app, "ENG-52"));
    assert_eq!(app.tickets[index(&app, "ENG-61")].new_count(), 0);
    assert!(
        saved().tickets[index(&app, "ENG-61")]
            .seen_jira
            .as_ref()
            .unwrap()
            .contains(&"c9".to_string())
    );
}

#[test]
fn a_reply_from_before_the_tracker_changed_is_dropped() {
    let mut app = acme_app();
    let i = index(&app, "ENG-52");
    let old = fetching(&mut app, "ENG-52");
    // The project moves to another workspace while the fetch runs: the panel empties and asks
    // the new one.
    let mut project = app.projects[0].clone();
    project.tracker.as_mut().unwrap().site = "acme-labs".into();
    app.update_project(0, project);
    let (tv, hour) = (&app.tickets[i], std::time::Duration::from_secs(3600));
    assert!(tv.jira.data.is_none() && !tv.jira.loading && tv.jira.due(hour));
    // The old workspace's reply lands after that: it isn't this tracker's issue.
    let issue = crate::app::test_support::issue("ENG-52");
    app.tx.send(Msg::Jira(old, id(&app, "ENG-52"), Ok(issue.clone()))).unwrap();
    app.drain_messages();
    let tv = &app.tickets[i];
    assert!(tv.jira.data.is_none() && tv.jira.fetched_at.is_none(), "a stale reply applied");
    // The new workspace's reply is the one shown.
    let new = fetching(&mut app, "ENG-52");
    app.tx.send(Msg::Jira(new, id(&app, "ENG-52"), Ok(issue))).unwrap();
    app.drain_messages();
    let tv = &app.tickets[i];
    assert!(tv.jira.data.is_some() && tv.jira.fetched_at.is_some() && !tv.jira.loading);
}

#[test]
fn a_pr_found_for_a_closed_ticket_is_not_the_reopened_tickets() {
    let mut app = acme_app();
    let old = looking_up(&mut app, "ENG-70");
    // Closed while gh runs, then added again without a worktree: another ticket with the key.
    app.close_at(index(&app, "ENG-70")).unwrap().hold.unwrap();
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-70"));
    request(
        "readd-ENG-70",
        &serde_json::json!({
            "repo": "/src/acme", "worktree": false, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-70" }],
        }),
    );
    app.take_inbox();
    let i = index(&app, "ENG-70");
    assert!(app.tickets[i].t.branch.is_none() && app.tickets[i].pr.data.is_none());
    // The old ticket's lookup lands on the new one: neither its PR nor its branch is taken.
    let found = || {
        Ok(crate::github::Lookup {
            pr: Some(crate::app::test_support::pr(146, "eng-70-export")),
            closed: None,
            branch_gone: false,
        })
    };
    app.tx.send(Msg::Pr(old, id(&app, "ENG-70"), found(), None)).unwrap();
    app.drain_messages();
    let t = &saved().tickets[i];
    assert!(app.tickets[i].pr.data.is_none() && t.branch.is_none(), "a stale lookup applied");
    // The new ticket's own lookup is.
    let new = looking_up(&mut app, "ENG-70");
    app.tx.send(Msg::Pr(new, id(&app, "ENG-70"), found(), None)).unwrap();
    app.drain_messages();
    let pr = app.tickets[i].pr.data.as_ref().and_then(Option::as_ref).map(|p| p.number);
    assert_eq!((pr, saved().tickets[i].branch.as_deref()), (Some(146), Some("eng-70-export")));
}

/// Adds ticket `key` to acme through the inbox, without a worktree, then marks git as creating
/// one for it, as `create_worktree` does without starting git. Returns the creation's id.
fn creating(app: &mut App, key: &str) -> u64 {
    request(
        &format!("add-{key}"),
        &serde_json::json!({
            "repo": "/src/acme", "worktree": false, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": key }],
        }),
    );
    app.take_inbox();
    app.creations += 1;
    let i = index(app, key);
    let tv = &mut app.tickets[i];
    tv.creating = true;
    tv.creation_id = app.creations;
    tv.t.pending_worktree = Some(Default::default());
    app.creations
}

/// What git sends when it has created `key`'s worktree, `made` now or found there already.
fn created(app: &App, n: u64, key: &str, made: bool) -> Msg {
    let c = crate::worktree::Created {
        path: format!("/src/acme-{key}").into(),
        branch: format!("{}-export", key.to_lowercase()),
        note: None,
        stacked: false,
        made,
    };
    Msg::Worktree(n, (app.projects[0].repo.clone(), key.into()), Ok(c), None)
}

#[test]
fn a_created_worktree_becomes_the_tickets_and_taskmans_only_if_git_made_it() {
    let mut app = acme_app();
    let n = creating(&mut app, "ENG-90");
    app.tx.send(created(&app, n, "ENG-90", true)).unwrap();
    app.drain_messages();
    let t = &saved().tickets[index(&app, "ENG-90")];
    assert_eq!(t.cwd, std::path::Path::new("/src/acme-ENG-90"));
    assert_eq!(t.branch.as_deref(), Some("eng-90-export"));
    assert!(t.created_worktree && t.pending_worktree.is_none());
    assert!(!app.tickets[index(&app, "ENG-90")].creating);

    // Already the branch's worktree when git looked: someone else's, which a close must leave.
    let n = creating(&mut app, "ENG-91");
    app.tx.send(created(&app, n, "ENG-91", false)).unwrap();
    app.drain_messages();
    let t = &saved().tickets[index(&app, "ENG-91")];
    assert_eq!(t.cwd, std::path::Path::new("/src/acme-ENG-91"));
    assert!(!t.created_worktree && t.pending_worktree.is_none());
}

#[test]
fn a_worktree_finished_after_its_ticket_closed_is_not_the_reopened_tickets() {
    let mut app = acme_app();
    let n = creating(&mut app, "ENG-90");
    // Closed, keeping the worktree, while git still runs; then added again without one.
    request(
        "close-ENG-90",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-90" }], "close": { "keep_worktree": true },
        }),
    );
    app.take_inbox();
    app.close_next();
    app.drain_messages();
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-90"));
    request(
        "readd-ENG-90",
        &serde_json::json!({
            "repo": "/src/acme", "worktree": false, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-90" }],
        }),
    );
    app.take_inbox();

    app.tx.send(created(&app, n, "ENG-90", true)).unwrap();
    app.drain_messages();
    // The new ticket stays in the main checkout, with nothing to remove when it closes.
    let t = &saved().tickets[index(&app, "ENG-90")];
    assert_eq!(t.cwd, std::path::Path::new("/src/acme"));
    assert_eq!(t.branch, None);
    assert!(!t.created_worktree);
    // The old creation goes as its close asked: kept.
    assert!(app.closed_creating.is_empty());
    assert!(
        toast(&app)
            .is_some_and(|t| t
                .ends_with("closed while its worktree was being created. Kept /src/acme-ENG-90")),
        "{:?}",
        toast(&app)
    );
}

/// The worktree git finished after its ticket closed goes only under the owner lock too.
#[test]
fn a_worktree_finished_after_its_ticket_closed_stays_when_its_session_can_not_be_claimed() {
    let mut app = acme_app();
    let n = creating(&mut app, "ENG-90");
    request(
        "close-ENG-90",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-90" }], "close": {},
        }),
    );
    app.take_inbox();
    app.close_next();
    app.drain_messages();
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "ENG-90"));
    // The state folder stops taking locks before git finishes.
    break_lock("/src/acme".as_ref(), "ENG-90");
    app.tx.send(created(&app, n, "ENG-90", true)).unwrap();
    app.drain_messages();
    assert!(app.removing.is_empty(), "removal started without the lock");
    let expected = "ENG-90 closed while its worktree was being created. \
                    Kept /src/acme-ENG-90: couldn't claim its session";
    assert!(toast(&app).is_some_and(|t| t.starts_with(expected)), "{:?}", toast(&app));
}

/// A workflow's worktree on `change/ENG-70` is what the lookup prefers for the key, but the one
/// taskman created stays the ticket's: a refresh never moves the ticket there, so closing can't
/// offer the workflow's folder and branch for removal.
#[test]
fn a_pr_lookup_never_moves_a_ticket_out_of_the_worktree_taskman_made() {
    let mut app = acme_app();
    let i = index(&app, "ENG-70");
    let tv = &mut app.tickets[i];
    (tv.session, tv.t.session_id) = (None, None);
    assert!(super::ticket::own_worktree(tv));
    let lookup = || Ok(crate::github::Lookup { pr: None, closed: None, branch_gone: false });
    let found = || Some((std::path::PathBuf::from("/src/wf/ENG-70"), "change/ENG-70".to_string()));
    let n = looking_up(&mut app, "ENG-70");
    app.tx.send(Msg::Pr(n, id(&app, "ENG-70"), lookup(), found())).unwrap();
    app.drain_messages();
    let tv = &app.tickets[i];
    assert_eq!(
        (tv.t.cwd.as_path(), tv.t.branch.as_deref(), tv.t.created_worktree),
        (std::path::Path::new("/src/acme-ENG-70"), Some("eng-70-work"), true)
    );
    // Without one of its own, the ticket moves there, and the folder isn't taskman's to remove.
    let tv = &mut app.tickets[i];
    (tv.t.cwd, tv.t.created_worktree) = ("/src/acme".into(), false);
    let n = looking_up(&mut app, "ENG-70");
    app.tx.send(Msg::Pr(n, id(&app, "ENG-70"), lookup(), found())).unwrap();
    app.drain_messages();
    let t = saved().tickets.into_iter().find(|t| t.key == "ENG-70").unwrap();
    assert_eq!(
        (t.cwd.as_path(), t.branch.as_deref(), t.created_worktree),
        (std::path::Path::new("/src/wf/ENG-70"), Some("change/ENG-70"), false)
    );
    assert!(!super::ticket::own_worktree(&app.tickets[i]));
}

#[test]
fn the_links_list_opens_with_the_tickets_links() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('l'));
    let Mode::Links(list) = &app.mode else { panic!("no links list: {:?}", toast(&app)) };
    assert_eq!(list.links[0].url, "https://linear.app/acme/issue/ENG-52");
    assert!(list.links.iter().any(|l| l.url == "https://github.com/acme/acme/pull/128"));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Char('c')));
    assert!(matches!(app.mode, Mode::Normal));
    settle(&mut app);
    assert_eq!(app.clipboard_out.len(), 1);
}

// ---- starting sessions ---------------------------------------------------------------------

/// The project agent runs, ENG-52 hasn't started and is away as `how`, and ENG-53 hasn't started:
/// the agent's tickets start one per tick, so ENG-53 should get its turn.
fn agent_starts_past(how: Away) {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    attach(&mut app, "acme-lead", SCREEN);
    app.select(index(&app, "acme-lead"));
    let eng52 = index(&app, "ENG-52");
    app.tickets[eng52].status.state = AgentState::NotStarted;
    app.tickets[eng52].away = Some(how);
    // ENG-53's claim is held as `taskman resume` holds it, so trying to start it marks it Outside
    // and no CLI runs.
    let eng53 = index(&app, "ENG-53");
    app.tickets[eng53].status.state = AgentState::NotStarted;
    let mut held =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "ENG-53")).unwrap().unwrap();
    held.write("outside 4242");
    for _ in 0..5 {
        app.ensure_selected_session();
    }
    assert_eq!(app.tickets[eng53].away, Some(Away::Outside), "ENG-53 was never tried");
    assert!(app.tickets[eng52].session.is_none(), "ENG-52 started while away");
    drop(held);
}

#[test]
fn a_handed_off_ticket_does_not_stop_an_agent_starting_the_others() {
    // Handed off, it waits for the user to bring it back.
    agent_starts_past(Away::HandedOff);
}

#[test]
fn a_ticket_running_outside_does_not_stop_an_agent_starting_the_others() {
    // Running in another terminal before its first hook said what it's doing.
    agent_starts_past(Away::Outside);
}

// ---- forms and Settings --------------------------------------------------------------------

/// The open form's field `label`.
fn field(app: &App, label: &str) -> String {
    match &app.mode {
        Mode::Form(f) => f.value(label),
        Mode::Settings(s) => s.form.value(label),
        _ => panic!("no form open"),
    }
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
}

#[test]
fn the_add_form_adds_an_agent() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(field(&app, "Project"), "acme");
    focus(&mut app, "Type");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Type"), "Agent");
    // Its role, then its name, last as the ticket is.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Role");
    type_text(&mut app, "Keep the guide current");
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Name");
    type_text(&mut app, "Docs Writer");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert_eq!(selected(&app), "acme-docs-writer");
    assert_eq!(app.tickets[app.sel].t.role.as_deref(), Some("Keep the guide current"));
    // The project's agents come before its tickets.
    let order: Vec<&str> = app.members(0).iter().map(|&i| app.tickets[i].t.key.as_str()).collect();
    assert_eq!(order[..3], ["acme-lead", "acme-release-notes-reviewer", "acme-docs-writer"]);
}

#[test]
fn the_add_form_switches_to_a_ticket_already_open() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "x");
    app.on_key(ctrl('u'));
    type_text(&mut app, "61");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("ENG-61 is already open; switched to it"));
    assert_eq!(selected(&app), "ENG-61");
    // Esc leaves the form as it was.
    prefix(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "62");
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.tickets.len(), 7);
}

#[test]
fn renames_an_agent() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('e'));
    assert_eq!(toast(&app), Some("Only agents can be renamed; a ticket is named by its key"));
    app.select(index(&app, "acme-lead"));
    prefix(&mut app, KeyCode::Char('e'));
    app.on_key(ctrl('u'));
    type_text(&mut app, "planner");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert_eq!(selected(&app), "acme-planner");
    assert!(saved().tickets.iter().any(|t| t.agent.as_deref() == Some("planner")));
}

/// Opens the Rename form on the selected agent and submits `name`.
fn rename(app: &mut App, name: &str) {
    prefix(app, KeyCode::Char('e'));
    app.on_key(ctrl('u'));
    type_text(app, name);
    app.on_key(key(KeyCode::Enter));
}

/// The Rename form's error, when it stayed open with one.
fn rename_error(app: &App) -> Option<&str> {
    match &app.mode {
        Mode::Form(form) => form.error.as_deref(),
        _ => None,
    }
}

/// Renamed, the agent's new name is claimed before the rename is saved and kept for the restart,
/// so `taskman resume` under it can't start a second copy while the old CLI is still stopping; the
/// old name's claim goes with the save, since nothing can resume the old name any more.
#[test]
fn a_rename_claims_the_new_name_before_saving_and_lets_the_old_one_go() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "acme-lead"));
    let i = app.sel;
    let (_sent, release) = attach_slow(&mut app, "acme-lead");
    let hold =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-lead")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    rename(&mut app, "planner");
    assert_eq!(toast(&app), Some("Renamed ✦ lead to ✦ planner"));
    assert!(matches!(app.mode, Mode::Normal));
    let tv = &app.tickets[i];
    assert_eq!((tv.t.key.as_str(), tv.t.agent.as_deref()), ("acme-planner", Some("planner")));
    assert_eq!(tv.status.state, AgentState::NotStarted);
    // The old CLI is still stopping; the new name is claimed, and kept for the start to come.
    assert!(tv.session.is_none() && tv.reaping() && tv.hold.is_some());
    assert_eq!(owner("acme-planner"), resume::Owner::Taskman(std::process::id()));
    assert!(
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-planner")).unwrap().is_err(),
        "taskman resume could take the new name"
    );
    // The old name is saved away, and its claim and owner file are gone.
    let keys: Vec<String> = saved().tickets.iter().map(|t| t.key.clone()).collect();
    assert!(keys.contains(&"acme-planner".to_string()) && !keys.contains(&"acme-lead".to_string()));
    assert_eq!(owner("acme-lead"), resume::Owner::Free);
    assert!(!resume::owner_file("/src/acme".as_ref(), "acme-lead").exists());
    // The tick that would start it finds the CLI still stopping: nothing starts, the claim stays.
    app.ensure_selected_session();
    let tv = &app.tickets[i];
    assert!(tv.session.is_none() && tv.away.is_none() && tv.hold.is_some(), "started too soon");
    // The CLI is gone: the next tick starts it under that claim. (The claim is handed to `taskman
    // resume` here so the test starts no real CLI: the start gets as far as taking the lock.)
    drop(release);
    reaped(&mut app);
    app.tickets[i].hold = None;
    let outside = held_outside("acme-planner");
    app.ensure_selected_session();
    assert_eq!(app.tickets[i].away, Some(Away::Outside), "no start once the CLI was gone");
    drop(outside);
}

/// A name whose session another terminal runs can't be taken, nor given up: the rename is refused
/// with nothing changed, either way.
#[test]
fn a_rename_is_refused_while_either_name_runs_elsewhere() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "acme-lead"));
    let i = app.sel;
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    let hold =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-lead")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    // A session under the new name runs in another terminal.
    let outside = held_outside("acme-planner");
    rename(&mut app, "planner");
    assert_eq!(
        rename_error(&app),
        Some("planner is running in another terminal; quit it there first")
    );
    let tv = &app.tickets[i];
    assert_eq!(tv.t.key, "acme-lead");
    assert!(tv.session.is_some() && !tv.reaping() && tv.hold.is_some(), "the session was stopped");
    assert_eq!(owner("acme-lead"), resume::Owner::Taskman(std::process::id()));
    assert!(saved().tickets.iter().any(|t| t.key == "acme-lead"));
    drop(outside);
    app.on_key(key(KeyCode::Esc));
    // The agent's own session was taken by another terminal since the last poll.
    app.select(index(&app, "acme-release-notes-reviewer"));
    let j = app.sel;
    assert!(app.tickets[j].hold.is_none());
    let outside = held_outside("acme-release-notes-reviewer");
    rename(&mut app, "planner");
    assert_eq!(rename_error(&app), Some("It's running in another terminal: quit it there first"));
    assert_eq!(app.tickets[j].t.key, "acme-release-notes-reviewer");
    assert!(
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-planner")).unwrap().is_ok(),
        "the new name was claimed"
    );
    drop(outside);
}

/// The lock is what keeps a session to one copy, so a name whose lock can't be taken (the state
/// folder can't be written, say) isn't taken either: refused, saying why, with nothing changed.
#[test]
fn a_rename_is_refused_when_the_new_name_can_not_be_claimed() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "acme-lead"));
    let i = app.sel;
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    let hold =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-lead")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    break_lock("/src/acme".as_ref(), "acme-planner");
    rename(&mut app, "planner");
    let error = rename_error(&app).unwrap_or_default();
    assert!(error.starts_with("Couldn't take planner's session lock: "), "{error}");
    assert!(error.ends_with(" can be written"), "{error}");
    let tv = &app.tickets[i];
    assert_eq!(tv.t.key, "acme-lead");
    assert!(tv.session.is_some() && !tv.reaping() && tv.hold.is_some(), "the session was stopped");
    assert!(saved().tickets.iter().any(|t| t.key == "acme-lead"));
}

/// A rename that can't be saved changes nothing: until it's saved, `taskman resume` finds the agent
/// under its old name, so that name stays claimed and its session runs on. Submitted again once the
/// state can be saved, it renames.
#[test]
fn a_rename_that_can_not_be_saved_changes_nothing() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    app.select(index(&app, "acme-lead"));
    let i = app.sel;
    let _sent = attach(&mut app, "acme-lead", SCREEN);
    let hold =
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-lead")).unwrap().unwrap();
    app.tickets[i].hold = Some(hold.taskman());
    // An agent closed under the new name, which the rename would drop.
    let closed = store::ClosedAgent {
        name: "planner".into(),
        session_id: "sid-planner".into(),
        cli: store::Cli::Claude,
        role: None,
    };
    app.projects[0].closed_agents.push(closed);
    let blocked = block_saves();
    rename(&mut app, "planner");
    let error = rename_error(&app).unwrap_or_default();
    assert!(error.starts_with("✦ lead keeps its name: couldn't save "), "{error}");
    let tv = &app.tickets[i];
    assert_eq!((tv.t.key.as_str(), tv.t.agent.as_deref()), ("acme-lead", Some("lead")));
    assert!(tv.session.is_some() && !tv.reaping() && tv.hold.is_some(), "the session was stopped");
    assert_eq!(owner("acme-lead"), resume::Owner::Taskman(std::process::id()));
    assert!(
        resume::take(&resume::owner_file("/src/acme".as_ref(), "acme-planner")).unwrap().is_ok(),
        "the new name stayed claimed"
    );
    assert_eq!(app.projects[0].closed_agents.len(), 1);

    std::fs::remove_dir_all(&blocked).unwrap();
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Renamed ✦ lead to ✦ planner"));
    assert!(saved().tickets.iter().any(|t| t.key == "acme-planner"));
    assert_eq!(owner("acme-lead"), resume::Owner::Free);
}

#[test]
/// Submitting the General form sets the prefix for the whole process (`keys::set_prefix`), and
/// every test running at once draws and reads keys with it. This one leaves it at ctrl+a; a test
/// that changed it would flip the hints in the snapshots and the prefix in other tests.
fn settings_saves_the_general_form() {
    let mut app = acme_app();
    app.open_settings(super::SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Down));
    let before = field(&app, "Updates");
    app.on_key(key(KeyCode::Right));
    assert_ne!(field(&app, "Updates"), before);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Saved"));
    assert_eq!(crate::keys::prefix(), crate::keys::Prefix::default());
    assert!(!app.config.auto_update);
    assert!(!store::load_config().unwrap().auto_update);
    // Esc closes Settings.
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn settings_moves_through_its_list() {
    let mut app = acme_app();
    app.open_settings(super::SettingsItem::Project(0), false);
    app.on_key(key(KeyCode::Down));
    assert_eq!(field(&app, "Name"), "notes");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], super::SettingsItem::Account(0));
    // An account a project uses can't go.
    app.on_key(ctrl('d'));
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(
        s.form.error.as_deref(),
        Some("Used by acme. Pick another account in their settings first.")
    );
}

#[test]
fn deleting_a_project_closes_its_tickets() {
    let mut app = acme_app();
    app.open_settings(super::SettingsItem::Project(1), false);
    app.on_key(ctrl('d'));
    assert!(matches!(app.mode, Mode::ConfirmDeleteProject(1)));
    draw(&mut app, 80, 24);
    // Esc goes back to Settings, the project kept.
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Settings(_)));
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Settings(_)));
    assert_eq!(app.projects.len(), 1);
    assert!(!app.tickets.iter().any(|tv| tv.t.key == "notes-writer"));
    assert_eq!(saved().projects.len(), 1);
}

/// A project whose removal can't be saved stays, its tickets where they were, their sessions
/// running and claimed: `taskman resume` still finds them.
#[test]
fn a_project_removal_that_can_not_be_saved_keeps_it() {
    let mut app = acme_app();
    lay_out(&mut app, 80, 24);
    // Among acme's tickets, so putting it back keeps the order.
    let writer = app.tickets.remove(index(&app, "notes-writer"));
    app.tickets.insert(2, writer);
    app.sel = 4;
    let _sent = attach(&mut app, "notes-writer", SCREEN);
    let hold = resume::take(&resume::owner_file("/src/notes".as_ref(), "notes-writer"));
    app.tickets[2].hold = Some(hold.unwrap().unwrap().taskman());
    let keys = |app: &App| app.tickets.iter().map(|tv| tv.t.key.clone()).collect::<Vec<_>>();
    let before = keys(&app);
    let blocked = block_saves();
    app.open_settings(super::SettingsItem::Project(1), false);
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Enter));
    let error = toast(&app).unwrap_or_default();
    assert!(error.starts_with("Kept project notes: couldn't save "), "{error}");
    assert_eq!(app.projects.len(), 2);
    assert_eq!((keys(&app), app.sel), (before, 4));
    let tv = &app.tickets[2];
    assert!(tv.session.is_some() && tv.hold.is_some(), "the session was stopped");
    std::fs::remove_dir_all(&blocked).unwrap();
}

/// The Settings form.
fn settings_form(app: &App) -> &super::Form {
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    &s.form
}

/// Moves to the Settings form's field `label` and types `text` over its value.
fn fill(app: &mut App, label: &str, text: &str) {
    for n in 0.. {
        if settings_form(app).fields[settings_form(app).focus].label == label {
            break;
        }
        assert!(n < 20, "no field {label} shown");
        app.on_key(key(KeyCode::Tab));
    }
    app.on_key(ctrl('u'));
    type_text(app, text);
}

/// Enter, and wait for the check it starts, if any (a connection test, which sends the token it
/// read and then its verdict).
fn submit(app: &mut App) {
    app.on_key(key(KeyCode::Enter));
    while settings_form(app).checking.is_some() {
        settle(app);
    }
}

/// config.json as saved, or "" before the first save.
fn config_file() -> String {
    std::fs::read_to_string(store::config_path()).unwrap_or_default()
}

const RAW_TOKEN_HINT: &str = "Enter where to read the token from, like env:JIRA_API_TOKEN or cmd:op read …, not the token itself";

#[test]
fn the_account_form_never_saves_a_token_it_has_no_site_to_test() {
    let mut app = acme_app();
    app.open_settings(super::SettingsItem::NewAccount, true);
    assert_eq!(field(&app, "Provider"), "Jira");
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Read from", "jira-raw-secret");
    submit(&mut app);
    assert_eq!(settings_form(&app).error.as_deref(), Some(RAW_TOKEN_HINT));
    submit(&mut app);
    assert_eq!(settings_form(&app).error.as_deref(), Some(RAW_TOKEN_HINT));
    assert_eq!(app.config.accounts.len(), 1);
    assert!(!config_file().contains("jira-raw-secret"));
    // A source saves, and the token typed before isn't in the file.
    fill(&mut app, "Read from", "env:JIRA_API_TOKEN");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved the Jira account (add a site to test it)"));
    let saved = config_file();
    assert!(saved.contains("env:JIRA_API_TOKEN") && !saved.contains("jira-raw-secret"), "{saved}");
}

#[test]
fn a_shell_variable_saves_as_env() {
    let mut app = acme_app();
    app.open_settings(super::SettingsItem::NewAccount, true);
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Read from", " $JIRA_API_TOKEN ");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved the Jira account (add a site to test it)"));
    let saved = store::load_config().unwrap();
    assert_eq!(saved.accounts[1].token, "env:JIRA_API_TOKEN");
    assert!(!config_file().contains("$JIRA_API_TOKEN"));
    // Anything but a plain name is refused, not run, and the form keeps it as typed.
    app.open_settings(super::SettingsItem::NewAccount, true);
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Read from", "$(printf jira-raw-secret)");
    submit(&mut app);
    assert_eq!(settings_form(&app).error.as_deref(), Some(RAW_TOKEN_HINT));
    assert_eq!(field(&app, "Read from"), "$(printf jira-raw-secret)");
    assert_eq!(store::load_config().unwrap().accounts.len(), 2);
}

#[test]
fn a_shell_variable_shows_as_env_in_the_form() {
    let mut app = acme_app();
    app.open_settings(super::SettingsItem::NewAccount, true);
    // A site that answers nothing keeps the form open on the failed check.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    fill(&mut app, "Site", &closed.to_string());
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Read from", "${JIRA_API_TOKEN}");
    submit(&mut app);
    assert!(settings_form(&app).error.is_some());
    assert_eq!(field(&app, "Read from"), "env:JIRA_API_TOKEN");
}

#[test]
fn saving_anyway_never_saves_a_token() {
    let mut app = acme_app();
    app.open_settings(super::SettingsItem::Account(0), true);
    assert_eq!(field(&app, "Provider"), "Linear");
    fill(&mut app, "Read from", "lin_api_raw_secret");
    // Save anyway is for a connection that failed: Enter again still refuses, and no test ran.
    for _ in 0..2 {
        submit(&mut app);
        assert_eq!(
            settings_form(&app).error.as_deref(),
            Some(
                "Enter where to read the token from, like env:LINEAR_API_KEY or cmd:op read …, not the token itself"
            )
        );
    }
    assert_eq!(app.config.accounts[0].token, "env:ACME_LINEAR_TOKEN");
    assert!(!config_file().contains("lin_api_raw_secret"));
}

#[test]
fn an_account_saved_with_a_token_says_what_to_change() {
    let mut app = acme_app();
    let old = serde_json::json!({ "id": "jira-1", "provider": "jira", "email": "dana@example.com", "token": "jira-raw-secret" });
    app.config.accounts.push(serde_json::from_value(old).unwrap());
    app.open_settings(super::SettingsItem::Account(1), false);
    let error = settings_form(&app).error.clone().unwrap_or_default();
    assert_eq!(error, format!("{RAW_TOKEN_HINT}, so config.json no longer keeps it"));
    // Using it says so too, without the token.
    let used = app.config.accounts[1].resolve_token().unwrap_err();
    assert!(used.contains("env:VAR or cmd:COMMAND") && !used.contains("jira-raw-secret"), "{used}");
}

// ---- `taskman add` through the inbox -------------------------------------------------------

/// Leaves a request in the inbox as `taskman add` does.
fn request(id: &str, req: &serde_json::Value) {
    let path = store::state_dir().join("inbox").join(format!("{id}.json"));
    store::write_atomic(&path, req.to_string().as_bytes()).unwrap();
}

fn reply(id: &str) -> Vec<add::Line> {
    store::read_json(&store::state_dir().join("inbox").join(format!("{id}.reply.json"))).unwrap()
}

#[test]
fn takes_tickets_from_the_inbox_and_replies() {
    let mut app = acme_app();
    let reply_by = crate::util::now() + 60;
    request(
        "0001",
        &serde_json::json!({
            "repo": "/src/acme", "from": "✦ lead", "worktree": false, "reply_by": reply_by,
            "items": [{ "kind": "ticket", "key": "ENG-90", "title": "Export to CSV" }, { "kind": "ticket", "key": "ENG-52" }],
        }),
    );
    app.take_inbox();
    assert!(app.tickets.iter().any(|tv| tv.t.key == "ENG-90" && tv.new));
    assert_eq!(selected(&app), "ENG-52", "an added ticket doesn't take the pane");
    let lines = reply("0001");
    assert!(lines.iter().all(|l| l.ok), "{lines:?}");
    assert!(lines[1].text.contains("already open"), "{lines:?}");
    assert!(toast(&app).is_some_and(|t| t.contains("ENG-90")), "{:?}", toast(&app));
    assert!(saved().tickets.iter().any(|t| t.key == "ENG-90"));
}

#[test]
fn an_agent_from_the_inbox_waits_for_the_dialog() {
    let mut app = acme_app();
    request(
        "0002",
        &serde_json::json!({
            "repo": "/src/acme", "from": "✦ lead", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "agent", "name": "tester", "role": "Run the tests" }],
        }),
    );
    app.take_inbox();
    assert!(reply("0002").iter().all(|l| l.ok));
    assert!(
        !app.tickets.iter().any(|tv| tv.t.key == "acme-tester"),
        "added before the user approved"
    );
    app.ask_next();
    assert!(matches!(app.mode, Mode::ConfirmAgent(_)));
}

/// `taskman close` always names tickets or asks for the merged ones; a request with neither,
/// which anything able to write the inbox could leave, would otherwise close every ticket.
#[test]
fn a_close_request_naming_nothing_closes_nothing() {
    let mut app = acme_app();
    let before: Vec<String> = app.tickets.iter().map(|tv| tv.t.key.clone()).collect();
    request(
        "0004",
        &serde_json::json!({
            "repo": "/src/acme", "reply_by": crate::util::now() + 60, "items": [], "close": {},
        }),
    );
    app.take_inbox();
    app.close_next();
    let lines = reply("0004");
    assert!(!lines[0].ok && lines[0].text.starts_with("nothing to close"), "{lines:?}");
    let after: Vec<String> = app.tickets.iter().map(|tv| tv.t.key.clone()).collect();
    assert_eq!(after, before);
    assert!(app.closing.is_empty() && app.removing.is_empty());
}

#[test]
fn a_request_for_a_project_that_is_gone_says_so() {
    let mut app = acme_app();
    request(
        "0003",
        &serde_json::json!({
            "repo": "/src/gone", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "ENG-90" }],
        }),
    );
    app.take_inbox();
    let lines = reply("0003");
    assert!(!lines[0].ok && lines[0].text.contains("no longer in taskman"), "{lines:?}");
}

// ---- accounts in Settings ------------------------------------------------------------------

/// Moves the open form's focus to its field `label`, as Tab does.
fn focus(app: &mut App, label: &str) {
    let focused = |app: &App| match &app.mode {
        Mode::Settings(s) => s.form.fields[s.form.focus].label,
        Mode::Form(f) => f.fields[f.focus].label,
        _ => panic!("no form open"),
    };
    for _ in 0..10 {
        if focused(app) == label {
            return;
        }
        app.on_key(key(KeyCode::Tab));
    }
    panic!("no field {label}");
}

/// The open Settings form's error.
fn form_error(app: &App) -> Option<String> {
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    s.form.error.clone()
}

/// Waits for the account form's connection check and answers it with `found`, as the tracker
/// would. The check's own thread only reads the token, which these tests make fail before any
/// request: no test reaches a tracker.
fn answer_check(app: &mut App, found: Result<(&str, &str), &str>) {
    let found = found.map(|(n, w)| (n.to_string(), w.to_string())).map_err(String::from);
    let mut before = Vec::new();
    loop {
        let msg = app.rx.recv_timeout(std::time::Duration::from_secs(10)).expect("no check");
        if let Msg::Checked(id, _) = msg {
            before.into_iter().for_each(|m| app.tx.send(m).unwrap());
            app.tx.send(Msg::Checked(id, super::forms::Check::Account(found))).unwrap();
            app.drain_messages();
            return;
        }
        before.push(msg);
    }
}

#[test]
fn adds_a_jira_account_and_saves_where_its_token_comes_from_not_the_token() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    assert_eq!(field(&app, "Provider"), "Jira");
    // Nothing that reads as done already.
    assert_eq!(field(&app, "Read from"), "");
    fill(&mut app, "Email", "");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(form_error(&app).as_deref(), Some("Enter the email you sign in to Jira with"));

    // A site to test against that answers nothing: the check fails, but the token is read.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    fill(&mut app, "Site", &closed.to_string());
    fill(&mut app, "Email", "you@example.com");
    fill(&mut app, "Read from", "cmd:printf %s-%s jira mock");
    app.on_key(key(KeyCode::Enter));
    while matches!(&app.mode, Mode::Settings(s) if s.form.checking.is_some()) {
        settle(&mut app);
    }
    let error = form_error(&app).unwrap();
    assert!(error.ends_with(". Fix it, or press Enter again to save anyway."), "{error}");
    assert!(store::load_config().unwrap().accounts.is_empty(), "saved before the second Enter");
    app.on_key(key(KeyCode::Enter));
    assert!(
        toast(&app)
            .is_some_and(|t| t.starts_with("Saved the account without a working connection"))
    );
    let config = std::fs::read_to_string(store::config_path()).unwrap();
    let saved = store::load_config().unwrap();
    let a = &saved.accounts[1];
    assert_eq!((a.id.as_str(), a.email.as_str()), ("jira-1", "you@example.com"));
    assert_eq!(a.site, format!("https://{closed}"));
    assert_eq!(a.token, "cmd:printf %s-%s jira mock");
    assert!(!config.contains("jira-mock"), "the token itself was saved: {config}");
    // Settings shows the new account, which no project uses yet.
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], super::SettingsItem::Account(1));
    assert_eq!(s.form.note.as_deref(), Some("Not used by any project"));
    assert!(!s.editing);
}

#[test]
fn adds_a_linear_account_once_its_key_is_the_workspaces() {
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Provider"), "Linear");
    assert_eq!(field(&app, "Read from"), "");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(
        form_error(&app).as_deref(),
        Some("Enter the Linear workspace (the part after linear.app/)")
    );
    fill(&mut app, "Site", "globex");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY");

    // The key is another workspace's: said, and not saved until a second Enter.
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "initech")));
    assert_eq!(
        form_error(&app).as_deref(),
        Some(
            "This key belongs to the workspace initech, not globex. Fix it, or press Enter again to save anyway."
        )
    );
    assert_eq!(store::load_config().map(|c| c.accounts.len()).unwrap_or(0), 0);

    // Fixed: checked again, and saved.
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY_2");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "globex")));
    assert_eq!(toast(&app), Some("✓ Connected to Linear globex as Dana Reyes"));
    let saved = store::load_config().unwrap();
    assert_eq!(saved.accounts.len(), 2);
    let a = &saved.accounts[1];
    assert_eq!((a.id.as_str(), a.site.as_str()), ("linear-2", "globex"));
    assert_eq!(a.token, "env:TASKMAN_TEST_UNSET_KEY_2");

    // The same account again is the one already there.
    app.on_key(key(KeyCode::Esc));
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    fill(&mut app, "Site", "globex");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY_2");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "globex")));
    assert_eq!(app.config.accounts.len(), 2);
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], super::SettingsItem::Account(1));
}

#[test]
fn a_second_linear_workspace_gets_a_key_of_its_own() {
    let mut app = acme_app();
    app.config.accounts[0].token = "env:LINEAR_API_KEY".into();
    app.keychain = Some(crate::keychain::Keychain::os());
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Provider"), "Linear");
    // The keychain first; picking a variable suggests one of its own.
    assert_eq!(field(&app, "Keep in"), "Keychain");
    assert_eq!(field(&app, "Read from"), "");
    focus(&mut app, "Keep in");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Read from"), "env:LINEAR_API_KEY_2");
    fill(&mut app, "Site", "globex-eu");
    assert_eq!(field(&app, "Read from"), "env:LINEAR_API_KEY_GLOBEX_EU");
    // A source the user typed stays.
    fill(&mut app, "Read from", "cmd:pass show linear");
    fill(&mut app, "Site", "initech");
    assert_eq!(field(&app, "Read from"), "cmd:pass show linear");
    // Back to Jira, a default token follows the provider.
    fill(&mut app, "Read from", "env:LINEAR_API_KEY");
    focus(&mut app, "Provider");
    app.on_key(key(KeyCode::Left));
    assert_eq!(field(&app, "Provider"), "Jira");
    assert_eq!(field(&app, "Read from"), "env:JIRA_API_TOKEN");
}

#[test]
fn without_a_keychain_the_variable_is_only_suggested() {
    let mut app = acme_app();
    app.config.accounts[0].token = "env:LINEAR_API_KEY".into();
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    fill(&mut app, "Site", "globex");
    let form = settings_form(&app);
    assert!(form.field("Keep in").unwrap().hidden && form.field("Token").unwrap().hidden);
    let read_from = form.field("Read from").unwrap();
    assert_eq!(read_from.value, "");
    assert!(read_from.hint.starts_with("env:LINEAR_API_KEY_GLOBEX reads"), "{}", read_from.hint);
    // Where to get a key, and the guide's other ways.
    let links: Vec<&str> = read_from.links.iter().map(|(_, l, _)| l.as_str()).collect();
    assert_eq!(links, ["https://linear.app/globex/settings/account/security", "Storing a token"]);
}

/// A keychain for the App: the stub in this test's scratch folder (`keychain::stub`). Returns
/// the stub's folder, where it logs its calls.
fn with_keychain(app: &mut App) -> std::path::PathBuf {
    let dir = crate::keychain::stub();
    app.keychain = Some(crate::keychain::Keychain::os());
    dir
}

/// A site that answers nothing, so a connection test fails at once and no tracker is reached.
fn closed_site() -> String {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string()
}

#[test]
fn saves_a_pasted_token_to_the_keychain_and_only_its_name_to_config() {
    let mut app = acme_app();
    let stub = with_keychain(&mut app);
    let label = crate::keychain::Keychain::os().label();
    prefix(&mut app, KeyCode::Char('A'));
    let form = settings_form(&app);
    assert_eq!(
        form.note.as_deref(),
        Some("taskman never saves your token in its files, only where to read it from.")
    );
    assert_eq!(field(&app, "Keep in"), "Keychain");
    assert!(form.field("Read from").unwrap().hidden);
    let token = form.field("Token").unwrap();
    assert!(token.masked && !token.hidden);
    assert_eq!(token.hint, format!("paste it here; taskman puts it in your {label}"));
    assert_eq!(token.links[0].1, "https://id.atlassian.com/manage-profile/security/api-tokens");
    fill(&mut app, "Site", &closed_site());
    fill(&mut app, "Email", "dana@example.com");

    // Not a token: refused before anything runs.
    fill(&mut app, "Token", "two words");
    submit(&mut app);
    let error = form_error(&app).unwrap();
    assert!(error.starts_with("That doesn't look like a token") && !error.contains("two"));
    assert!(!stub.join("calls").exists());

    // Saved to the keychain, read back and tested; the test fails, and the form forgets the token.
    fill(&mut app, "Token", "ATATT-mock-1");
    submit(&mut app);
    let error = form_error(&app).unwrap();
    assert!(error.ends_with(". Fix it, or press Enter again to save anyway."), "{error}");
    assert_eq!(field(&app, "Token"), "");
    assert!(store::load_config().unwrap().accounts.is_empty(), "saved before the second Enter");
    let k = crate::keychain::Keychain::os();
    assert_eq!(k.read("jira-1"), Ok("ATATT-mock-1".into()));
    submit(&mut app);
    let a = &store::load_config().unwrap().accounts[1];
    assert_eq!((a.id.as_str(), a.token.as_str()), ("jira-1", "keychain:jira-1"));
    assert!(!config_file().contains("ATATT"));
    let calls = std::fs::read_to_string(stub.join("calls")).unwrap();
    assert!(!calls.contains("ATATT"), "the token was an argument: {calls}");

    // Its settings say where the token is; Enter saves it again without pasting it.
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], super::SettingsItem::Account(1));
    let form = settings_form(&app);
    assert_eq!(field(&app, "Keep in"), "Keychain");
    assert_eq!(
        form.field("Token").unwrap().hint,
        format!("in your {label}; paste a new one to replace it")
    );
    app.on_key(key(KeyCode::Enter));
    submit(&mut app);
    assert!(form_error(&app).unwrap().ends_with("press Enter again to save anyway."));
    // A new one replaces it, under the same name.
    fill(&mut app, "Token", "ATATT-mock-2");
    submit(&mut app);
    submit(&mut app);
    assert_eq!(k.read("jira-1"), Ok("ATATT-mock-2".into()));
    assert_eq!(store::load_config().unwrap().accounts[1].token, "keychain:jira-1");
    assert_eq!(app.config.accounts.len(), 2);
}

#[test]
fn a_jira_account_with_no_site_is_saved_to_the_keychain_untested() {
    let mut app = acme_app();
    with_keychain(&mut app);
    prefix(&mut app, KeyCode::Char('A'));
    fill(&mut app, "Email", "dana@example.com");
    submit(&mut app);
    assert_eq!(form_error(&app).as_deref(), Some("Paste the token into the Token field"));
    fill(&mut app, "Token", "ATATT-mock");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved the Jira account (add a site to test it)"));
    assert_eq!(store::load_config().unwrap().accounts[1].token, "keychain:jira-1");
    assert_eq!(crate::keychain::Keychain::os().read("jira-1"), Ok("ATATT-mock".into()));
}

#[test]
fn a_keychain_that_fails_saves_nothing() {
    let mut app = acme_app();
    let stub = with_keychain(&mut app);
    std::fs::write(stub.join("fail"), "The keyring is locked").unwrap();
    prefix(&mut app, KeyCode::Char('A'));
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Token", "ATATT-mock");
    // No "save anyway": Enter again tries again.
    for _ in 0..2 {
        submit(&mut app);
        let error = form_error(&app).unwrap();
        assert!(
            error.contains("The keyring is locked")
                && error.ends_with(". Fix it, or pick Variable or command under Keep in."),
            "{error}"
        );
    }
    assert_eq!(field(&app, "Token"), "ATATT-mock", "kept to try again");
    assert!(store::load_config().map(|c| c.accounts.len()).unwrap_or(0) < 2);
}

#[test]
fn an_unset_variable_says_how_to_set_it_and_offers_the_keychain() {
    let mut app = acme_app();
    with_keychain(&mut app);
    prefix(&mut app, KeyCode::Char('A'));
    fill(&mut app, "Site", &closed_site());
    fill(&mut app, "Email", "dana@example.com");
    focus(&mut app, "Keep in");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Read from"), "env:JIRA_API_TOKEN");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_TOKEN");
    submit(&mut app);
    let error = form_error(&app).unwrap();
    assert!(
        error.starts_with("TASKMAN_TEST_UNSET_TOKEN isn't set in the shell taskman started from: add export TASKMAN_TEST_UNSET_TOKEN=… to ")
            && error.ends_with(
                ". Or pick Keychain under Keep in and paste the token there. Fix it, or press Enter again to save anyway."
            ),
        "{error}"
    );
}

#[test]
fn deleting_an_account_can_delete_its_keychain_item() {
    let mut app = acme_app();
    with_keychain(&mut app);
    let k = crate::keychain::Keychain::os();
    k.store("jira-1", "ATATT-mock").unwrap();
    let account = serde_json::json!({ "id": "jira-1", "provider": "jira", "email": "dana@example.com",
                                      "token": "keychain:jira-1" });
    app.config.accounts.push(serde_json::from_value(account).unwrap());
    app.open_settings(super::SettingsItem::Account(1), false);
    app.on_key(ctrl('d'));
    assert!(matches!(app.mode, Mode::ConfirmDeleteAccount(1)));
    // Enter keeps the token: Esc here, and `d` deletes both.
    app.on_key(key(KeyCode::Esc));
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Char('d')));
    assert_eq!(toast(&app), Some("Deleted account dana@example.com · keychain"));
    assert_eq!(store::load_config().unwrap().accounts.len(), 1);
    settle(&mut app);
    assert_eq!(
        toast(&app).map(String::from),
        Some(format!(
            "Deleted account dana@example.com · keychain and its token in your {}",
            k.label()
        ))
    );
    assert!(k.read("jira-1").is_err());
    // An account without an item of its own has no `d`.
    let account = serde_json::json!({ "id": "jira-2", "provider": "jira", "email": "dana@example.com",
                                      "token": "env:JIRA_API_TOKEN" });
    app.config.accounts.push(serde_json::from_value(account).unwrap());
    app.open_settings(super::SettingsItem::Account(1), false);
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(app.mode, Mode::ConfirmDeleteAccount(1)));
}

#[test]
fn edits_an_account_in_place() {
    let mut app = acme_app();
    app.open_settings(super::SettingsItem::Account(0), false);
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.form.note.as_deref(), Some("Used by acme"));
    app.on_key(key(KeyCode::Enter));
    // The provider is fixed; a token source is what changes.
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Provider"), "Linear");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "acme")));
    assert_eq!(toast(&app), Some("✓ Connected to Linear acme as Dana Reyes"));
    let saved = store::load_config().unwrap();
    assert_eq!(saved.accounts.len(), 1);
    assert_eq!(saved.accounts[0].id, "linear-1", "acme's tracker names it by its id");
    assert_eq!(saved.accounts[0].token, "env:TASKMAN_TEST_UNSET_KEY");
    assert_eq!(app.account_users(0), ["acme"]);
}

#[test]
fn deletes_an_account_no_project_uses() {
    let mut app = acme_app();
    let unused = serde_json::json!({ "id": "jira-1", "provider": "jira", "site": "https://example.atlassian.net",
                                     "email": "you@example.com", "token": "env:JIRA_API_TOKEN" });
    app.config.accounts.push(serde_json::from_value(unused).unwrap());
    app.open_settings(super::SettingsItem::Account(1), false);
    app.on_key(ctrl('d'));
    assert!(matches!(app.mode, Mode::ConfirmDeleteAccount(1)));
    draw(&mut app, 80, 24);
    // Esc keeps it.
    app.on_key(key(KeyCode::Esc));
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], super::SettingsItem::Account(1));
    app.on_key(ctrl('d'));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Deleted account you@example.com · env:JIRA_API_TOKEN"));
    assert_eq!(store::load_config().unwrap().accounts.len(), 1);
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], super::SettingsItem::Account(0));
}

// ---- GitHub Issues -------------------------------------------------------------------------

/// `acme_app` with project web on GitHub Issues (acme/web, prefix WEB), whose folder isn't a git
/// repository, so nothing runs git, and whose open issues are listed already, so the Add form
/// doesn't ask gh.
fn web_app() -> App {
    let mut app = acme_app();
    let web = serde_json::json!({
        "name": "web", "repo": "/src/web",
        "tracker": { "provider": "github", "site": "acme/web", "key": "WEB" },
    });
    app.projects.push(serde_json::from_value(web).unwrap());
    app.github.insert("/src/web".into(), crate::github::Gh::NotGit);
    let issue = |number, title: &str, mine| crate::tracker::Listed {
        key: format!("WEB-{number}"),
        title: title.into(),
        mine,
    };
    let list = vec![
        issue(9, "Crash on start", true),
        issue(15, "Fix the login form", false),
        issue(14, "Fix the search page", false),
    ];
    let listed = super::add_form::OpenIssues::Listed(list);
    app.open_issues.insert("/src/web".into(), (std::time::Instant::now(), listed));
    app
}

/// The open issues listed under the Add form's Ticket field.
fn listed(app: &App) -> Vec<String> {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.field("Ticket").unwrap().list.lines.clone()
}

/// The Add form's error.
fn add_error(app: &App) -> Option<&str> {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.error.as_deref()
}

fn open_in_web(app: &App, key: &str) -> bool {
    app.tickets.iter().any(|tv| tv.t.is(std::path::Path::new("/src/web"), key))
}

#[test]
fn the_add_form_lists_open_issues_and_adds_one_by_its_title() {
    let mut app = web_app();
    app.open_ticket_form(Some(app.projects.len() - 1));
    assert_eq!(field(&app, "Project"), "web");
    assert_eq!(
        listed(&app),
        [
            "#9 Crash on start · assigned to you",
            "#15 Fix the login form",
            "#14 Fix the search page"
        ]
    );
    type_text(&mut app, "fix");
    assert_eq!(listed(&app), ["#15 Fix the login form", "#14 Fix the search page"]);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(add_error(&app), Some("2 open issues match; type more of the title, or the number"));
    type_text(&mut app, " LOGIN");
    assert_eq!(listed(&app), ["#15 Fix the login form"]);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(open_in_web(&app, "WEB-15"));
    assert!(saved().tickets.iter().any(|t| t.key == "WEB-15" && t.repo.as_os_str() == "/src/web"));
}

#[test]
fn the_add_form_takes_issue_numbers_and_links_and_refuses_pull_requests() {
    let mut app = web_app();
    app.open_ticket_form(Some(app.projects.len() - 1));
    // Digits list the issues whose number starts with them.
    type_text(&mut app, "1");
    assert_eq!(listed(&app), ["#15 Fix the login form", "#14 Fix the search page"]);
    app.on_key(ctrl('u'));
    type_text(&mut app, "https://github.com/acme/web/pull/45");
    let pr = "acme/web#45 is a pull request; add the issue it closes instead";
    assert_eq!(listed(&app), [pr]);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(add_error(&app), Some(pr));
    app.on_key(ctrl('u'));
    type_text(&mut app, "https://github.com/acme/web/issues/9");
    assert_eq!(listed(&app), ["#9 Crash on start · assigned to you"]);
    app.on_key(ctrl('u'));
    // A closed issue isn't listed, and is added all the same.
    type_text(&mut app, "#3");
    assert_eq!(listed(&app), ["No open issue's number starts with 3; Enter adds it as it is"]);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(open_in_web(&app, "WEB-3"));
}

#[test]
fn the_add_form_says_when_the_open_issues_could_not_be_listed() {
    let mut app = web_app();
    let failed =
        super::add_form::OpenIssues::Failed("gh isn't logged in; run gh auth login".into());
    app.open_issues.insert("/src/web".into(), (std::time::Instant::now(), failed));
    app.open_ticket_form(Some(app.projects.len() - 1));
    assert_eq!(
        listed(&app),
        ["Couldn't list the open issues: gh isn't logged in; run gh auth login"]
    );
    type_text(&mut app, "crash");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(add_error(&app), Some("Couldn't list the open issues; type the issue's number"));
    // A list that arrives while the form is open shows at once.
    let listing = app.start_listing(app.projects.len() - 1).unwrap();
    app.open_issues_listed(
        listing,
        Ok(vec![crate::tracker::Listed {
            key: "WEB-9".into(),
            title: "Crash on start".into(),
            mine: false,
        }]),
    );
    assert_eq!(listed(&app), ["#9 Crash on start"]);
}

/// An open issue, as gh lists it.
fn open_issue(number: u64, title: &str) -> crate::tracker::Listed {
    crate::tracker::Listed { key: format!("WEB-{number}"), title: title.into(), mine: false }
}

#[test]
fn a_list_of_open_issues_from_before_the_tracker_changed_is_dropped() {
    let mut app = web_app();
    let p = app.projects.len() - 1;
    app.open_issues.remove(std::path::Path::new("/src/web"));
    let old = app.start_listing(p).unwrap();
    // The project moves to another repository while acme/web's issues are listed, and lists the
    // new one's.
    let mut project = app.projects[p].clone();
    project.tracker.as_mut().unwrap().site = "acme/shop".into();
    app.update_project(p, project);
    let new = app.start_listing(p).unwrap();
    app.tx.send(Msg::OpenIssues(new, Ok(vec![open_issue(4, "Checkout is slow")]))).unwrap();
    // acme/web's list lands last: its numbers aren't acme/shop's issues.
    app.tx.send(Msg::OpenIssues(old, Ok(vec![open_issue(15, "Fix the login form")]))).unwrap();
    app.drain_messages();
    app.mode = Mode::Normal;
    app.open_ticket_form(Some(p));
    assert_eq!(listed(&app), ["#4 Checkout is slow"]);
    type_text(&mut app, "login");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(
        add_error(&app),
        Some("No open issue's title has \"login\"; type the issue's number")
    );
    assert!(!open_in_web(&app, "WEB-15"));
}

#[test]
fn a_list_of_open_issues_for_a_deleted_project_is_dropped() {
    let mut app = web_app();
    let p = app.projects.len() - 1;
    let web = std::path::Path::new("/src/web");
    app.open_issues.remove(web);
    let old = app.start_listing(p).unwrap();
    let project = app.projects[p].clone();
    app.delete_project(p);
    let late = || Ok(vec![open_issue(15, "Fix the login form")]);
    app.tx.send(Msg::OpenIssues(old.clone(), late())).unwrap();
    app.drain_messages();
    assert!(!app.open_issues.contains_key(web));
    // Added again, the project waits on a list of its own; the old one, landing late, isn't it.
    app.projects.push(project);
    let new = app.start_listing(p).unwrap();
    app.tx.send(Msg::OpenIssues(old, late())).unwrap();
    app.tx.send(Msg::OpenIssues(new, Ok(vec![open_issue(9, "Crash on start")]))).unwrap();
    app.drain_messages();
    app.open_ticket_form(Some(p));
    assert_eq!(listed(&app), ["#9 Crash on start"]);
}

// ---- Open issues on every tracker, picked with ↑/↓ ----------------------------------------------

/// An open issue of acme's (`ENG`).
fn eng(n: u64, title: &str, mine: bool) -> crate::tracker::Listed {
    crate::tracker::Listed { key: format!("ENG-{n}"), title: title.into(), mine }
}

/// `acme_app` with the Add form open on project acme, whose open issues are `issues`, listed
/// already so nothing asks the tracker; its folder isn't a git repository, so nothing runs git.
fn adding_in_acme(issues: Vec<crate::tracker::Listed>) -> App {
    let mut app = acme_app();
    app.github.insert("/src/acme".into(), crate::github::Gh::NotGit);
    let listed = super::add_form::OpenIssues::Listed(issues);
    app.open_issues.insert("/src/acme".into(), (std::time::Instant::now(), listed));
    app.open_ticket_form(Some(0));
    app
}

/// The Add form's open-issue list, with its highlight.
fn open_list(app: &App) -> super::List {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.field("Ticket").unwrap().list.clone()
}

/// A field's hint in the open form.
fn hint(app: &App, label: &str) -> String {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.field(label).unwrap().hint.clone()
}

fn focused(app: &App) -> &'static str {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.fields[f.focus].label
}

fn open_in_acme(app: &App, key: &str) -> bool {
    app.tickets.iter().any(|tv| tv.t.is(std::path::Path::new("/src/acme"), key))
}

#[test]
fn the_add_form_lists_linear_and_jira_issues_by_their_keys() {
    let issues = || {
        vec![
            eng(64, "Fix the login page", true),
            eng(71, "Export to CSV", false),
            eng(58, "Slow search", false),
        ]
    };
    let mut app = adding_in_acme(issues());
    assert_eq!(
        listed(&app),
        [
            "ENG-64 Fix the login page · assigned to you",
            "ENG-71 Export to CSV",
            "ENG-58 Slow search"
        ]
    );
    assert_eq!(
        hint(&app, "Ticket"),
        "a number (ENG-12), a key, a link, or words from an open issue's title"
    );
    type_text(&mut app, "csv");
    assert_eq!(listed(&app), ["ENG-71 Export to CSV"]);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(open_in_acme(&app, "ENG-71"));

    // On Jira, the same: keys name an issue, open or not.
    let mut app = acme_app();
    let jira = r#"{ "provider": "jira", "site": "https://example.atlassian.net", "key": "ENG" }"#;
    app.projects[0].tracker = Some(serde_json::from_str(jira).unwrap());
    app.github.insert("/src/acme".into(), crate::github::Gh::NotGit);
    let listed_now = super::add_form::OpenIssues::Listed(issues());
    app.open_issues.insert("/src/acme".into(), (std::time::Instant::now(), listed_now));
    app.open_ticket_form(Some(0));
    type_text(&mut app, "eng-58");
    assert_eq!(listed(&app), ["ENG-58 Slow search"]);
    app.on_key(ctrl('u'));
    type_text(&mut app, "ENG-99");
    assert_eq!(listed(&app), ["ENG-99 isn't an open issue; Enter adds it as it is"]);
    app.on_key(ctrl('u'));
    type_text(&mut app, "6");
    assert_eq!(listed(&app), ["ENG-64 Fix the login page · assigned to you"]);
}

#[test]
fn the_add_form_lists_a_boards_tickets_not_done_and_adds_the_one_picked() {
    let dir = store::state_dir().join("plan");
    let board = crate::local::board(&dir, crate::local::FOLDER);
    crate::local::ensure_columns(&board).unwrap();
    let write = |path: &str, content: &str| std::fs::write(board.join(path), content).unwrap();
    write("todo/WP-1-fix-login.md", "# Fix the login form\n");
    write("doing/WP-2-export.md", "# Export to CSV\n");
    write("done/WP-3-old.md", "# Fix the old login\n");
    let mut app = acme_app();
    let plan = serde_json::json!({
        "name": "plan", "repo": dir,
        "tracker": { "provider": "local", "site": "", "key": "WP" },
    });
    app.projects.push(serde_json::from_value(plan).unwrap());
    app.github.insert(dir.clone(), crate::github::Gh::NotGit);
    let p = app.projects.len() - 1;
    app.open_ticket_form(Some(p));
    // Read from the board in the background, as a tracker's are fetched.
    settle(&mut app);
    assert_eq!(listed(&app), ["WP-1 Fix the login form", "WP-2 Export to CSV"]);
    // A title still makes a new ticket, the list showing the ones it's like.
    type_text(&mut app, "login");
    assert_eq!(listed(&app), ["WP-1 Fix the login form"]);
    assert_eq!(hint(&app, "Ticket"), "new: WP-4 in todo, titled as typed");
    // Picked, the ticket on the board is added instead.
    app.on_key(key(KeyCode::Down));
    assert_eq!(hint(&app, "Ticket"), "workplan/todo/WP-1-fix-login.md in todo");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(app.tickets.iter().any(|tv| tv.t.is(&dir, "WP-1")));
    assert_eq!(crate::local::files(&board).len(), 3);
}

/// Eight open issues: five shown at a time.
fn eight() -> Vec<crate::tracker::Listed> {
    (1..=8).map(|n| eng(n, &format!("Task {n}"), false)).collect()
}

#[test]
fn arrows_move_a_highlight_through_the_open_issues_and_scroll_past_five() {
    let mut app = adding_in_acme(eight());
    let lines = |first: u64, last: u64| (first..=last).map(|n| format!("ENG-{n} Task {n}"));
    let window = |first: u64, more: &str| lines(first, first + 4).chain([more.to_string()]);
    assert_eq!(listed(&app), window(1, "↓ 3 more; type to narrow").collect::<Vec<_>>());
    assert_eq!(open_list(&app).highlighted(), None);
    // The first ↓ highlights the first issue; the field keeps the focus.
    app.on_key(key(KeyCode::Down));
    assert_eq!((open_list(&app).at, open_list(&app).highlighted()), (Some(0), Some(0)));
    assert_eq!(focused(&app), "Ticket");
    for _ in 0..5 {
        app.on_key(key(KeyCode::Down));
    }
    // Past the fifth, the window scrolls to keep it in view, its counts above and below.
    assert_eq!(open_list(&app).at, Some(5));
    let mid = "↑ 1 more · ↓ 2 more; type to narrow";
    assert_eq!(listed(&app), window(2, mid).collect::<Vec<_>>());
    assert_eq!(open_list(&app).highlighted(), Some(4));
    for _ in 0..2 {
        app.on_key(key(KeyCode::Down));
    }
    // The last; ↓ past it leaves the field (`arrows_past_a_list_leave_the_field_and_pick_nothing`).
    assert_eq!(open_list(&app).at, Some(7));
    assert_eq!(listed(&app), window(4, "↑ 3 more; type to narrow").collect::<Vec<_>>());
    // ↑ moves back up; the window moves only once the highlight leaves it.
    app.on_key(key(KeyCode::Up));
    assert_eq!(listed(&app)[0], "ENG-4 Task 4");
    for _ in 0..4 {
        app.on_key(key(KeyCode::Up));
    }
    assert_eq!(open_list(&app).at, Some(2));
    assert_eq!(listed(&app), window(3, "↑ 2 more · ↓ 1 more; type to narrow").collect::<Vec<_>>());
    assert_eq!(open_list(&app).highlighted(), Some(0));
    // Enter adds the highlighted issue, though nothing was typed.
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal), "{:?}", toast(&app));
    assert!(open_in_acme(&app, "ENG-3"));
}

#[test]
fn arrows_past_a_list_leave_the_field_and_pick_nothing() {
    let mut app = adding_in_acme(eight());
    assert_eq!(focused(&app), "Ticket");
    type_text(&mut app, "task");
    // ↑ on the first issue goes back to what's typed; ↑ again to the field above, Base, and on
    // up through the fields, the typed text kept.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Up));
    assert_eq!((focused(&app), open_list(&app).at), ("Ticket", None));
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Base");
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Worktree");
    assert_eq!(field(&app, "Ticket"), "task");
    // ↓ comes back down to the field, then into the list; Ticket is last, so ↓ at its last
    // issue stays there rather than wrapping round to the top.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Ticket");
    for _ in 0..8 {
        app.on_key(key(KeyCode::Down));
    }
    assert_eq!((focused(&app), open_list(&app).at), ("Ticket", Some(7)));
    app.on_key(key(KeyCode::Down));
    assert_eq!((focused(&app), open_list(&app).at), ("Ticket", Some(7)));
    // Tab leaves a highlighted list too, and keeps the issue picked, as before.
    app.on_key(ctrl('u'));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Tab));
    assert_eq!(focused(&app), "Project");
    app.on_key(key(KeyCode::Enter));
    assert!(open_in_acme(&app, "ENG-2"), "{:?}", toast(&app));
}

#[test]
fn arrows_past_the_project_list_leave_the_field() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    fill(&mut app, "Workspace", "acme");
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Ok(&ENG_OPS));
    focus(&mut app, "Team");
    let at = |app: &App| settings_form(app).field("Team").unwrap().list.at;
    let focused = |app: &App| settings_form(app).fields[settings_form(app).focus].label;
    // ↑ out of the top: back to what's typed, then the field above.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Up));
    assert_eq!((focused(&app), at(&app)), ("Team", None));
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Account");
    // Team is the form's last field: ↓ at the bottom of its list stays there, nothing taken.
    focus(&mut app, "Team");
    type_text(&mut app, "o");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!((focused(&app), at(&app)), ("Team", Some(0)));
    assert_eq!(field(&app, "Team"), "o");
    // Tab out of a highlighted list takes the one highlighted.
    focus(&mut app, "Team");
    app.on_key(ctrl('u'));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Tab));
    assert_eq!((focused(&app), field(&app, "Team").as_str()), ("Path", "OPS"));
}

#[test]
fn typing_drops_the_highlight_and_arrows_elsewhere_move_between_fields() {
    let mut app = adding_in_acme(eight());
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(open_list(&app).at, Some(1));
    // Typing narrows the list and starts again, nothing highlighted.
    type_text(&mut app, "task");
    assert_eq!((open_list(&app).at, open_list(&app).top), (None, 0));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(add_error(&app), Some("8 open issues match; type more of the title, or the number"));
    type_text(&mut app, " 7");
    app.on_key(key(KeyCode::Down));
    assert_eq!(open_list(&app).highlighted(), Some(0));
    app.on_key(key(KeyCode::Backspace));
    assert_eq!(open_list(&app).at, None);
    // With no issue listed, ↓ and ↑ move between fields as before.
    app.on_key(ctrl('u'));
    type_text(&mut app, "nothing like it");
    assert_eq!(listed(&app), ["No open issue's title has those words"]);
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Base");
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Ticket");
    app.on_key(ctrl('u'));
    assert_eq!(open_list(&app).picks, 8);
    // Away from the Ticket field, they move between fields though the list shows.
    app.on_key(key(KeyCode::BackTab));
    app.on_key(key(KeyCode::Up));
    assert_eq!(focused(&app), "Worktree");
    assert_eq!(open_list(&app).at, None);
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(focused(&app), "Ticket");
    assert_eq!(open_list(&app).at, None);
}

#[test]
fn github_issues_are_added_and_closed_through_the_inbox() {
    let mut app = web_app();
    request(
        "web-add",
        &serde_json::json!({
            "repo": "/src/web", "from": "✦ lead", "worktree": false, "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "WEB-12", "title": "Login fails with SSO" }],
        }),
    );
    app.take_inbox();
    assert!(reply("web-add").iter().all(|l| l.ok));
    assert!(open_in_web(&app, "WEB-12"));
    // No worktree and nothing merged asked for, so it closes without a PR lookup.
    request(
        "web-close",
        &serde_json::json!({
            "repo": "/src/web", "reply_by": crate::util::now() + 60,
            "items": [{ "kind": "ticket", "key": "WEB-12" }], "close": {},
        }),
    );
    app.take_inbox();
    app.close_next();
    app.drain_messages();
    assert!(!open_in_web(&app, "WEB-12"));
    let lines = reply("web-close");
    assert!(lines[0].ok && lines[0].text.starts_with("Closed WEB-12"), "{lines:?}");
    assert!(!saved().tickets.iter().any(|t| t.key == "WEB-12"));
}

/// Moves to the Settings form's choice `label` and cycles it to `value`.
fn choose(app: &mut App, label: &str, value: &str) {
    while settings_form(app).fields[settings_form(app).focus].label != label {
        app.on_key(key(KeyCode::Tab));
    }
    for _ in 0..10 {
        if settings_form(app).value(label) == value {
            return;
        }
        app.on_key(key(KeyCode::Right));
    }
    panic!("{label} has no {value}");
}

#[test]
fn a_github_issues_tracker_takes_its_repository_from_the_remote() {
    let dir = store::state_dir().join("web");
    std::fs::create_dir_all(&dir).unwrap();
    // An SSH alias, as people with a work and a personal account set up, is github.com to gh.
    sh(&dir, "git init -q && git remote add origin git@github.com-work:acme/web.git");
    let mut app = acme_app();
    let web = serde_json::json!({ "name": "web", "repo": dir });
    app.projects.push(serde_json::from_value(web).unwrap());
    let p = app.projects.len() - 1;
    app.open_settings(super::SettingsItem::Project(p), true);
    choose(&mut app, "Tracker", "GitHub Issues");
    assert!(settings_form(&app).field("Site").unwrap().hidden);
    assert!(settings_form(&app).field("Account").unwrap().hidden);
    assert_eq!(settings_form(&app).value("Ticket prefix"), "WEB");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved web"));
    let t = app.projects[p].tracker.clone().unwrap();
    assert_eq!(
        (t.provider, t.site.as_str(), t.key.as_str()),
        (store::Provider::Github, "acme/web", "WEB")
    );
    assert_eq!(
        app.issue_url(
            &serde_json::from_value(
                serde_json::json!({ "key": "WEB-12", "repo": dir, "cwd": dir })
            )
            .unwrap()
        )
        .as_deref(),
        Some("https://github.com/acme/web/issues/12")
    );
    // Without a GitHub remote it can't be saved, and the form says why.
    sh(&dir, "git remote remove origin");
    // Saving left the form; Enter edits it again.
    app.on_key(key(KeyCode::Enter));
    submit(&mut app);
    let e = settings_form(&app).error.clone().unwrap_or_default();
    assert!(
        e.starts_with(
            "GitHub Issues reads the repository from the project's GitHub remote, and it has none"
        ),
        "{e}"
    );
}

#[test]
fn an_empty_start_takes_the_keys_bare_and_after_the_prefix() {
    // The welcome screen names the prefix form, which works anywhere; bare keys still work here.
    let empty = || offline(Saved::default());
    for press in [
        |app: &mut App| app.on_key(key(KeyCode::Char('P'))),
        |app: &mut App| prefix(app, KeyCode::Char('P')),
    ] {
        let mut app = empty();
        press(&mut app);
        assert!(matches!(app.mode, Mode::Settings(_)));
    }
    let with_a_project = || offline(Saved { tickets: vec![], ..super::test_support::acme() });
    for press in [
        |app: &mut App| app.on_key(key(KeyCode::Char('n'))),
        |app: &mut App| prefix(app, KeyCode::Char('n')),
    ] {
        let mut app = with_a_project();
        press(&mut app);
        assert!(matches!(app.mode, Mode::Form(_)));
    }
}

// ---- the project forms: sign in, then pick the project or team -----------------------------

/// `ctrl+a P` on a folder of this test's, with `tracker` picked; returns the folder.
fn new_project(app: &mut App, tracker: &str) -> std::path::PathBuf {
    let dir = store::state_dir().join("web");
    std::fs::create_dir_all(&dir).unwrap();
    prefix(app, KeyCode::Char('P'));
    fill(app, "Path", &dir.display().to_string());
    choose(app, "Tracker", tracker);
    dir
}

/// The labels of the Settings form's fields shown.
fn shown(app: &App) -> Vec<&'static str> {
    settings_form(app).fields.iter().filter(|f| !f.hidden).map(|f| f.label).collect()
}

fn hint_of(app: &App, label: &str) -> String {
    settings_form(app).field(label).unwrap().hint.clone()
}

fn listed_under(app: &App, label: &str) -> Vec<String> {
    settings_form(app).field(label).unwrap().list.lines.clone()
}

/// The project saved last, as tickets.json has it.
fn saved_tracker() -> store::Tracker {
    saved().projects.last().and_then(|p| p.tracker.clone()).unwrap()
}

const ENG_OPS: [(&str, &str); 2] = [("ENG", "Engineering"), ("OPS", "Operations")];

#[test]
fn a_new_jira_project_signs_in_then_lists_the_projects_to_pick_from() {
    let mut app = offline(Saved::default());
    let stub = with_keychain(&mut app);
    new_project(&mut app, "Jira");
    // The site, then a new account's own fields in the same form: no second form.
    assert_eq!(
        shown(&app),
        ["Path", "Tracker", "Site", "Account", "Email", "Keep in", "Token", "Project"]
    );
    assert_eq!(field(&app, "Account"), "+ new account");
    assert!(hint_of(&app, "Account").starts_with("taskman never saves your token in its files"));
    assert!(hint_of(&app, "Project").starts_with("listed once taskman can sign in"));
    fill(&mut app, "Site", "example.atlassian.net");
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Token", "ATATT-mock-1");
    // Nothing is asked while the sign-in is typed; leaving the token lists the projects.
    assert!(settings_form(&app).choices.is_none());
    app.on_key(key(KeyCode::Tab));
    assert_eq!(settings_form(&app).fields[settings_form(&app).focus].label, "Project");
    assert_eq!(listed_under(&app, "Project"), ["Listing the projects this account can see…"]);
    answer_choices(&mut app, Ok(&ENG_OPS));
    assert_eq!(listed_under(&app, "Project"), ["ENG  Engineering", "OPS  Operations"]);
    // Typing narrows them, by key or name; ↓ picks.
    type_text(&mut app, "oper");
    assert_eq!(listed_under(&app, "Project"), ["OPS  Operations"]);
    app.on_key(ctrl('u'));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert!(config_file().is_empty());

    // Enter: the token goes to the keychain, read back and tested; then both are saved.
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "")));
    assert_eq!(
        toast(&app),
        Some("Added project web · ✓ Connected to example.atlassian.net as Dana Reyes")
    );
    let a = &store::load_config().unwrap().accounts[0];
    assert_eq!(
        (a.id.as_str(), a.email.as_str(), a.site.as_str(), a.token.as_str()),
        ("jira-1", "dana@example.com", "https://example.atlassian.net", "keychain:jira-1")
    );
    assert_eq!(crate::keychain::Keychain::os().read("jira-1"), Ok("ATATT-mock-1".into()));
    assert!(!config_file().contains("ATATT"));
    let calls = std::fs::read_to_string(stub.join("calls")).unwrap();
    assert!(!calls.contains("ATATT"), "the token was an argument: {calls}");
    let t = saved_tracker();
    assert_eq!(
        (t.provider, t.site.as_str(), t.key.as_str(), t.account.as_deref()),
        (store::Provider::Jira, "https://example.atlassian.net", "OPS", Some("jira-1"))
    );
    // And the Add form opens on it.
    assert!(matches!(&app.mode, Mode::Form(f) if f.value("Project") == "web"));
}

#[test]
fn a_saved_account_that_fits_is_picked_and_lists_at_once() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    assert_eq!(shown(&app), ["Path", "Tracker", "Workspace", "Account", "Read from", "Team"]);
    fill(&mut app, "Workspace", "acme");
    // acme's key is for this workspace: picked, and its fields go.
    assert_eq!(field(&app, "Account"), "acme · env:ACME_LINEAR_TOKEN");
    assert_eq!(shown(&app), ["Path", "Tracker", "Workspace", "Account", "Team"]);
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Ok(&ENG_OPS));
    fill(&mut app, "Team", "ops");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Added project web"));
    let t = saved_tracker();
    assert_eq!(
        (t.site.as_str(), t.key.as_str(), t.account.as_deref()),
        ("acme", "OPS", Some("linear-1"))
    );
    assert_eq!(app.config.accounts.len(), 1);
}

#[test]
fn a_saved_account_picked_brings_its_workspace() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    assert_eq!(field(&app, "Account"), "+ new account");
    choose(&mut app, "Account", "acme · env:ACME_LINEAR_TOKEN");
    assert_eq!(field(&app, "Workspace"), "acme");
}

#[test]
fn a_list_that_fails_says_why_and_takes_a_key_typed() {
    let mut app = offline(Saved::default());
    new_project(&mut app, "Jira");
    fill(&mut app, "Site", "example.atlassian.net");
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_TOKEN");
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Err("Jira request failed: network unreachable"));
    let hint = hint_of(&app, "Project");
    assert!(
        hint.starts_with("Couldn't list the projects: Jira request failed: network unreachable.")
            && hint.contains("Type its key (ENG for ENG-12) to save without the list"),
        "{hint}"
    );
    assert!(listed_under(&app, "Project").is_empty());
    type_text(&mut app, "eng");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "")));
    assert_eq!(saved_tracker().key, "ENG");
    assert_eq!(store::load_config().unwrap().accounts[0].token, "env:TASKMAN_TEST_UNSET_TOKEN");
}

#[test]
fn a_connection_that_fails_saves_on_a_second_enter() {
    let mut app = offline(Saved::default());
    new_project(&mut app, "Jira");
    fill(&mut app, "Site", "example.atlassian.net");
    fill(&mut app, "Email", "dana@example.com");
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_TOKEN");
    fill(&mut app, "Project", "ENG");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Err("Jira rejected the credentials (check the email and API token)"));
    assert_eq!(
        form_error(&app).as_deref(),
        Some(
            "Jira rejected the credentials (check the email and API token). Fix it, or press Enter again to save anyway."
        )
    );
    assert!(config_file().is_empty(), "saved before the second Enter");
    app.on_key(key(KeyCode::Enter));
    let t = toast(&app).unwrap();
    assert!(
        t.starts_with("Added project web · Saved the account without a working connection"),
        "{t}"
    );
    assert_eq!(saved_tracker().account.as_deref(), Some("jira-1"));
}

#[test]
fn a_key_none_of_the_listed_is_saved_on_a_second_enter() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    fill(&mut app, "Workspace", "acme");
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Ok(&ENG_OPS));
    fill(&mut app, "Team", "QA");
    assert_eq!(
        hint_of(&app, "Team"),
        "none of the teams this account can see; Enter saves QA as typed"
    );
    submit(&mut app);
    assert_eq!(
        form_error(&app).as_deref(),
        Some(
            "QA isn't one of the 2 teams this account can see. Pick one from the list, or press Enter again to save anyway."
        )
    );
    app.on_key(key(KeyCode::Enter));
    assert_eq!(saved_tracker().key, "QA");
}

#[test]
fn the_linear_token_page_follows_the_workspace_typed() {
    let page = |app: &App| settings_form(app).field("Read from").unwrap().links[0].2.clone();
    // In a project form, from its Workspace.
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    assert_eq!(page(&app), "https://linear.app/<workspace>/settings/account/security");
    fill(&mut app, "Workspace", "globex");
    assert_eq!(page(&app), "https://linear.app/globex/settings/account/security");
    app.on_key(key(KeyCode::Backspace));
    assert_eq!(page(&app), "https://linear.app/globe/settings/account/security");
    // In an account form, from its Site.
    let mut app = acme_app();
    prefix(&mut app, KeyCode::Char('A'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    assert_eq!(page(&app), "https://linear.app/<workspace>/settings/account/security");
    fill(&mut app, "Site", "globex");
    assert_eq!(page(&app), "https://linear.app/globex/settings/account/security");
}

#[test]
fn a_new_linear_account_is_the_workspaces_and_lists_its_teams() {
    let mut app = acme_app();
    new_project(&mut app, "Linear");
    fill(&mut app, "Workspace", "globex");
    assert_eq!(field(&app, "Account"), "+ new account");
    // No email for Linear, and a key of its own for a second workspace.
    assert_eq!(shown(&app), ["Path", "Tracker", "Workspace", "Account", "Read from", "Team"]);
    assert!(hint_of(&app, "Read from").starts_with("env:LINEAR_API_KEY reads"));
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY");
    app.on_key(key(KeyCode::Tab));
    answer_choices(&mut app, Ok(&[("WEB", "Website")]));
    // The only team is picked.
    assert_eq!(field(&app, "Team"), "WEB");
    assert_eq!(hint_of(&app, "Team"), "Website · ↑/↓ for another");
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "initech")));
    assert_eq!(
        form_error(&app).as_deref(),
        Some(
            "This key belongs to the workspace initech, not globex. Fix it, or press Enter again to save anyway."
        )
    );
    // Another key: the list was for the old one, so it goes, and the team typed stays.
    fill(&mut app, "Read from", "env:TASKMAN_TEST_UNSET_KEY_2");
    assert!(settings_form(&app).choices.is_none());
    app.on_key(key(KeyCode::Enter));
    answer_check(&mut app, Ok(("Dana Reyes", "globex")));
    assert_eq!(toast(&app), Some("Added project web · ✓ Connected to Linear globex as Dana Reyes"));
    let a = &store::load_config().unwrap().accounts[1];
    assert_eq!(
        (a.id.as_str(), a.site.as_str(), a.token.as_str()),
        ("linear-2", "globex", "env:TASKMAN_TEST_UNSET_KEY_2")
    );
    let t = saved_tracker();
    assert_eq!(
        (t.site.as_str(), t.key.as_str(), t.account.as_deref()),
        ("globex", "WEB", Some("linear-2"))
    );
}

#[test]
fn github_issues_and_local_files_suggest_a_prefix() {
    let mut app = offline(Saved::default());
    let dir = store::state_dir().join("web");
    std::fs::create_dir_all(&dir).unwrap();
    sh(&dir, "git init -q && git remote add origin git@github.com:acme/storefront.git");
    let board = crate::local::board(&dir, crate::local::FOLDER);
    crate::local::ensure_columns(&board).unwrap();
    std::fs::write(board.join("todo/OPS-3-fix-login.md"), "# Fix login\n").unwrap();
    prefix(&mut app, KeyCode::Char('P'));
    fill(&mut app, "Path", &dir.display().to_string());
    // GitHub Issues: from the repository's name; no account and nothing listed.
    choose(&mut app, "Tracker", "GitHub Issues");
    assert_eq!(field(&app, "Ticket prefix"), "STO");
    assert_eq!(shown(&app), ["Path", "Tracker", "Ticket prefix"]);
    // Local files: the prefix the board's tickets use already.
    choose(&mut app, "Tracker", "Local files");
    assert_eq!(field(&app, "Ticket prefix"), "OPS");
    // One typed stays.
    fill(&mut app, "Ticket prefix", "WP");
    choose(&mut app, "Tracker", "GitHub Issues");
    assert_eq!(field(&app, "Ticket prefix"), "WP");
}

#[test]
fn a_projects_settings_list_its_teams_and_change_the_prefix() {
    let mut app = acme_app();
    app.open_settings(super::SettingsItem::Project(0), false);
    assert_eq!(field(&app, "Team"), "ENG");
    assert!(settings_form(&app).choices.is_none(), "listed before editing");
    // Editing: its account can sign in, so the teams are listed at once.
    app.on_key(key(KeyCode::Enter));
    answer_choices(&mut app, Ok(&ENG_OPS));
    assert_eq!(hint_of(&app, "Team"), "Engineering");
    focus(&mut app, "Team");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    // Leaving the field takes the one highlighted.
    app.on_key(key(KeyCode::Tab));
    assert_eq!(field(&app, "Team"), "OPS");
    submit(&mut app);
    assert_eq!(toast(&app), Some("Saved acme"));
    let t = saved().projects[0].tracker.clone().unwrap();
    assert_eq!((t.key.as_str(), t.account.as_deref()), ("OPS", Some("linear-1")));
}
