use super::*;
use crate::app::SettingsItem;
use crate::app::test_support::many_projects;

fn cursor(app: &App) -> SettingsItem {
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    s.items[s.cursor]
}

#[test]
fn the_settings_list_wraps_around_and_keeps_the_cursor_in_view() {
    let mut app = many_projects(30);
    app.open_settings(SettingsItem::General, false);
    assert!(draw(&mut app, 80, 24).contains("Prefix and updates"));
    // ↑ from the first item goes to the last, scrolled into view.
    app.on_key(key(KeyCode::Up));
    assert_eq!(cursor(&app), SettingsItem::Help);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("▸ Help & feedback"), "{screen}");
    assert!(!screen.contains("Prefix and updates"), "{screen}");
    // ↓ from the last goes back to the first, at the top of the list again.
    app.on_key(key(KeyCode::Down));
    assert_eq!(cursor(&app), SettingsItem::General);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("│ GENERAL"), "{screen}");
    assert!(screen.contains("▸ Prefix and updates"), "{screen}");
    // Stepping down the projects, each one shows as it's reached.
    for p in 0..30 {
        app.on_key(key(KeyCode::Down));
        assert_eq!(cursor(&app), SettingsItem::Project(p));
        let name = app.projects[p].name.clone();
        let screen = draw(&mut app, 80, 24);
        assert!(screen.contains(&format!("▸ {name} ")), "{name} not shown:\n{screen}");
    }
}

#[test]
fn the_settings_list_scrolls_only_as_far_as_the_cursor_needs() {
    let top = |app: &App| match &app.mode {
        Mode::Settings(s) => s.top,
        _ => panic!("Settings closed"),
    };
    let mut app = many_projects(30);
    app.open_settings(SettingsItem::Project(29), false);
    draw(&mut app, 80, 24);
    let scrolled = top(&app);
    assert!(scrolled > 0);
    // Back up one: that row was in view already, so the list stays put.
    app.on_key(key(KeyCode::Up));
    assert_eq!(cursor(&app), SettingsItem::Project(28));
    let screen = draw(&mut app, 80, 24);
    assert_eq!(top(&app), scrolled);
    assert!(screen.contains("▸ acme-28 ") && screen.contains("  acme-29 "), "{screen}");
}

// ---- Settings → General ---------------------------------------------------------------------

#[test]
fn settings_save_the_scrollback() {
    let mut app = acme_app();
    app.open_settings(SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    for _ in 0..4 {
        app.on_key(key(KeyCode::Down));
    }
    assert_eq!(field(&app, "Scrollback"), "10,000 rows");
    app.on_key(key(KeyCode::Left));
    assert_eq!(field(&app, "Scrollback"), "5,000 rows");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Saved"));
    assert_eq!(app.config.scrollback, 5_000);
    assert_eq!(store::load_config().unwrap().scrollback, 5_000);
    // Saving the other settings leaves it be.
    app.open_settings(SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.config.scrollback, 5_000);
}

/// A number set by hand in the config file is kept, and offered among the presets.
#[test]
fn settings_keep_a_scrollback_set_by_hand() {
    let mut app = acme_app();
    app.config.scrollback = 3_000;
    app.open_settings(SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    for _ in 0..4 {
        app.on_key(key(KeyCode::Down));
    }
    assert_eq!(field(&app, "Scrollback"), "3,000 rows");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Scrollback"), "5,000 rows");
    app.on_key(key(KeyCode::Left));
    app.on_key(key(KeyCode::Left));
    assert_eq!(field(&app, "Scrollback"), "2,000 rows");
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.config.scrollback, 3_000);
}

#[test]
fn settings_save_how_to_notify() {
    let mut app = acme_app();
    assert_eq!(app.config.notify, crate::notify::Setting::Auto);
    app.open_settings(SettingsItem::General, false);
    app.on_key(key(KeyCode::Enter));
    for _ in 0..6 {
        app.on_key(key(KeyCode::Down));
    }
    assert_eq!(field(&app, "Notify when you're away"), "Desktop or terminal");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Notify when you're away"), "Terminal only (OSC 9)");
    app.on_key(key(KeyCode::Right));
    assert_eq!(field(&app, "Notify when you're away"), "Off");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(toast(&app), Some("Saved"));
    assert_eq!(app.config.notify, crate::notify::Setting::Off);
    let saved = std::fs::read_to_string(store::config_path()).unwrap();
    assert!(saved.contains(r#""notify": "off""#), "{saved}");
}
