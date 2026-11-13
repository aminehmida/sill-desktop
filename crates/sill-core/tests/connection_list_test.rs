//! iOSClient/ConnectionList.swift's rules, against Tests/checks/connection-list's cases
//! (ConnectionListTest.kt). The rows of ui/MacTiles.kt are the GUI's and not ported.

use std::collections::{BTreeMap, BTreeSet};

use sill_core::net::connection_list::{PreviewPlan, Section, Style};
use sill_core::net::{ConnectionList as C, SavedMacs};

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}
fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}
fn map(v: &[(&str, &str)]) -> BTreeMap<String, String> {
    v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}
fn plan(delete: &[&str], show: &[&str]) -> PreviewPlan {
    PreviewPlan { delete: strings(delete), show: strings(show) }
}

#[test]
fn style_defaults_to_tiles_and_a_stored_choice_wins() {
    assert_eq!(Style::Tiles, C::DEFAULT_STYLE);
    for raw in [None, Some(""), Some("grid"), Some("List"), Some("tiles")] {
        assert_eq!(Style::Tiles, C::stored_style(raw));
    }
    assert_eq!(Style::List, C::stored_style(Some("list")));
    assert_eq!(Style::Tiles, Style::List.toggled());
    assert_eq!(Style::List, Style::Tiles.toggled());
    let mut shown = C::DEFAULT_STYLE;
    for _ in 0..4 {
        shown = C::stored_style(Some(shown.toggled().raw()));
    }
    assert_eq!(C::DEFAULT_STYLE, shown, "four toggles from the default come back to it");
}

#[test]
fn pictures_are_taken_and_kept_only_with_tiles_for_a_saved_mac() {
    for style in [Style::List, Style::Tiles] {
        for fg in [true, false] {
            for saved in [true, false] {
                assert_eq!(style == Style::Tiles && fg && saved, C::takes_preview(style, fg, saved));
            }
        }
        for saved in [true, false] {
            assert_eq!(style == Style::Tiles && saved, C::keeps_decoded_picture(style, saved));
        }
    }
}

#[test]
fn the_list_hides_the_pictures_and_keeps_their_files() {
    let on_disk = strings(&["M1.jpg", "M2.jpg", "GONE.jpg", "notes.txt"]);
    let saved = set(&["M1", "M2"]);
    assert_eq!(plan(&["GONE.jpg"], &["M1", "M2"]), C::preview_plan(Style::Tiles, &on_disk, &saved));
    let kept = strings(&["M1.jpg", "M2.jpg", "notes.txt"]);
    assert_eq!(plan(&[], &[]), C::preview_plan(Style::List, &kept, &saved), "switched to the list: nothing shows, nothing deleted");
    assert_eq!(plan(&[], &["M1", "M2"]), C::preview_plan(Style::Tiles, &kept, &saved), "back to tiles: every kept picture again");
    assert_eq!(plan(&["GONE.jpg"], &[]), C::preview_plan(Style::List, &on_disk, &saved));
    assert_eq!(plan(&["M2.jpg"], &["M1"]), C::preview_plan(Style::Tiles, &strings(&["M1.jpg", "M2.jpg"]), &set(&["M1"])));
    assert_eq!(strings(&["M2"]), C::preview_plan(Style::Tiles, &strings(&["M1.png", "M1.jpg.jpg", ".jpg", "M2.jpg"]), &saved).show);
    assert_eq!(strings(&["M1", "M2"]), C::preview_plan(Style::Tiles, &strings(&["M2.jpg", "M1.jpg"]), &saved).show);
}

#[test]
fn preview_file_names_refuse_paths_and_stale_files_are_pruned() {
    assert_eq!(Some("041061050R3GG28A.jpg".to_string()), C::preview_file_name("041061050R3GG28A"));
    let long = "x".repeat(129);
    for bad in ["", "../secret", "a/b", "a.b", "é", long.as_str()] {
        assert_eq!(None, C::preview_file_name(bad), "{bad}");
    }
    assert_eq!(
        strings(&["M2.jpg"]),
        C::stale_preview_files(&strings(&["M1.jpg", "M2.jpg", "M3.jpg", "notes.txt", "M2.png"]), &set(&["M1", "M3"]))
    );
}

#[test]
fn columns_and_pixels() {
    assert_eq!((2, 2, 1, 1), (C::columns(380.0, false), C::columns(340.0, false), C::columns(339.0, false), C::columns(380.0, true)));
    let px = |w, h| C::preview_pixels(w, h);
    assert_eq!(Some((640, 415)), px(3024, 1964));
    assert_eq!(Some((640, 360)), px(1920, 1080));
    assert_eq!(Some((640, 360)), px(6016, 3384));
    assert_eq!(Some((640, 400)), px(640, 400));
    assert_eq!(Some((640, 400)), px(641, 401));
    assert_eq!(Some((500, 300)), px(500, 300));
    assert_eq!(Some((1, 1)), px(1, 1));
    assert_eq!(Some((640, 1137)), px(1080, 1920));
    assert_eq!(Some((640, 1)), px(7680, 1));
    assert_eq!((None, None, None), (px(0, 100), px(100, 0), px(-5, 10)));
}

#[test]
fn a_black_capture_never_replaces_a_picture() {
    let argb = |r: u32, g: u32, b: u32| (255 << 24) | (r << 16) | (g << 8) | b;
    let black = vec![argb(0, 0, 0); 40];
    assert!(C::looks_black(&black, 8, 5));
    assert!(!C::looks_black(&[argb(96, 96, 96); 40], 8, 5));
    assert!(C::looks_black(&[argb(24, 24, 24); 40], 8, 5));
    assert!(!C::looks_black(&[argb(25, 25, 25); 40], 8, 5));
    let mut last = black.clone();
    last[39] = argb(0, 0, 200);
    assert!(!C::looks_black(&last, 8, 5), "one bright pixel, the last");
    let mut first = black.clone();
    first[0] = argb(0, 200, 0);
    assert!(!C::looks_black(&first, 8, 5));
    assert!(C::looks_black(&[], 0, 5), "no picture counts as black");
    assert!(C::looks_black(&[argb(200, 200, 200); 10], 8, 5), "too few pixels counts as black");
}

#[test]
fn group_names_are_cleaned() {
    assert_eq!(None, C::clean_group(None));
    assert_eq!(None, C::clean_group(Some("   \n\t ")));
    assert_eq!(Some("Work Office".to_string()), C::clean_group(Some("  Work \n Office ")));
    assert_eq!(32, C::clean_group(Some(&"A".repeat(40))).unwrap().len());
    assert_eq!(Some("B".repeat(31)), C::clean_group(Some(&format!("{} C", "B".repeat(31)))), "a name cut at the cap ends without a space");
    for other in ["Other", " other ", "OTHER"] {
        assert_eq!(None, C::clean_group(Some(other)));
    }
    assert_eq!(Some("Others".to_string()), C::clean_group(Some("Others")));
    assert_eq!(Some("Other Macs".to_string()), C::clean_group(Some("Other Macs")));
    assert_eq!(Some("Home".to_string()), C::clean_group(Some("home")));
    assert_eq!(Some("Work".to_string()), C::clean_group(Some("WORK")));
    assert_eq!(Some("Personal".to_string()), C::clean_group(Some(" personal ")));
    assert_eq!(32, C::clean_group(Some(&"🏠".repeat(40))).unwrap().chars().count(), "a character is never cut in half");
}

#[test]
fn groups_are_stored_set_and_carried() {
    assert!(C::decode_groups(None).is_empty() && C::decode_groups(Some("not json")).is_empty());
    let mut groups = C::setting_group(Some(" work "), "M1", &BTreeMap::new());
    groups = C::setting_group(Some("home"), "M2", &groups);
    assert_eq!(map(&[("M1", "Work"), ("M2", "Home")]), groups);
    assert_eq!(groups, C::decode_groups(Some(&C::encode_groups(&groups))));
    assert_eq!(r#"{"M1":"Work","M2":"Home"}"#, C::encode_groups(&map(&[("M2", "Home"), ("M1", "Work")])));
    groups = C::setting_group(Some("  "), "M1", &groups);
    assert_eq!(map(&[("M2", "Home")]), groups);
    assert!(C::setting_group(Some("other"), "M2", &groups).is_empty());
    assert_eq!(map(&[("M2", "Lab")]), C::decode_groups(Some(r#"{"":"Work","M1":"   ","M2":"Lab","M3":"Other","M4":7}"#)));
    assert_eq!(
        map(&[("M1", "Home Office"), ("M3", "Home Office")]),
        C::setting_group(Some("home office"), "M3", &map(&[("M1", "Home Office")]))
    );
    assert_eq!(map(&[("M1", "Studio"), ("M3", "Lab")]), C::setting_group(Some("Lab"), "M3", &map(&[("M1", "Studio")])));
    assert_eq!(map(&[("NEW", "Work"), ("M1", "Home")]), C::carrying_group("OLD", "NEW", &map(&[("OLD", "Work"), ("M1", "Home")])));
    assert_eq!(map(&[("NEW", "Lab")]), C::carrying_group("OLD", "NEW", &map(&[("OLD", "Work"), ("NEW", "Lab")])));
    assert_eq!(map(&[("M1", "Home")]), C::carrying_group("M1", "M1", &map(&[("M1", "Home")])));
    assert_eq!(map(&[("M1", "Home")]), C::keeping_saved(&map(&[("M1", "Home"), ("GONE", "Work")]), &set(&["M1", "M2"])));
}

#[test]
fn groups_come_in_a_stable_order() {
    assert_eq!(strings(&["Home", "Work", "Personal"]), C::available_groups(&BTreeMap::new()));
    assert_eq!(
        strings(&["Home", "Work", "Personal", "Client", "Studio"]),
        C::available_groups(&map(&[("M1", "Studio"), ("M2", "Work"), ("M3", "Client")]))
    );
    assert_eq!(
        strings(&["Home", "Work", "Personal", "Attic", "lab", "Room 9", "Room 10"]),
        C::available_groups(&map(&[("M1", "lab"), ("M2", "Attic"), ("M3", "Room 10"), ("M4", "Room 9")]))
    );
}

#[derive(Debug, Clone, PartialEq)]
struct Row(&'static str, Option<&'static str>);

#[test]
fn sections_follow_the_groups() {
    let rows = vec![Row("A", Some("M1")), Row("B", Some("M2")), Row("C", None), Row("D", Some("M3"))];
    let id = |r: &Row| r.1.map(str::to_string);
    let section = |title: Option<&str>, items: Vec<Row>| Section { title: title.map(str::to_string), items };
    assert!(C::sections(&Vec::<Row>::new(), id, &map(&[("M1", "Work")])).is_empty());
    assert_eq!(vec![section(None, rows.clone())], C::sections(&rows, id, &BTreeMap::new()));
    assert_eq!(vec![section(None, rows.clone())], C::sections(&rows, id, &map(&[("OTHER", "Work")])));
    assert_eq!(
        vec![
            section(Some("Home"), vec![rows[1].clone()]),
            section(Some("Work"), vec![rows[0].clone(), rows[3].clone()]),
            section(Some("Other"), vec![rows[2].clone()])
        ],
        C::sections(&rows, id, &map(&[("M1", "Work"), ("M3", "Work"), ("M2", "Home")]))
    );
    let customs = map(&[("M1", "Studio"), ("M2", "Client"), ("M3", "Personal")]);
    let titles = |rows: &[Row]| -> Vec<String> { C::sections(rows, id, &customs).into_iter().map(|s| s.title.unwrap()).collect() };
    assert_eq!(strings(&["Personal", "Client", "Studio", "Other"]), titles(&rows));
    let reversed: Vec<Row> = rows.iter().rev().cloned().collect();
    assert_eq!(strings(&["Personal", "Client", "Studio", "Other"]), titles(&reversed));
    assert_eq!(
        vec![section(Some("Home"), vec![rows[0].clone(), rows[1].clone()])],
        C::sections(&rows[..2], id, &map(&[("M1", "Home"), ("M2", "Home")]))
    );
}

#[test]
fn keepable_addresses() {
    for h in [
        "192.168.1.20", "100.101.102.103", "mac.tail1234.ts.net", "fearless.ts.net", "fe80.example.com", "2001:db8::1",
        "fd7a:115c:a1e0::1", "[2001:db8::2]", "::ffff:192.168.1.2", "febz.local",
    ] {
        assert!(SavedMacs::keepable(h, false), "{h}");
    }
    for h in [
        "", "169.254.10.1", "fe80::1", "FE80::1", "fe9f::1", "febf::1", "fe80::1%wlan0", "127.0.0.1", "::1", "localhost",
        "::ffff:169.254.1.1", "[fe80::2]",
    ] {
        assert!(!SavedMacs::keepable(h, false), "{h}");
    }
    assert!(SavedMacs::keepable("127.0.0.1", true));
    assert!(SavedMacs::keepable("::1", true));
    assert!(SavedMacs::keepable("fec0::1", false));
}
