use super::*;
use std::time::Duration;

// ---- reordering the sidebar ----------------------------------------------------------------

/// The keys of the tickets (not the agents) of acme, in the order the sidebar lists them.
fn tickets_of_acme(app: &App) -> Vec<String> {
    let keys = app.members(0).into_iter().map(|i| app.tickets[i].t.key.clone());
    keys.filter(|k| k.starts_with("ENG")).collect()
}

fn project_names(app: &App) -> Vec<&str> {
    app.projects.iter().map(|p| p.name.as_str()).collect()
}

/// The rows `hit` is drawn on.
fn rows_of(app: &App, hit: Hit) -> ratatui::layout::Rect {
    app.sidebar_hits.iter().find(|(_, h)| *h == hit).unwrap().0
}

/// Whether the sidebar drew project headers and no cards.
fn only_projects(app: &App) -> bool {
    !app.sidebar_hits.is_empty()
        && app.sidebar_hits.iter().all(|(_, h)| matches!(h, Hit::Project(_)))
}

fn card_rows(app: &App, key: &str) -> ratatui::layout::Rect {
    rows_of(app, Hit::Ticket(index(app, key)))
}

/// Presses on `key`'s card and drags to row `y`, redrawing as the event loop does. The button
/// stays down.
fn drag_card_to(app: &mut App, key: &str, y: u16) {
    let r = card_rows(app, key);
    mouse(app, MouseEventKind::Down(MouseButton::Left), r.x + 3, r.y);
    draw(app, 160, 40);
    mouse(app, MouseEventKind::Drag(MouseButton::Left), r.x + 3, y);
    draw(app, 160, 40);
}

fn release(app: &mut App, row: u16) {
    mouse(app, MouseEventKind::Up(MouseButton::Left), 3, row);
    draw(app, 160, 40);
}

fn footer(app: &mut App) -> String {
    let screen = draw(app, 160, 40);
    let line = screen.lines().rev().find(|l| l.contains("Esc")).unwrap_or_default();
    line.trim_matches(|c: char| c == '"' || c == ' ').to_string()
}

#[test]
fn prefix_braces_start_move_mode_and_the_arrows_go_on() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    prefix(&mut app, KeyCode::Char('}'));
    assert!(matches!(app.mode, Mode::Move(_)));
    assert_eq!(tickets_of_acme(&app), ["ENG-53", "ENG-52", "ENG-61", "ENG-70"]);
    assert!(footer(&mut app).contains("moving ENG-52 · ↑/↓ move"), "{}", footer(&mut app));
    // No prefix now: braces and arrows.
    app.on_key(key(KeyCode::Char('}')));
    app.on_key(key(KeyCode::Down));
    assert_eq!(tickets_of_acme(&app), ["ENG-53", "ENG-61", "ENG-70", "ENG-52"]);
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Char('{')));
    assert_eq!(tickets_of_acme(&app), ["ENG-53", "ENG-52", "ENG-61", "ENG-70"]);
    assert_eq!(selected(&app), "ENG-52", "the selection goes with the card");
    // Saved as it goes: the file lists the cards in the new order.
    let order: Vec<_> = saved().tickets.iter().map(|t| t.key.clone()).collect();
    assert_eq!(&order[2..6], ["ENG-53", "ENG-52", "ENG-61", "ENG-70"]);
    // Enter keeps it.
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(tickets_of_acme(&app), ["ENG-53", "ENG-52", "ENG-61", "ENG-70"]);
}

#[test]
fn esc_puts_the_order_back_as_it_was_before_move_mode() {
    let (mut app, to_session) = at_eng_52();
    prefix(&mut app, KeyCode::Char('}'));
    app.on_key(key(KeyCode::Char('}')));
    app.on_key(key(KeyCode::Char('p')));
    app.on_key(key(KeyCode::Down));
    assert_eq!(project_names(&app), ["notes", "acme"]);
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(tickets_of_acme(&app), ["ENG-52", "ENG-53", "ENG-61", "ENG-70"]);
    assert_eq!(project_names(&app), ["acme", "notes"]);
    assert_eq!(selected(&app), "ENG-52");
    assert_eq!(saved().projects[0].name, "acme");
    assert_eq!(sent(&to_session), "", "the keys of move mode never reach the session");
}

#[test]
fn any_other_key_ends_move_mode_and_is_spent_on_it() {
    let (mut app, to_session) = at_eng_52();
    prefix(&mut app, KeyCode::Char('{'));
    assert!(matches!(app.mode, Mode::Move(_)), "at the edge, nothing moves, but the mode starts");
    assert_eq!(tickets_of_acme(&app), ["ENG-52", "ENG-53", "ENG-61", "ENG-70"]);
    app.on_key(key(KeyCode::Char('y')));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(sent(&to_session), "");
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(sent(&to_session), "y");
}

#[test]
fn p_moves_the_project_with_every_project_folded_and_their_folds_come_back() {
    let mut app = acme_app();
    app.projects[1].collapsed = false;
    draw(&mut app, 160, 40);
    prefix(&mut app, KeyCode::Char('}'));
    app.on_key(key(KeyCode::Char('p')));
    let screen = draw(&mut app, 160, 40);
    assert!(footer(&mut app).contains("moving project acme"));
    assert!(screen.contains("▸ acme (6)") && screen.contains("▸ notes (1)"), "{screen}");
    assert!(only_projects(&app), "every project is folded while one moves");
    app.on_key(key(KeyCode::Down));
    assert_eq!(project_names(&app), ["notes", "acme"]);
    // The edge: nothing past the last project.
    app.on_key(key(KeyCode::Down));
    assert_eq!(project_names(&app), ["notes", "acme"]);
    app.on_key(key(KeyCode::Enter));
    let screen = draw(&mut app, 160, 40);
    assert!(screen.contains("ENG-52") && screen.contains("✦ writer"), "{screen}");
    assert!(!app.projects[0].collapsed && !app.projects[1].collapsed, "no fold state changed");
    assert_eq!(saved().projects[0].name, "notes");
}

#[test]
fn previous_and_next_follow_the_new_order() {
    let mut app = acme_app();
    app.projects[1].collapsed = false;
    app.select(index(&app, "ENG-70"));
    prefix(&mut app, KeyCode::Char('{'));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Enter));
    // Agents, then ENG-52, ENG-70, ENG-53 ...
    app.select(index(&app, "ENG-52"));
    prefix(&mut app, KeyCode::Char(']'));
    assert_eq!(selected(&app), "ENG-70");
    // And a project moved ahead comes before the others' cards.
    app.select(index(&app, "notes-writer"));
    prefix(&mut app, KeyCode::Char('{'));
    app.on_key(key(KeyCode::Char('p')));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Enter));
    prefix(&mut app, KeyCode::Char(']'));
    assert_eq!(selected(&app), "acme-lead");
}

#[test]
fn a_card_stops_at_the_edge_of_its_group() {
    let mut app = acme_app();
    app.select(index(&app, "acme-lead"));
    prefix(&mut app, KeyCode::Char('{'));
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.members(0)[0], index(&app, "acme-lead"));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    // Past the other agent, but never among the tickets.
    let keys: Vec<_> = app.members(0).iter().map(|&i| app.tickets[i].t.key.clone()).collect();
    assert_eq!(keys[..3], ["acme-release-notes-reviewer", "acme-lead", "ENG-52"]);
}

#[test]
fn a_click_on_a_card_is_not_a_drag() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    let r = card_rows(&app, "ENG-61");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 3, r.y);
    // Still on the card's own rows: not a drag.
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), r.x + 5, r.y + 2);
    draw(&mut app, 160, 40);
    assert!(app.dragged().is_none());
    assert!(!footer(&mut app).contains("move ENG-61"));
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x + 5, r.y + 2);
    assert_eq!(selected(&app), "ENG-61");
    assert_eq!(tickets_of_acme(&app), ["ENG-52", "ENG-53", "ENG-61", "ENG-70"]);
    assert!(app.drag.is_none());
}

#[test]
fn dragging_a_card_shows_where_it_lands_and_dropping_moves_it() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    // ENG-52 to the lower half of ENG-61's rows: below it.
    let below = card_rows(&app, "ENG-61").bottom() - 1;
    drag_card_to(&mut app, "ENG-52", below);
    assert!(app.dragged().is_some());
    assert_eq!(footer(&mut app), "move ENG-52 below ENG-61 · Esc cancels");
    assert_eq!(tickets_of_acme(&app), ["ENG-52", "ENG-53", "ENG-61", "ENG-70"], "not yet");
    // The card stays where it is, and the line sits on the row under ENG-61.
    let line = app.drag.as_ref().and_then(|d| d.target).unwrap().line;
    assert_eq!(line, card_rows(&app, "ENG-61").bottom());
    release(&mut app, below);
    assert_eq!(tickets_of_acme(&app), ["ENG-53", "ENG-61", "ENG-52", "ENG-70"]);
    assert_eq!(selected(&app), "ENG-52");
    assert!(app.drag.is_none());
    let order: Vec<_> = saved().tickets.iter().map(|t| t.key.clone()).collect();
    assert_eq!(&order[2..6], ["ENG-53", "ENG-61", "ENG-52", "ENG-70"]);
    // And up to the top of its group: above the first ticket.
    let top = card_rows(&app, "ENG-53").y;
    drag_card_to(&mut app, "ENG-70", top);
    assert_eq!(footer(&mut app), "move ENG-70 above ENG-53 · Esc cancels");
    release(&mut app, top);
    assert_eq!(tickets_of_acme(&app), ["ENG-70", "ENG-53", "ENG-61", "ENG-52"]);
}

#[test]
fn a_drop_where_the_card_cannot_go_snaps_to_the_nearest_place_it_can() {
    let mut app = acme_app();
    draw(&mut app, 160, 40);
    // Over the agents above its group: the group's first place.
    let agent = card_rows(&app, "acme-lead").y;
    drag_card_to(&mut app, "ENG-70", agent);
    assert_eq!(footer(&mut app), "move ENG-70 above ENG-52 · Esc cancels");
    release(&mut app, agent);
    assert_eq!(tickets_of_acme(&app), ["ENG-70", "ENG-52", "ENG-53", "ENG-61"]);
    // Over another project's header, far below: the last place.
    let notes = rows_of(&app, Hit::Project(1)).y;
    drag_card_to(&mut app, "ENG-70", notes);
    release(&mut app, notes);
    assert_eq!(tickets_of_acme(&app), ["ENG-52", "ENG-53", "ENG-61", "ENG-70"]);
    // Dropped on its own place, nothing changes.
    let own = card_rows(&app, "ENG-53");
    drag_card_to(&mut app, "ENG-53", own.bottom() + 1);
    assert_eq!(footer(&mut app), "move ENG-53 · no change · Esc cancels");
    release(&mut app, own.bottom() + 1);
    assert_eq!(tickets_of_acme(&app), ["ENG-52", "ENG-53", "ENG-61", "ENG-70"]);
}

#[test]
fn esc_during_a_drag_cancels_it() {
    let (mut app, to_session) = at_eng_52();
    let below = card_rows(&app, "ENG-70").bottom();
    drag_card_to(&mut app, "ENG-52", below);
    assert!(app.dragged().is_some());
    app.on_key(key(KeyCode::Esc));
    assert!(app.drag.is_none());
    assert_eq!(sent(&to_session), "", "the Esc is spent on the drag");
    release(&mut app, below);
    assert_eq!(tickets_of_acme(&app), ["ENG-52", "ENG-53", "ENG-61", "ENG-70"]);
    // A later Esc is the session's again.
    app.on_key(key(KeyCode::Esc));
    assert_eq!(sent(&to_session), "\x1b");
}

#[test]
fn dragging_a_project_header_moves_the_project_with_all_folded() {
    let mut app = acme_app();
    app.projects[1].collapsed = false;
    draw(&mut app, 160, 40);
    let acme = rows_of(&app, Hit::Project(0));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), acme.x + 3, acme.y);
    assert!(!app.projects[0].collapsed, "the header folds on release, if it was a click");
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), acme.x + 3, acme.y + 5);
    draw(&mut app, 160, 40);
    assert!(only_projects(&app), "every project folded while one is dragged");
    let notes = rows_of(&app, Hit::Project(1));
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), notes.x + 3, notes.y + 1);
    draw(&mut app, 160, 40);
    assert_eq!(footer(&mut app), "move project acme below notes · Esc cancels");
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), notes.x + 3, notes.y + 1);
    assert_eq!(project_names(&app), ["notes", "acme"]);
    assert_eq!(saved().projects[0].name, "notes");
    let screen = draw(&mut app, 160, 40);
    assert!(screen.contains("ENG-52"), "folds are back after the drop");
    assert!(!app.projects[0].collapsed && !app.projects[1].collapsed);
    assert_eq!(selected(&app), "ENG-52");
}

#[test]
fn a_long_sidebar_scrolls_while_a_drag_rests_at_its_edge() {
    let mut app = long_sidebar(20);
    draw(&mut app, 160, 40);
    let rows = list_rows(&app);
    drag_card_to(&mut app, "ENG-52", rows.end - 1);
    assert_eq!(app.sidebar_scroll.top, 0);
    assert_eq!(app.drag_scrolls_in(), Some(Duration::ZERO));
    app.follow_drag();
    assert_eq!(app.sidebar_scroll.top, 1);
    // Throttled: another call straight away waits, and the event loop waits as long.
    app.follow_drag();
    assert_eq!(app.sidebar_scroll.top, 1);
    let next = app.drag_scrolls_in().unwrap();
    assert!(!next.is_zero() && next <= Duration::from_millis(80), "{next:?}");
    // In the middle of the list, nothing scrolls.
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 3, rows.start + 8);
    app.drag.as_mut().unwrap().scrolled_at = None;
    assert_eq!(app.drag_scrolls_in(), None);
    app.follow_drag();
    assert_eq!(app.sidebar_scroll.top, 1);
    // At the top edge, up again.
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 3, rows.start);
    app.follow_drag();
    assert_eq!(app.sidebar_scroll.top, 0);
    // And no further: nothing to wake for.
    assert_eq!(app.drag_scrolls_in(), None);
}

#[test]
fn dragging_in_a_pane_still_selects_text() {
    let (mut app, _) = at_eng_52();
    select_top_row(&mut app, 6);
    assert!(app.selection.as_ref().is_some_and(|s| s.released));
    assert!(app.drag.is_none());
}
