//! The App driven as the event loop drives it: keys, mouse reports, pastes and the messages
//! threads send, on the mock project of `test_support::acme_app`, with sessions that run nothing
//! (`Session::fake`). Each test checks what the user would see or what reaches a session: the mode,
//! the selection, the bytes sent, the dialogs, the toasts and what was saved.

use super::sessions::HAND_OFF_WAIT;
use super::test_support::{
    acme_app, ago, answer_choices, attach, attach_crashed, attach_ended, attach_slow, browser,
    ctrl, draw, key, lay_out, long_sidebar, offline, panicking, reaped, sent, settle, settle_until,
    toast,
};
use super::{AgentState, App, Away, Hit, Mode, sync::Msg};
use crate::store::{self, Saved};
use crate::{add, resume};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

mod accounts;
mod add_form;
mod forms;
mod inbox;
mod input;
mod jump;
mod mouse;
mod projects;
mod queue;
mod reorder;
mod sessions;
mod settings;
mod sleep;
mod sync;
mod tickets;
mod toasts;

/// What a thread whose work panicked reports (`sync::Reporter`).
const BUG: &str = "taskman hit a bug; details in ~/.local/state/taskman/panics.log";

/// Whether the toasts so far include `msg`, as an error.
fn logged_error(app: &App, msg: &str) -> bool {
    app.toast_log.iter().any(|l| l.level == crate::app::Level::Error && l.msg == msg)
}

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

// ---- the sidebar ---------------------------------------------------------------------------

/// The sidebar's list sits under its title and leaves the last row blank.
fn list_rows(app: &App) -> std::ops::Range<u16> {
    let s = app.areas.sidebar;
    s.y + 2..s.bottom() - 1
}

// ---- sessions, their claims and saving -----------------------------------------------------

/// Holds ticket `key`'s claim as `taskman resume` does while it runs the session elsewhere.
fn held_outside(key: &str) -> resume::Hold {
    let mut hold = resume::take(&resume::owner_file("/src/acme".as_ref(), key)).unwrap().unwrap();
    hold.write("outside 4242");
    hold
}

fn owner(key: &str) -> resume::Owner {
    resume::peek(&resume::owner_file("/src/acme".as_ref(), key))
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

/// Puts a folder where tickets.json goes, so saving fails until it's removed; returns it.
fn block_saves() -> std::path::PathBuf {
    let file = store::state_dir().join("tickets.json");
    let _ = std::fs::remove_file(&file);
    std::fs::create_dir_all(file.join("in-the-way")).unwrap();
    file
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

/// Adds ticket ENG-5 to project web, a git repository of this test's with no tracker, and waits
/// for git to fail at making its worktree: a folder is in the way. Returns the ticket and the
/// folder; removed, the next try succeeds.
fn failed_worktree(app: &mut App) -> (usize, std::path::PathBuf) {
    let repo = store::state_dir().join("web");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, "git init -q -b main && git commit -q --allow-empty -m init");
    let web = serde_json::json!({ "name": "web", "repo": repo });
    app.projects.push(serde_json::from_value(web).unwrap());
    let in_the_way = crate::worktree::folder_path(crate::worktree::BESIDE, &repo, "ENG-5");
    std::fs::create_dir_all(&in_the_way).unwrap();
    let p = app.projects.len() - 1;
    let added = app.add_ticket(p, "ENG-5".into(), store::Cli::Claude, true, None, true);
    assert!(matches!(added, Ok(crate::app::ticket::Added::New(_))));
    settle(app);
    (app.sel, in_the_way)
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

// ---- forms ---------------------------------------------------------------------------------

/// The open form's field `label`.
fn field(app: &App, label: &str) -> String {
    match &app.mode {
        Mode::Form(f) => f.value(label),
        Mode::Settings(s) => s.form.value(label),
        _ => panic!("no form open"),
    }
}

fn focused(app: &App) -> &'static str {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.fields[f.focus].label
}

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

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
}

fn switch_to_ticket(app: &mut App) {
    focus(app, "Type");
    app.on_key(key(KeyCode::Left));
    assert_eq!(field(app, "Type"), "Ticket");
    focus(app, "Ticket");
}

/// The Add form as `ctrl+a n` opens it, on Type, switched to Ticket with the Ticket field focused.
fn open_ticket_form_on_ticket(app: &mut App, project: usize) {
    app.open_ticket_form(Some(project));
    switch_to_ticket(app);
}

/// The open issues listed under the Add form's Ticket field.
fn listed(app: &App) -> Vec<String> {
    let Mode::Form(f) = &app.mode else { panic!("no form open") };
    f.field("Ticket").unwrap().list.lines.clone()
}

/// The Settings form.
fn settings_form(app: &App) -> &crate::app::Form {
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

/// Enter, and wait for the check it starts, if any (a connection test, which sends the token it
/// read and then its verdict).
fn submit(app: &mut App) {
    app.on_key(key(KeyCode::Enter));
    while settings_form(app).checking.is_some() {
        settle(app);
    }
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
            app.tx.send(Msg::Checked(id, Ok(crate::app::forms::Check::Account(found)))).unwrap();
            app.drain_messages();
            return;
        }
        before.push(msg);
    }
}

/// A keychain for the App: the stub in this test's scratch folder (`keychain::stub`). Returns
/// the stub's folder, where it logs its calls.
fn with_keychain(app: &mut App) -> std::path::PathBuf {
    let dir = crate::keychain::stub();
    app.keychain = Some(crate::keychain::Keychain::os());
    dir
}

/// config.json as saved, or "" before the first save.
fn config_file() -> String {
    std::fs::read_to_string(store::config_path()).unwrap_or_default()
}

/// `ctrl+a P` on a folder of this test's, with `tracker` picked; returns the folder.
fn new_project(app: &mut App, tracker: &str) -> std::path::PathBuf {
    let dir = store::state_dir().join("web");
    std::fs::create_dir_all(&dir).unwrap();
    prefix(app, KeyCode::Char('P'));
    fill(app, "Path", &dir.display().to_string());
    choose(app, "Tracker", tracker);
    dir
}

const ENG_OPS: [(&str, &str); 2] = [("ENG", "Engineering"), ("OPS", "Operations")];
