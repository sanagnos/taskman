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
    assert_eq!(cursor(&app), SettingsItem::NewAccount);
    let screen = draw(&mut app, 80, 24);
    assert!(screen.contains("▸ + new account"), "{screen}");
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
