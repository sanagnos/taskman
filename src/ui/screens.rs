//! Snapshots of the screens users see, drawn from a mock project (`test_support::acme_app`) into a
//! `TestBackend`: what each shows is in `snapshots/`, one file each, reviewed like code. After a
//! change to drawing, `cargo insta review` (or `INSTA_UPDATE=always cargo test`) updates them.
//!
//! Most are 80 by 24, where the sidebar folds and the panels hide; the panels and the agents' board
//! need a wider screen, 160 by 40.

use crate::app::test_support::{
    acme_app, attach, attach_crashed, attach_ended, draw, draw_colours, key, long_sidebar, offline,
};
use crate::app::{AgentAsk, AgentState, Mode, SettingsItem};
use crate::store::Saved;
use crate::worktree::RemoveCheck;
use crossterm::event::KeyCode;
use insta::assert_snapshot;
use std::time::Instant;

const SESSION: &str = "\x1b[1m✻ Welcome to Claude Code\x1b[0m\n\n> Add the magic link sign-in\n\n\
● I'll start with the token table.\n\n  ⎿  Wrote src/auth/link.rs\n\n> ";

fn select(app: &mut crate::app::App, key: &str) {
    let i = app.tickets.iter().position(|tv| tv.t.key == key).unwrap();
    app.select(i);
}

#[test]
fn welcome() {
    assert_snapshot!(draw(&mut offline(Saved::default()), 80, 24));
}

#[test]
fn welcome_with_a_project() {
    let saved = Saved { tickets: vec![], ..crate::app::test_support::acme() };
    assert_snapshot!(draw(&mut offline(saved), 80, 24));
}

#[test]
fn ticket_with_its_panels() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    attach(&mut app, "ENG-52", SESSION);
    assert_snapshot!(draw(&mut app, 160, 40));
}

#[test]
fn ticket_on_a_small_screen() {
    let mut app = acme_app();
    draw(&mut app, 80, 24);
    attach(&mut app, "ENG-52", SESSION);
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// On a white terminal: a light selection and footer, hints grey enough to read on white, and the
/// toast amber rather than yellow. Its colours follow the screen.
#[test]
fn ticket_on_a_light_terminal() {
    use crate::theme::{Probe, Setting, Theme};
    let mut app = acme_app();
    let probe = Probe { fg: Some([0; 3]), bg: Some([255; 3]), colorfgbg: None };
    app.theme = Theme::new(Setting::Auto, &probe);
    draw(&mut app, 100, 30);
    attach(&mut app, "ENG-52", SESSION);
    app.toast("Copied 12 characters");
    assert_snapshot!(draw_colours(&mut app, 100, 30));
}

/// Its review threads too, as the tracker panel is off.
#[test]
fn pr_panel() {
    let mut app = acme_app();
    app.prefs.show_jira = false;
    draw(&mut app, 160, 40);
    attach(&mut app, "ENG-52", SESSION);
    assert_snapshot!(draw(&mut app, 160, 40));
}

/// No PR yet, and the last look for one failed: the label is red, and the panel says why.
#[test]
fn no_pr_after_a_failed_refresh() {
    let mut app = acme_app();
    select(&mut app, "ENG-70");
    let tv = app.tickets.iter_mut().find(|tv| tv.t.key == "ENG-70").unwrap();
    tv.pr.error = Some("gh: error connecting to api.github.com".into());
    tv.pr.fetched_at = crate::app::test_support::ago(30.0);
    assert_snapshot!(draw(&mut app, 160, 40));
}

#[test]
fn agents_board() {
    let mut app = acme_app();
    select(&mut app, "acme-lead");
    draw(&mut app, 160, 40);
    attach(
        &mut app,
        "acme-lead",
        "\x1b[1m✻ Welcome to Claude Code\x1b[0m\n\n> Which tickets wait on review?\n\n> ",
    );
    assert_snapshot!(draw(&mut app, 160, 40));
}

/// A dismissed review reads as one on the agents' board, as in the PR panel, not as a comment.
#[test]
fn a_dismissed_review_on_the_board() {
    use crate::github::ReviewState;
    let mut app = acme_app();
    for tv in &mut app.tickets {
        let Some(Some(pr)) = &mut tv.pr.data else { continue };
        for r in pr.reviews.iter_mut().filter(|r| r.state == ReviewState::ChangesRequested) {
            r.state = ReviewState::Dismissed;
        }
    }
    select(&mut app, "acme-lead");
    let screen = draw(&mut app, 160, 40);
    assert!(screen.contains("– sam-okafor dismissed"), "{screen}");
    assert!(!screen.contains("sam-okafor commented"), "{screen}");
}

#[test]
fn ended_session() {
    let mut app = acme_app();
    select(&mut app, "ENG-70");
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// What a CLI printed before it exited, with why it ended over the pane, under the output.
const EXITED: &str = "\x1b[1m✻ Welcome to Claude Code\x1b[0m\n\n> Add the magic link sign-in\n\n\
\x1b[31mAPI Error: 401 OAuth token has expired. Please run /login.\x1b[0m\n";

fn exited(width: u16) -> String {
    let mut app = acme_app();
    select(&mut app, "ENG-70");
    draw(&mut app, width, 24);
    attach_ended(&mut app, "ENG-70", EXITED, 1);
    draw(&mut app, width, 24)
}

#[test]
fn exited_session_with_its_output() {
    assert_snapshot!(exited(80));
}

#[test]
fn exited_session_at_40_columns() {
    assert_snapshot!(exited(40));
}

/// Its CLI isn't installed: the pane says what to do, in the words a real failed start gives.
fn start_failed(width: u16) -> String {
    let mut app = acme_app();
    select(&mut app, "ENG-70");
    let cmd = portable_pty::CommandBuilder::new("/opt/acme/bin/claude");
    let e = crate::session::Session::spawn(
        cmd.clone(),
        "ENG-70",
        24,
        80,
        crate::store::SCROLLBACK,
        Default::default(),
        Default::default(),
    )
    .err()
    .unwrap();
    let why = crate::session::start_error(&cmd, &e, "Claude Code", "TASKMAN_CLAUDE");
    let tv = app.tickets.iter_mut().find(|tv| tv.t.key == "ENG-70").unwrap();
    tv.end = Some(crate::session::End::StartFailed(why));
    draw(&mut app, width, 24)
}

#[test]
fn session_that_did_not_start() {
    assert_snapshot!(start_failed(80));
}

#[test]
fn session_that_did_not_start_at_40_columns() {
    assert_snapshot!(start_failed(40));
}

/// Its reader panicked part-way through the output: the screen as it was left, and the crash.
fn crashed(width: u16) -> String {
    let mut app = acme_app();
    select(&mut app, "ENG-70");
    draw(&mut app, width, 24);
    attach_crashed(&mut app, "ENG-70", SESSION);
    draw(&mut app, width, 24)
}

#[test]
fn session_whose_reader_crashed() {
    assert_snapshot!(crashed(80));
}

#[test]
fn session_whose_reader_crashed_at_40_columns() {
    assert_snapshot!(crashed(40));
}

/// A resume refused because Claude runs the conversation in the background.
#[test]
fn session_running_in_claudes_background() {
    let mut app = acme_app();
    select(&mut app, "ENG-70");
    draw(&mut app, 80, 24);
    let said = "That session is running in the background (a1b2c3d4). Run `claude attach a1b2c3d4` to open it, or `claude stop a1b2c3d4` first to resume it here.";
    attach_ended(&mut app, "ENG-70", said, 1);
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// Its owner lock couldn't be taken, so the session didn't start.
#[test]
fn session_without_its_lock() {
    let mut app = acme_app();
    select(&mut app, "ENG-70");
    let tv = app.tickets.iter_mut().find(|tv| tv.t.key == "ENG-70").unwrap();
    tv.lock_error = Some(
        "~/.local/state/taskman/status/acme-0123abcd/ENG-70.owner: Permission denied (os error 13)"
            .into(),
    );
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// A sidebar longer than the screen, from the top: the last row counts the cards below, with ●
/// as one of them needs you.
#[test]
fn long_sidebar_from_the_top() {
    assert_snapshot!(draw(&mut long_sidebar(20), 160, 40));
}

/// Scrolled to its last card: the top row counts the cards above, and the folded project below is
/// the one entry hidden.
#[test]
fn long_sidebar_at_its_last_card() {
    let mut app = long_sidebar(20);
    draw(&mut app, 160, 40);
    select(&mut app, "ENG-90");
    assert_snapshot!(draw(&mut app, 160, 40));
}

#[test]
fn folded_long_sidebar_from_the_top() {
    assert_snapshot!(draw(&mut long_sidebar(20), 80, 24));
}

#[test]
fn folded_long_sidebar_at_its_last_card() {
    let mut app = long_sidebar(20);
    draw(&mut app, 80, 24);
    select(&mut app, "ENG-90");
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// Move mode: the card steps down one place, and the footer says how to go on.
#[test]
fn moving_a_card_with_the_keys() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(key(KeyCode::Char('}')));
    assert_snapshot!(draw(&mut app, 160, 40));
}

/// Moving a project: every project is folded, the one that moves marked.
#[test]
fn moving_a_project_with_the_keys() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(key(KeyCode::Char('}')));
    app.on_key(key(KeyCode::Char('p')));
    assert_snapshot!(draw(&mut app, 160, 40));
}

/// A card dragged: dimmed where it stands, the landing line under ENG-61, the footer's words.
#[test]
fn dragging_a_card() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    let rows = |app: &crate::app::App, key: &str| {
        let i = app.tickets.iter().position(|tv| tv.t.key == key).unwrap();
        app.sidebar_hits.iter().find(|(_, h)| *h == crate::app::Hit::Ticket(i)).unwrap().0
    };
    let (from, to) = (rows(&app, "ENG-52"), rows(&app, "ENG-61"));
    let mouse = |app: &mut crate::app::App, kind, y| {
        app.on_mouse(MouseEvent { kind, column: 3, row: y, modifiers: KeyModifiers::NONE });
        draw(app, 160, 40)
    };
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), from.y);
    let screen = mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), to.bottom() - 1);
    assert_snapshot!(screen);
}

#[test]
fn prefix_menu() {
    let mut app = acme_app();
    app.on_key(crate::app::test_support::ctrl('a'));
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// Help whole, tall enough not to scroll: every key and the whole legend, its lines wrapped
/// rather than cut at 80 columns.
#[test]
fn help() {
    let mut app = acme_app();
    app.mode = Mode::Help(Default::default());
    let screen = draw(&mut app, 80, 60);
    assert!(screen.contains("Press any key to close."), "{screen}");
    assert_snapshot!(screen);
}

/// Wide enough, the legend is two to a row.
#[test]
fn help_on_a_wide_screen() {
    let mut app = acme_app();
    app.mode = Mode::Help(Default::default());
    let screen = draw(&mut app, 120, 50);
    assert!(screen.contains("Press any key to close."), "{screen}");
    assert_snapshot!(screen);
}

/// On a short screen Help scrolls; End shows its last rows, the legend's end among them.
#[test]
fn help_scrolled_to_its_end() {
    let mut app = acme_app();
    app.mode = Mode::Help(Default::default());
    draw(&mut app, 80, 24);
    app.on_key(key(KeyCode::End));
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// Narrower than its text, Help wraps each key's words under its own column and puts the legend
/// one to a row.
#[test]
fn help_at_60_columns() {
    let mut app = acme_app();
    app.mode = Mode::Help(Default::default());
    assert_snapshot!(draw(&mut app, 60, 24));
}

#[test]
fn quit_dialog() {
    let mut app = acme_app();
    app.mode = Mode::ConfirmQuit;
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// A closed ticket's worktree is still being removed: quitting waits for it, and says so.
#[test]
fn quit_dialog_waiting_for_a_worktree_removal() {
    let mut app = acme_app();
    let _removal = crate::app::test_support::removing(&mut app, &["ENG-48"]);
    app.mode = Mode::ConfirmQuit;
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// `prefix R` on a session that's working asks first, saying what restarting cuts short.
#[test]
fn restart_dialog_while_working() {
    let mut app = acme_app();
    let i = app.sel;
    app.tickets[i].status.state = AgentState::Working;
    app.mode = Mode::ConfirmRestart(("/src/acme".into(), "ENG-52".into()));
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// ENG-53 needs the user: restarting leaves what it asked unanswered.
#[test]
fn restart_dialog_while_it_needs_you() {
    let mut app = acme_app();
    app.mode = Mode::ConfirmRestart(("/src/acme".into(), "ENG-53".into()));
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// acme has tickets and agents both, counted apart.
#[test]
fn delete_project_dialog() {
    let mut app = acme_app();
    app.mode = Mode::ConfirmDeleteProject(0);
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// Inside tmux, a hand-off offers a new window with the command that resumes the session.
#[test]
fn tmux_offer() {
    let mut app = acme_app();
    app.mode = Mode::OfferTmux(("/src/acme".into(), "ENG-52".into()));
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// `prefix l`: the ticket's links from its panels, then the session's, newest first; a link
/// that doesn't open in a browser is dimmed and says it can be copied.
#[test]
fn links_list() {
    let mut app = acme_app();
    draw(&mut app, 100, 40);
    let output = "> Deploy the preview\n\n● Deployed to https://preview.example.com/eng-52\n\n  \
                  Logs: \x1b]8;;file:///src/acme-ENG-52/deploy.log\x1b\\deploy.log\x1b]8;;\x1b\\\n\n  \
                  See \x1b]8;;https://docs.example.com/deploys\x1b\\the deploy guide\x1b]8;;\x1b\\\n";
    attach(&mut app, "ENG-52", output);
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(key(KeyCode::Char('l')));
    assert!(matches!(app.mode, Mode::Links(_)));
    assert_snapshot!(draw(&mut app, 100, 40));
}

#[test]
fn close_dialog_offering_to_remove_the_worktree() {
    let mut app = acme_app();
    app.close_check = Some(RemoveCheck {
        path: "/src/acme-ENG-52".into(),
        problems: Vec::new(),
        branch: Some("eng-52-login".into()),
        head: "0123456".into(),
        ignored: Vec::new(),
    });
    app.mode = Mode::ConfirmClose;
    assert_snapshot!(draw(&mut app, 80, 24));
}

#[test]
fn close_dialog_offering_to_delete_ignored_files() {
    let mut app = acme_app();
    app.close_check = Some(RemoveCheck {
        path: "/src/acme-ENG-52".into(),
        problems: Vec::new(),
        branch: Some("eng-52-login".into()),
        head: "0123456".into(),
        ignored: [".env", "data.db", "notes/", "uploads/"].map(String::from).to_vec(),
    });
    app.mode = Mode::ConfirmClose;
    assert_snapshot!(draw(&mut app, 80, 24));
}

#[test]
fn close_dialog_keeping_a_worktree_it_could_not_check() {
    let mut app = acme_app();
    app.close_unchecked = Some(
        "git worktree failed: not a git repository (or any of the parent directories): .git".into(),
    );
    app.mode = Mode::ConfirmClose;
    assert_snapshot!(draw(&mut app, 80, 24));
}

#[test]
fn close_dialog_keeping_the_worktree() {
    let mut app = acme_app();
    app.close_check = Some(RemoveCheck {
        path: "/src/acme-ENG-52".into(),
        problems: vec!["2 uncommitted files".into(), "1 commit not pushed".into()],
        branch: None,
        head: "0123456".into(),
        ignored: vec![".env".into()],
    });
    app.mode = Mode::ConfirmClose;
    assert_snapshot!(draw(&mut app, 80, 24));
}

#[test]
fn agent_approval() {
    let mut app = acme_app();
    let req = serde_json::from_value(serde_json::json!({
        "repo": "/src/acme", "from": "✦ lead", "reply_by": 0,
        "items": [{ "kind": "agent", "name": "tester", "role": "Run the test suite on each PR and report failures" }],
    }))
    .unwrap();
    app.mode = Mode::ConfirmAgent(Box::new(AgentAsk { req, open: false, shown: Instant::now() }));
    assert_snapshot!(draw(&mut app, 80, 24));
}

#[test]
fn settings() {
    let mut app = acme_app();
    app.open_settings(SettingsItem::Project(0), false);
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// Settings with more projects than fit, on the last: the list scrolls to show it, and counts
/// the items above and below.
#[test]
fn settings_scrolled_to_the_last_project() {
    let mut app = crate::app::test_support::many_projects(30);
    app.open_settings(SettingsItem::Project(29), false);
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// Settings → General, with the defaults: sleep after an hour, no limit, 10,000 rows of scrollback.
#[test]
fn settings_general() {
    let mut app = acme_app();
    app.open_settings(SettingsItem::General, false);
    assert_snapshot!(draw(&mut app, 100, 34));
}

/// Sleeping sessions are a quiet glyph; one that finished a turn nobody has looked at keeps
/// saying so.
#[test]
fn sleeping_sessions_in_the_sidebar() {
    use crate::app::AgentState::{Done, Idle};
    let mut app = acme_app();
    for (key, state, seen) in
        [("ENG-53", Idle, true), ("ENG-61", Done, false), ("ENG-70", Done, true)]
    {
        let tv = app.tickets.iter_mut().find(|tv| tv.t.key == key).unwrap();
        tv.session = None;
        (tv.status.state, tv.status.seen, tv.asleep) = (state, seen, true);
    }
    assert_snapshot!(draw(&mut app, 160, 40));
}

#[test]
fn add_form() {
    let mut app = acme_app();
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(key(KeyCode::Char('n')));
    assert_snapshot!(draw(&mut app, 80, 24));
}

// ---- first run: a new project with Jira, signed in, to its Jira project ---------------------

/// A first run's app: no projects, and a keychain if `keychain`.
fn first_run(keychain: bool) -> crate::app::App {
    let mut app = offline(Saved::default());
    if keychain {
        app.keychain = Some(crate::keychain::macos_stub());
    }
    app
}

/// Types `text` over the value of the open form's field `label`.
fn type_into(app: &mut crate::app::App, label: &str, text: &str) {
    for _ in 0..20 {
        let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
        if s.form.fields[s.form.focus].label == label {
            app.on_key(crate::app::test_support::ctrl('u'));
            text.chars().for_each(|c| app.on_key(key(KeyCode::Char(c))));
            return;
        }
        app.on_key(key(KeyCode::Tab));
    }
    panic!("no field {label} shown");
}

/// `ctrl+a P`, a folder, Jira, its site and the new account's email.
fn new_jira_project(app: &mut crate::app::App) {
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(key(KeyCode::Char('P')));
    // A folder that's there on every machine, so the project can be saved, and isn't a link to
    // another: the project is drawn where its path leads, and macOS's /tmp leads to /private/tmp.
    type_into(app, "Path", "/usr");
    type_into(app, "Tracker", "");
    app.on_key(key(KeyCode::Right));
    type_into(app, "Site", "acme.atlassian.net");
    type_into(app, "Email", "dana@example.com");
}

/// The projects a mock account sees, more than the list shows at once.
const PROJECTS: [(&str, &str); 7] = [
    ("DATA", "Data platform"),
    ("ENG", "Engineering"),
    ("MOB", "Mobile apps"),
    ("OPS", "Operations"),
    ("QA", "Quality"),
    ("SEC", "Security"),
    ("WEB", "Website"),
];

/// Jira picked: its site, then a new account's fields in the same form, the token to paste last.
#[test]
fn first_run_1_new_project_with_jira() {
    let mut app = first_run(true);
    new_jira_project(&mut app);
    assert_snapshot!(draw(&mut app, 120, 40));
}

/// At 80 columns the token page's URL wraps, whole, on lines of its own: never cut with `…`.
#[test]
fn first_run_1_at_80_columns() {
    let mut app = first_run(true);
    new_jira_project(&mut app);
    let screen = draw(&mut app, 80, 40);
    let lines: Vec<&str> = screen.lines().collect();
    let first = lines.iter().position(|l| l.contains("https://")).expect("no URL drawn");
    let url: String =
        lines[first..first + 2].iter().map(|l| l.rsplit('│').nth(1).unwrap().trim()).collect();
    assert_eq!(url, "https://id.atlassian.com/manage-profile/security/api-tokens");
    assert!(lines[first - 1].contains("Get a token:"), "{screen}");
    assert_snapshot!(screen);
}

/// The token pasted and Tab: taskman signs in and lists the Jira projects.
#[test]
fn first_run_2_listing_the_projects() {
    let mut app = first_run(true);
    new_jira_project(&mut app);
    type_into(&mut app, "Token", "ATATT-mock-token");
    app.on_key(key(KeyCode::Tab));
    assert_snapshot!(draw(&mut app, 120, 40));
}

/// Where there's no keychain (WSL, a server): the token goes in a file only the user can read.
#[test]
fn first_run_2_without_a_keychain() {
    let mut app = first_run(false);
    new_jira_project(&mut app);
    app.on_key(key(KeyCode::Tab));
    assert_snapshot!(draw(&mut app, 120, 40));
}

/// The projects listed: ↓ picks one; typing narrows them.
#[test]
fn first_run_3_projects_listed() {
    let mut app = first_run(true);
    new_jira_project(&mut app);
    type_into(&mut app, "Token", "ATATT-mock-token");
    app.on_key(key(KeyCode::Tab));
    crate::app::test_support::answer_choices(&mut app, Ok(&PROJECTS));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_snapshot!(draw(&mut app, 120, 40));
}

/// The list couldn't load: why, and the key can still be typed.
#[test]
fn first_run_3_projects_not_listed() {
    let mut app = first_run(true);
    new_jira_project(&mut app);
    type_into(&mut app, "Token", "ATATT-mock-token");
    app.on_key(key(KeyCode::Tab));
    let e = "Jira rejected the credentials (check the email and API token)";
    crate::app::test_support::answer_choices(&mut app, Err(e));
    assert_snapshot!(draw(&mut app, 120, 40));
}

/// A variable that isn't set: what to add where, and the keychain instead.
#[test]
fn first_run_4_a_variable_that_is_not_set() {
    let mut app = first_run(true);
    new_jira_project(&mut app);
    type_into(&mut app, "Keep in", "");
    app.on_key(key(KeyCode::Right));
    type_into(&mut app, "Project", "");
    // Listed: the sign-in is read when focus leaves it, and the account is checked on Enter.
    crate::app::test_support::answer_choices(&mut app, Ok(&PROJECTS));
    type_into(&mut app, "Project", "ENG");
    app.on_key(key(KeyCode::Enter));
    let e = "JIRA_API_TOKEN isn't set in the shell taskman started from: add export JIRA_API_TOKEN=… to ~/.zshrc, then open a new terminal and start taskman there";
    crate::app::test_support::answer_check(&mut app, Err(e.into()));
    assert_snapshot!(draw(&mut app, 120, 40));
}

/// A project picked and the connection works: the account and project are saved, and the Add form
/// opens on it.
#[test]
fn first_run_5_connected() {
    let mut app = first_run(true);
    new_jira_project(&mut app);
    type_into(&mut app, "Token", "ATATT-mock-token");
    app.on_key(key(KeyCode::Tab));
    crate::app::test_support::answer_choices(&mut app, Ok(&PROJECTS));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    crate::app::test_support::answer_check(&mut app, Ok(("Dana Reyes", "")));
    assert_eq!(app.config.accounts[0].token, "keychain:jira-1");
    assert_eq!(app.projects[0].tracker.as_ref().map(|t| t.key.as_str()), Some("ENG"));
    assert_snapshot!(draw(&mut app, 120, 40));
}

/// Deleting an account whose token is in the keychain offers to delete that too.
#[test]
fn delete_account_with_its_keychain_item() {
    let mut app = acme_app();
    app.keychain = Some(crate::keychain::macos_stub());
    let account = serde_json::json!({ "id": "jira-1", "provider": "jira", "email": "dana@example.com",
                                      "token": "keychain:jira-1" });
    app.config.accounts.push(serde_json::from_value(account).unwrap());
    app.mode = Mode::ConfirmDeleteAccount(1);
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// Project acme on GitHub Issues (acme/web, prefix ENG).
fn on_github(app: &mut crate::app::App) {
    let t = serde_json::json!({ "provider": "github", "site": "acme/web", "key": "ENG" });
    app.projects[0].tracker = Some(serde_json::from_value(t).unwrap());
}

/// A GitHub issue: its labels, assignees and the PRs that close it.
#[test]
fn github_issue_panel() {
    let mut app = acme_app();
    on_github(&mut app);
    app.prefs.show_gh = false;
    let tv = app.tickets.iter_mut().find(|tv| tv.t.key == "ENG-52").unwrap();
    let issue = tv.jira.data.as_mut().unwrap();
    issue.summary = "Login fails with SSO".into();
    (issue.status, issue.status_category) = ("Open".into(), "new".into());
    (issue.issue_type, issue.priority) = ("bug, auth".into(), String::new());
    issue.assignee = "ana-example, ben-example".into();
    issue.prs = vec![
        crate::tracker::LinkedPr {
            number: 45,
            state: crate::github::PrState::Open,
            url: "https://github.com/acme/web/pull/45".into(),
        },
        crate::tracker::LinkedPr {
            number: 40,
            state: crate::github::PrState::Closed,
            url: "https://github.com/acme/web/pull/40".into(),
        },
    ];
    draw(&mut app, 160, 40);
    attach(&mut app, "ENG-52", SESSION);
    assert_snapshot!(draw(&mut app, 160, 40));
}

/// The open issues, assigned to you first, under the Ticket field.
#[test]
fn add_form_listing_open_issues() {
    let mut app = acme_app();
    on_github(&mut app);
    let issue = |number, title: &str, mine| crate::tracker::Listed {
        key: format!("ENG-{number}"),
        title: title.into(),
        mine,
    };
    let list = vec![
        issue(9, "Crash on start", true),
        issue(15, "Fix the login form so that a long title is cut to fit the form's width", false),
        issue(14, "Fix the search page", false),
        issue(12, "Login fails with SSO", false),
        issue(3, "Docs: explain setup", false),
        issue(2, "Slow export", false),
    ];
    let listing = app.start_listing(0).unwrap();
    app.open_issues_listed(listing, Ok(list));
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(key(KeyCode::Char('n')));
    // Opens on Type, as an Agent; the list is under the Ticket field.
    app.on_key(key(KeyCode::Left));
    for _ in 0..4 {
        app.on_key(key(KeyCode::Tab));
    }
    assert_snapshot!(draw(&mut app, 80, 30));
}

/// A Linear project's open issues, ↓ pressed past the fifth: the window scrolled to the
/// highlight, with how many more are above and below it.
#[test]
fn add_form_picking_an_open_issue() {
    let mut app = acme_app();
    let titles = ["Fix the login page", "Export to CSV", "Slow search", "Crash on start"];
    let list = (1..=9)
        .map(|n: usize| crate::tracker::Listed {
            key: format!("ENG-{}", 60 + n),
            title: titles[n % titles.len()].into(),
            mine: n == 1,
        })
        .collect();
    let listing = app.start_listing(0).unwrap();
    app.open_issues_listed(listing, Ok(list));
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(key(KeyCode::Char('n')));
    // Opens on Type, as an Agent; the list is under the Ticket field.
    app.on_key(key(KeyCode::Left));
    for _ in 0..4 {
        app.on_key(key(KeyCode::Tab));
    }
    for _ in 0..6 {
        app.on_key(key(KeyCode::Down));
    }
    assert_snapshot!(draw(&mut app, 80, 30));
}

/// The terminal can shrink between the tick that laid the screen out and the draw: the frame's
/// buffer is the new size, and the session's pane and its links are written into it directly, so
/// the frame is laid out again from its own size.
#[test]
fn a_frame_smaller_than_the_layout_does_not_panic() {
    use crate::app::test_support::lay_out;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut app = acme_app();
    lay_out(&mut app, 160, 40);
    attach(&mut app, "ENG-52", SESSION);
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let claude = app.areas.claude;
    assert!(claude.right() <= 80 && claude.bottom() <= 24, "{claude:?}");
    // The session now fits its pane.
    let s = app.tickets.iter().find(|tv| tv.t.key == "ENG-52").unwrap().session.as_ref().unwrap();
    let size = crate::term::size(crate::session::lock(&s.parser).screen());
    assert_eq!(size, (claude.height, claude.width));
}

#[test]
fn jump_box_narrowing_the_sidebar() {
    let mut app = acme_app();
    draw(&mut app, 120, 30);
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(crate::app::test_support::key(crossterm::event::KeyCode::Char('/')));
    for c in "sign".chars() {
        app.on_key(crate::app::test_support::key(crossterm::event::KeyCode::Char(c)));
    }
    app.on_key(crate::app::test_support::key(crossterm::event::KeyCode::Down));
    assert_snapshot!(draw(&mut app, 120, 30));
}

#[test]
fn jump_box_without_a_match() {
    let mut app = acme_app();
    draw(&mut app, 120, 30);
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(crate::app::test_support::key(crossterm::event::KeyCode::Char('/')));
    for c in "zzz".chars() {
        app.on_key(crate::app::test_support::key(crossterm::event::KeyCode::Char(c)));
    }
    assert_snapshot!(draw(&mut app, 120, 30));
}

/// ENG-53 and notes-writer wait, the second for longer: mid-bucket ages, so they draw the same on
/// any day.
fn waiting_app() -> crate::app::App {
    let mut app = acme_app();
    for (key, secs, input) in [
        ("ENG-53", 270, Some("cargo test --all-features 2>&1 | tail -n 40")),
        ("notes-writer", 2 * 3600 + 600, None),
    ] {
        let tv = app.tickets.iter_mut().find(|tv| tv.t.key == key).unwrap();
        tv.status.waiting_since = Some(crate::util::now() - secs);
        tv.status.permission = input.is_some();
        tv.status.tool = input.map(|_| "Bash".into());
        tv.status.input = input.map(str::to_string);
        tv.status.message = "Claude has a question about the release notes".into();
    }
    app
}

#[test]
fn waiting_queue_oldest_first() {
    let mut app = waiting_app();
    draw(&mut app, 120, 30);
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(crate::app::test_support::key(crossterm::event::KeyCode::Char('w')));
    app.on_key(crate::app::test_support::key(crossterm::event::KeyCode::Down));
    assert_snapshot!(draw(&mut app, 120, 30));
}

#[test]
fn waiting_queue_on_a_narrow_screen() {
    let mut app = waiting_app();
    draw(&mut app, 60, 24);
    app.on_key(crate::app::test_support::ctrl('a'));
    app.on_key(crate::app::test_support::key(crossterm::event::KeyCode::Char('w')));
    assert_snapshot!(draw(&mut app, 60, 24));
}

#[test]
fn sidebar_badge_with_the_waiting_age() {
    let mut app = waiting_app();
    assert_snapshot!(draw(&mut app, 120, 30));
}

#[test]
fn sidebar_badge_with_the_waiting_age_in_the_narrowest_sidebar() {
    let mut app = waiting_app();
    assert_snapshot!(draw(&mut app, 64, 24));
}

/// An error toast is red and keeps the waiting count beside it.
#[test]
fn error_toast_with_the_waiting_count() {
    let mut app = acme_app();
    app.error("Couldn't write ~/.cache/taskman/ENG-52/history.txt: disk full");
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("waiting on you"), "{screen}");
    assert_snapshot!(screen);
}

/// A long error wraps onto the blank row above the footer, and the count stays on the last row.
#[test]
fn long_error_toast_wraps_above_the_footer() {
    let mut app = acme_app();
    app.error(LONG_ERROR);
    let screen = draw(&mut app, 80, 40);
    assert!(screen.contains("waiting on you"), "{screen}");
    assert_snapshot!(screen);
}

/// Where there is no blank row to wrap onto, the message is cut and says where to read it whole.
#[test]
fn long_error_toast_on_a_short_screen_points_to_the_list() {
    let mut app = acme_app();
    app.error(LONG_ERROR);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains(" ~ lists it"), "{screen}");
    assert_snapshot!(screen);
}

const LONG_ERROR: &str = "Couldn't create ENG-70's worktree: fatal: a branch named \
    'eng-70-fix-login' already exists, and git refused to check it out in /src/acme-ENG-70 \
    because another worktree has it checked out";

/// The notices: every toast shown (`ctrl+a ~`), newest first, errors red.
#[test]
fn notices_list() {
    let mut app = acme_app();
    app.toast("Marked as read");
    app.warn("No PR found for this ticket yet");
    app.error(LONG_ERROR);
    app.mode = Mode::Log(Default::default());
    assert_snapshot!(draw(&mut app, 80, 24));
}

/// Too many to show on a narrow screen: the keys wrap between hints, and the list gives up rows
/// to them rather than run under the border.
#[test]
fn notices_scrolling_on_a_narrow_screen() {
    let mut app = acme_app();
    for n in 1..=12 {
        app.toast(format!("Refreshed ENG-{n}"));
    }
    app.mode = Mode::Log(Default::default());
    let screen = draw(&mut app, 40, 16);
    assert!(screen.contains("other keys close"), "{screen}");
    assert_snapshot!(screen);
}
