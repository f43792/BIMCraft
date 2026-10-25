//! Point clouds in the window, driven as a user drives them: Insert > Point Cloud brings scans in through its box, the
//! Properties palette shows and moves one, and Visibility/Graphics hides them as a category.

use std::path::PathBuf;

use super::*;

/// A scan file in a folder of this test's own: a wall face 2 m long and 2 m tall at `y` m, in metres.
fn face(test: &str, name: &str, y: f64) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bimcraft-ui-clouds-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let text: String =
        (0..=50).flat_map(|i| (0..=50).map(move |k| format!("{} {y} {}\n", f64::from(i) * 0.04, f64::from(k) * 0.04))).collect();
    std::fs::write(&path, text).unwrap();
    path
}

fn scans(h: &Harness<BimApp>) -> Vec<Id> {
    let m = &h.state().session.model;
    m.elements.iter().filter(|(_, e)| matches!(e, Element::PointCloud(_))).map(|(id, _)| *id).collect()
}

#[test]
fn insert_point_cloud_brings_the_files_picked_in_as_one_scan_and_selects_it() {
    let mut h = app(BOX);
    press(&mut h, "Insert");
    assert!(h.query_by_label("Link Model").is_some(), "the Insert tab links models too");
    // The system's dialog gives the files; a test hands them over as it would.
    let (a, b) = (face("insert", "east.xyz", 0.0), face("insert", "west.xyz", 3.0));
    h.state_mut().point_cloud_paths(vec![a, b]);
    h.run();
    assert!(h.query_by_label("2 files, brought in as one scan's stations").is_some());
    type_into(&mut h, "Name", "survey");
    type_into(&mut h, "Spacing (mm)", "100");
    pick(&mut h, Role::RadioButton, "Auto - Origin to Origin");
    press(&mut h, "OK");
    let placed = scans(&h);
    assert_eq!(placed.len(), 1);
    let m = &h.state().session.model;
    let cloud = m.clouds.get("survey").unwrap();
    assert_eq!(cloud.stations().len(), 2);
    // Origin to Origin: where the files put it.
    let (lo, hi) = m.cloud_box(placed[0]).unwrap();
    assert!((lo[1] - 0.0).abs() < 1.0 && (hi[1] - 3000.0).abs() < 1.0, "{lo:?} {hi:?}");
    assert_eq!(h.state().selection, placed, "what was brought in is selected, as Revit does");
    assert!(h.query_by_label("2 files, brought in as one scan's stations").is_none(), "the box closed");
}

#[test]
fn a_scan_that_cant_come_in_says_why_in_its_box() {
    let mut h = app(BOX);
    let bad = face("bad", "scan.laz", 0.0);
    h.state_mut().point_cloud_paths(vec![bad]);
    h.run();
    press(&mut h, "OK");
    assert!(scans(&h).is_empty());
    assert!(h.query_by_label_contains("decompress it to LAS").is_some(), "the reason shows in the box, which stays open");
    press(&mut h, "Cancel");
    assert!(h.query_by_label("OK").is_none());
}

#[test]
fn properties_show_a_scan_and_move_it() {
    let path = face("properties", "face.xyz", 0.0);
    let mut h = app(&format!("point_cloud {}", json!({"path": path})));
    let id = scans(&h)[0];
    h.state_mut().selection = vec![id];
    h.run();
    // (File is the File menu's too.)
    for label in ["Scan", "Points", "Offset", "Angle"] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    assert!(h.query_by_label("face.xyz").is_some());
    type_into(&mut h, "Offset", "500");
    h.key_press(Key::Enter);
    h.run();
    let Some(Element::PointCloud(pc)) = h.state().session.model.elements.get(id) else { panic!() };
    assert_eq!(pc.offset, 500.0);
}

#[test]
fn visibility_graphics_lists_point_clouds_with_the_model_categories() {
    let path = face("vg", "face.xyz", 0.0);
    let mut h = app(&format!("point_cloud {}", json!({"path": path})));
    press(&mut h, "View");
    press(&mut h, "Visibility/Graphics");
    // Last of the model's categories, it is found by name.
    type_into(&mut h, "Category name search:", "point");
    pick(&mut h, Role::CheckBox, "Point Clouds");
    press(&mut h, "OK");
    let m = &h.state().session.model;
    let level = m.levels()[0].0;
    assert!(!bimcraft_core::graphics::Look::of(m, level).shows(scans(&h)[0]), "the plan hides them now");
}
