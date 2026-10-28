use super::*;
use crate::app::STATUS_EVERY;
use crate::app::mouse::AUTO_SCROLL_EVERY;
use std::time::Duration;

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

/// Clicks the Claude pane's top row at `col`, `n` times in a row.
fn click_top_row(app: &mut App, col: u16, n: usize) {
    let c = app.areas.claude;
    for _ in 0..n {
        mouse(app, MouseEventKind::Down(MouseButton::Left), c.x + col, c.y);
        mouse(app, MouseEventKind::Up(MouseButton::Left), c.x + col, c.y);
    }
}

#[test]
fn a_double_click_selects_and_copies_a_word() {
    let (mut app, to_session) = at_eng_52();
    click_top_row(&mut app, 3, 2); // "Welcome"
    draw(&mut app, 80, 24);
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["Welcome"]);
    assert!(toast(&app).is_some_and(|t| t.contains("7 characters")), "{:?}", toast(&app));
    assert!(app.selection.as_ref().is_some_and(|s| s.dragged()), "it stays highlighted");
    assert_eq!(sent(&to_session), "");
    // ctrl+c copies it again and clears it.
    app.on_key(ctrl('c'));
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["Welcome", "Welcome"]);
    assert!(app.selection.is_none());
}

#[test]
fn a_triple_click_selects_the_line() {
    let (mut app, _sent) = at_eng_52();
    click_top_row(&mut app, 3, 3);
    draw(&mut app, 80, 24);
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["✻ Welcome to Claude Code"]);
}

#[test]
fn a_fourth_click_starts_over() {
    let (mut app, _sent) = at_eng_52();
    click_top_row(&mut app, 3, 4);
    draw(&mut app, 80, 24);
    assert!(app.selection.is_none());
    assert!(app.clipboard_out.is_empty());
}

#[test]
fn slow_clicks_and_clicks_elsewhere_do_not_count_together() {
    let (mut app, _sent) = at_eng_52();
    click_top_row(&mut app, 3, 1);
    // The first click was long ago.
    let (at, when, n) = app.last_click.unwrap();
    app.last_click =
        Some((at, when.checked_sub(std::time::Duration::from_millis(600)).unwrap(), n));
    click_top_row(&mut app, 3, 1);
    draw(&mut app, 80, 24);
    assert!(app.selection.is_none());
    assert!(app.clipboard_out.is_empty());

    // A click on another cell starts over, and its own second click selects.
    click_top_row(&mut app, 12, 1);
    click_top_row(&mut app, 3, 1);
    draw(&mut app, 80, 24);
    assert!(app.selection.is_none());
    click_top_row(&mut app, 3, 1);
    draw(&mut app, 80, 24);
    settle(&mut app);
    assert_eq!(decoded(&app.clipboard_out), ["Welcome"]);
}

#[test]
fn a_double_click_on_a_blank_cell_selects_nothing() {
    let (mut app, _sent) = at_eng_52();
    let c = app.areas.claude;
    for _ in 0..2 {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), c.x + 60, c.y + 1);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), c.x + 60, c.y + 1);
    }
    draw(&mut app, 80, 24);
    assert!(app.selection.is_none());
    assert!(app.clipboard_out.is_empty());
}

#[test]
fn a_double_click_selects_in_a_panel() {
    let (mut app, _sent) = at_eng_52();
    draw(&mut app, 200, 50);
    let r = app.areas.jira.unwrap();
    // The panel's border and a column of padding come first; the first row is its title or text.
    let (x, y) = (r.x + 3, r.y + 1);
    for _ in 0..2 {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
    }
    draw(&mut app, 200, 50);
    settle(&mut app);
    let copied = decoded(&app.clipboard_out);
    assert_eq!(copied.len(), 1, "{copied:?}");
    assert!(!copied[0].is_empty() && !copied[0].contains(char::is_whitespace), "{copied:?}");
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

#[test]
fn a_session_clearing_its_history_ends_the_selection_with_a_toast() {
    let (mut app, _sent) = at_eng_52();
    let parser = {
        let tv = app.tickets.iter().find(|tv| tv.t.key == "ENG-52").unwrap();
        tv.session.as_ref().unwrap().parser.clone()
    };
    // A history to lose: with none, a clear changes nothing the selection holds.
    for i in 0..60 {
        crate::session::lock(&parser).process(format!("line {i}\r\n").as_bytes());
    }
    select_top_row(&mut app, 3);
    assert!(app.selection.as_ref().is_some_and(|s| s.snapshot.is_some()));
    app.follow_selection();
    assert!(app.selection.is_some(), "nothing has changed yet");
    crate::session::lock(&parser).process(b"\x1b[3J");
    app.follow_selection();
    assert!(app.selection.is_none());
    assert!(toast(&app).is_some_and(|t| t.contains("cleared its history")), "{:?}", toast(&app));
}

#[test]
fn a_selection_held_past_its_pane_wakes_the_loop_at_the_scrolling_pace() {
    let (mut app, _sent) = at_eng_52();
    let parser = app.selected().unwrap().session.as_ref().unwrap().parser.clone();
    // A history to scroll up through.
    for i in 0..40 {
        crate::session::lock(&parser).process(format!("line {i}\r\n").as_bytes());
    }
    let c = app.areas.claude;
    assert!(c.y > 0, "room above the pane to hold the pointer");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), c.x, c.y);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), c.x + 3, c.y);
    assert_eq!(app.selection_scrolls_in(), None, "inside the pane, nothing scrolls");
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), c.x + 3, c.bottom());
    assert_eq!(app.selection_scrolls_in(), None, "the pane shows the newest rows already");
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), c.x + 3, c.y - 1);
    assert_eq!(app.selection_scrolls_in(), Some(Duration::ZERO));
    took_dirty(&app);
    let top = |app: &App| app.view_top(app.selection.as_ref().unwrap());
    let before = top(&app);
    app.follow_selection();
    assert_eq!(top(&app), before - 1);
    assert!(took_dirty(&app));
    // Until the next step is due, the event loop waits for it, not for the status poll.
    app.selection.as_mut().unwrap().scrolled_at = Some(just_now());
    assert_eq!(app.selection_scrolls_in(), Some(AUTO_SCROLL_EVERY));
    app.last_status_poll = just_now();
    assert_eq!(app.tick_in(), AUTO_SCROLL_EVERY);
    app.follow_selection();
    assert_eq!(top(&app), before - 1, "not due yet");
    // At the top of the history, nothing is left to scroll: no step, no wake, no frame.
    for steps in 0.. {
        if app.selection_scrolls_in().is_none() {
            break;
        }
        assert!(steps < 100, "still scrolling up at {}", top(&app));
        app.selection.as_mut().unwrap().scrolled_at = None;
        app.follow_selection();
    }
    took_dirty(&app);
    assert_eq!(app.selection_scrolls_in(), None);
    assert_eq!(app.tick_in(), STATUS_EVERY);
    let oldest = top(&app);
    app.follow_selection();
    assert_eq!(top(&app), oldest);
    assert!(!took_dirty(&app));
    // Moved back to the bottom edge, there's history to come back down through.
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), c.x + 3, c.bottom());
    assert!(app.selection_scrolls_in().is_some());
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), c.x + 3, c.bottom());
    assert_eq!(app.selection_scrolls_in(), None);
    assert_eq!(app.tick_in(), STATUS_EVERY);
}

#[test]
fn a_selection_held_past_a_panel_scrolls_only_as_far_as_its_lines_go() {
    let (mut app, _sent) = at_eng_52();
    draw(&mut app, 200, 50);
    let r = app.areas.jira.unwrap();
    // The panel's inner rows are between its borders.
    let (x, above, below) = (r.x + 3, r.y, r.bottom() - 1);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, r.y + 1);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, above);
    assert_eq!(app.selection_scrolls_in(), None, "the panel is at its top");
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, below);
    draw(&mut app, 200, 50);
    let rows = app.panel.as_ref().map(crate::selection::Panel::rows).unwrap();
    assert!(rows > 2, "{rows} rows");
    assert_eq!(app.selection_scrolls_in(), Some(Duration::ZERO));
    // A step short of the end, one more step, then none.
    app.tickets[app.sel].jira_scroll = u16::try_from(rows - 2).unwrap();
    took_dirty(&app);
    app.follow_selection();
    assert_eq!(usize::from(app.tickets[app.sel].jira_scroll), rows - 1);
    assert!(took_dirty(&app));
    draw(&mut app, 200, 50);
    took_dirty(&app);
    assert_eq!(app.selection_scrolls_in(), None, "its last row is at the top");
    app.selection.as_mut().unwrap().scrolled_at = None;
    app.follow_selection();
    assert_eq!(usize::from(app.tickets[app.sel].jira_scroll), rows - 1);
    assert!(!took_dirty(&app));
}

// ---- the mouse -----------------------------------------------------------------------------

#[test]
fn clicks_in_the_sidebar_select_tickets_and_fold_projects() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    let at = |app: &App, hit: crate::app::Hit| {
        app.sidebar_hits.iter().find(|(_, h)| *h == hit).unwrap().0
    };
    let card = at(&app, crate::app::Hit::Ticket(index(&app, "ENG-61")));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), card.x + 3, card.y + 1);
    assert_eq!(selected(&app), "ENG-61");
    // The wheel over a panel scrolls it.
    let jira = app.areas.jira.unwrap();
    mouse(&mut app, MouseEventKind::ScrollDown, jira.x + 3, jira.y + 3);
    assert_eq!(app.tickets[app.sel].jira_scroll, 2);
    draw(&mut app, 160, 40);
    let notes = at(&app, crate::app::Hit::Project(1));
    // A header folds on release, once it's known not to be a drag.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), notes.x + 1, notes.y);
    assert!(app.projects[1].collapsed);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), notes.x + 1, notes.y);
    assert!(!app.projects[1].collapsed);
    assert!(!saved().projects[1].collapsed);
}

/// Presses and releases the mouse on project `p`'s header.
fn click_header(app: &mut App, p: usize) {
    let r = app.sidebar_hits.iter().find(|(_, h)| *h == Hit::Project(p)).unwrap().0;
    mouse(app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
    mouse(app, MouseEventKind::Up(MouseButton::Left), r.x + 1, r.y);
}

/// The project Settings shows.
fn settings_project(app: &App) -> crate::app::SettingsItem {
    let Mode::Settings(s) = &app.mode else { panic!("Settings closed") };
    s.items[s.cursor]
}

#[test]
fn a_clicked_header_is_the_current_project_until_a_card_is_selected() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    app.select(index(&app, "ENG-52"));
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(field(&app, "Project"), "acme");
    app.on_key(key(KeyCode::Esc));
    // A click on notes' header unfolds it, as before, and makes notes current, the card staying
    // selected.
    click_header(&mut app, 1);
    assert!(!app.projects[1].collapsed);
    assert_eq!(selected(&app), "ENG-52");
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(field(&app, "Project"), "notes");
    app.on_key(key(KeyCode::Esc));
    prefix(&mut app, KeyCode::Char(','));
    assert_eq!(settings_project(&app), crate::app::SettingsItem::Project(1));
    app.on_key(key(KeyCode::Esc));
    // A second click folds it again; notes stays current.
    draw(&mut app, 160, 40);
    click_header(&mut app, 1);
    assert!(app.projects[1].collapsed);
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(field(&app, "Project"), "notes");
    app.on_key(key(KeyCode::Esc));
    // Selecting a card in acme again, even the one already selected, makes acme current.
    draw(&mut app, 160, 40);
    let r = card(&app, "ENG-52").unwrap();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 3, r.y + 1);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x + 3, r.y + 1);
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(field(&app, "Project"), "acme");
    app.on_key(key(KeyCode::Esc));
    prefix(&mut app, KeyCode::Char(','));
    assert_eq!(settings_project(&app), crate::app::SettingsItem::Project(0));
}

#[test]
fn an_empty_projects_header_makes_it_current() {
    let mut app = acme_app();
    let web = serde_json::json!({ "name": "web", "repo": "/src/web" });
    app.projects.push(serde_json::from_value(web).unwrap());
    draw(&mut app, 160, 40);
    click_header(&mut app, 2);
    prefix(&mut app, KeyCode::Char('n'));
    assert_eq!(field(&app, "Project"), "web");
    app.on_key(key(KeyCode::Esc));
    prefix(&mut app, KeyCode::Char(','));
    assert_eq!(settings_project(&app), crate::app::SettingsItem::Project(2));
    app.on_key(key(KeyCode::Esc));
    // With no cards at all, `n` alone opens the form, on the clicked project too.
    app.tickets.clear();
    draw(&mut app, 160, 40);
    click_header(&mut app, 2);
    app.on_key(key(KeyCode::Char('n')));
    assert_eq!(field(&app, "Project"), "web");
}

/// Where the sidebar draws `key`'s card, if it does.
fn card(app: &App, key: &str) -> Option<ratatui::layout::Rect> {
    let hit = Hit::Ticket(index(app, key));
    app.sidebar_hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r)
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
    // The find box: the full list comes back from the top, and scrolls to show it.
    prefix(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "ENG-52");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), "ENG-52");
    draw(&mut app, 160, 40);
    let rows = list_rows(&app);
    let r = card(&app, "ENG-52").expect("back in view");
    assert!(rows.contains(&r.y) && r.bottom() <= rows.end && r.height == 3, "shown whole: {r:?}");
    // The wheel hides its first rows; a click on what's left scrolls back just enough to show
    // it whole, under the "more" row.
    while card(&app, "ENG-52").is_some_and(|r| r.height == 3) {
        mouse(&mut app, MouseEventKind::ScrollDown, s.x + 2, s.y + 5);
        draw(&mut app, 160, 40);
    }
    let partial = card(&app, "ENG-52").expect("partly in view");
    assert_eq!(partial.y, list_rows(&app).start + 1);
    let top = app.sidebar_scroll.top;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), partial.x + 3, partial.y);
    assert_eq!(selected(&app), "ENG-52");
    draw(&mut app, 160, 40);
    let r = card(&app, "ENG-52").unwrap();
    assert_eq!((r.y, r.height), (list_rows(&app).start + 1, 3));
    assert!(app.sidebar_scroll.top < top);
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
