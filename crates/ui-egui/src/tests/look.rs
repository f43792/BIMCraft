//! The window's look (fork issue 2): the Floating and Compact layouts, and the cards round the view.

use super::*;

/// The left edge of the ribbon's first tab.
fn first_tab(h: &Harness<BimApp>) -> f32 {
    h.get_by_label("Architecture").rect().left()
}

#[test]
fn the_compact_layout_puts_the_cards_flush_and_the_settings_file_keeps_it() {
    let file = std::env::temp_dir().join(format!("bimcraft-ui-look-{}", std::process::id())).join("settings.json");
    let _ = std::fs::remove_file(&file);
    let mut h = app(BOX);
    h.state_mut().use_settings(file.clone());
    h.run();
    let floating = first_tab(&h);
    press(&mut h, "Settings");
    pick(&mut h, Role::RadioButton, "Compact");
    press(&mut h, "Close");
    assert!(h.state().compact);
    let flush = first_tab(&h);
    assert_eq!(floating - flush, f32::from(crate::style::GAP), "the ribbon's card no longer stands off the window's edge");
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(kept["layout"], "compact");
    // A window started on the same file is laid out as this one was left, and Floating puts the cards back.
    let mut again = app(BOX);
    again.state_mut().use_settings(file.clone());
    again.run();
    assert!(again.state().compact);
    assert_eq!(first_tab(&again), flush);
    press(&mut again, "Settings");
    pick(&mut again, Role::RadioButton, "Floating");
    press(&mut again, "Close");
    assert_eq!(first_tab(&again), floating);
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn a_click_on_a_card_or_between_cards_is_no_click_on_the_view() {
    let mut h = app(BOX);
    tool(&mut h, "Wall");
    let walls = ids(&h, "wall").len();
    let view = h.state().view_rect;
    // On the Options Bar's card, on the status bar's, and in the gap between the Properties card and the view.
    let options = h.get_by_label("Modify | Place Wall").rect().center();
    for at in [options, pos2(view.center().x, view.bottom() + 20.0), pos2(view.left() - 3.0, view.center().y)] {
        click_at(&mut h, at);
        click_at(&mut h, at + vec2(40.0, 0.0));
    }
    assert_eq!(ids(&h, "wall").len(), walls, "no wall was drawn");
}
