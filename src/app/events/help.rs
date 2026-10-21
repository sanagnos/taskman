use super::*;
use crate::app::SettingsItem;
use crate::app::help::Machine;
use crate::app::test_support::browser;

/// Settings on Help & feedback, keys going to its links, the focus on `label`.
fn on_link(app: &mut App, label: &str) {
    app.open_settings(SettingsItem::General, false);
    // The last item, so ↑ from the first wraps round to it.
    app.on_key(key(KeyCode::Up));
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    assert_eq!(s.items[s.cursor], SettingsItem::Help);
    app.on_key(key(KeyCode::Enter));
    focus(app, label);
}

fn shown_link(app: &App, label: &str) -> Option<String> {
    settings_form(app).field(label).unwrap().links.first().map(|(_, _, url)| url.clone())
}

#[test]
fn the_bug_link_opens_the_bug_form_filled_in() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    let opened = browser(&mut app);
    on_link(&mut app, "Bug");
    app.on_key(key(KeyCode::Enter));
    let repo = env!("CARGO_PKG_REPOSITORY");
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        *opened.lock().unwrap(),
        [format!(
            "{repo}/issues/new?template=bug.yml&taskman=taskman%20{version}\
             &cli=Claude%20Code%202.1.3&os=Ubuntu%2024.04.1%20LTS\
             &terminal=Windows%20Terminal%2C%20160x40"
        )]
    );
    assert!(toast(&app).is_some_and(|t| t.starts_with("Opened ")), "{:?}", toast(&app));
    // Settings stays open, on the page.
    assert_eq!(focused(&app), "Bug");
    // The guide, the feature form and the advisory form, each its own link.
    for (label, path) in [
        ("Guide", "/blob/main/docs/guide.md"),
        ("Feature", "/issues/new?template=feature.yml"),
        ("Security problem", "/security/advisories/new"),
    ] {
        focus(&mut app, label);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(opened.lock().unwrap().last().unwrap(), &format!("{repo}{path}"));
    }
}

#[test]
fn c_copies_the_link() {
    let mut app = acme_app();
    let opened = browser(&mut app);
    on_link(&mut app, "Feature");
    app.on_key(key(KeyCode::Char('c')));
    settle(&mut app);
    let url = format!("{}/issues/new?template=feature.yml", env!("CARGO_PKG_REPOSITORY"));
    assert_eq!(decoded(&app.clipboard_out), std::slice::from_ref(&url));
    assert!(opened.lock().unwrap().is_empty());
    assert_eq!(shown_link(&app, "Feature"), Some(url));
    assert_eq!(settings_form(&app).error, None);
}

/// With no browser to open it, Enter copies the link and shows it.
#[test]
fn without_a_browser_the_link_is_copied_and_shown() {
    let mut app = acme_app();
    on_link(&mut app, "Guide");
    app.on_key(key(KeyCode::Enter));
    settle(&mut app);
    let url = format!("{}/blob/main/docs/guide.md", env!("CARGO_PKG_REPOSITORY"));
    assert_eq!(decoded(&app.clipboard_out), std::slice::from_ref(&url));
    assert_eq!(shown_link(&app, "Guide"), Some(url.clone()));
    let error = settings_form(&app).error.clone().unwrap();
    assert!(error.starts_with("No browser found"), "{error}");
    let screen = draw(&mut app, 160, 40);
    assert!(screen.contains("No browser found"), "{screen}");
    assert!(screen.contains(env!("CARGO_PKG_REPOSITORY")), "{screen}");
}

/// Over SSH a browser would open on the other machine: the link is copied instead.
#[test]
fn over_ssh_the_link_is_copied_rather_than_opened() {
    let mut app = acme_app();
    app.machine.as_mut().unwrap().ssh = true;
    let opened = browser(&mut app);
    on_link(&mut app, "Security problem");
    app.on_key(key(KeyCode::Enter));
    settle(&mut app);
    assert!(opened.lock().unwrap().is_empty());
    let url = format!("{}/security/advisories/new", env!("CARGO_PKG_REPOSITORY"));
    assert_eq!(decoded(&app.clipboard_out), [url]);
    assert!(settings_form(&app).error.clone().unwrap().starts_with("Over SSH"));
}

/// Until the lookup answers, the bug link has taskman's version; then the rest, the focus kept.
#[test]
fn the_bug_link_fills_in_what_the_lookup_found() {
    let mut app = acme_app();
    (app.machine, app.machine_asked) = (None, true);
    on_link(&mut app, "Bug");
    let hint = |app: &App| settings_form(app).field("Bug").unwrap().hint.clone();
    let version = env!("CARGO_PKG_VERSION");
    assert!(hint(&app).starts_with(&format!("fills in taskman {version}; looking up")));
    let machine = Machine {
        cli: "Codex 0.50.0".into(),
        os: "macOS 15.5 (Apple Silicon)".into(),
        terminal: Some("iTerm2 3.5.0".into()),
        inside: Some("tmux 3.4".into()),
        ssh: false,
    };
    app.tx.send(Msg::Machine(machine)).unwrap();
    app.drain_messages();
    assert_eq!(focused(&app), "Bug");
    assert_eq!(
        hint(&app),
        format!(
            "fills in taskman {version}, Codex 0.50.0, macOS 15.5 (Apple Silicon) and the terminal: \
             iTerm2 3.5.0, inside tmux 3.4"
        )
    );
}
