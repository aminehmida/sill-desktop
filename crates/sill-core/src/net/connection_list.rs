//! The connect screen's arrangement (rows, or tiles with each saved Mac's last picture), the
//! groups a saved Mac can be put in, and the stored pictures (iOSClient/ConnectionList.swift).
//! Pure rules.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// Rows or tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    List,
    Tiles,
}

impl Style {
    pub fn raw(self) -> &'static str {
        match self {
            Style::List => "list",
            Style::Tiles => "tiles",
        }
    }

    pub fn toggled(self) -> Style {
        if self == Style::List { Style::Tiles } else { Style::List }
    }
}

/// What a launch or a change of style does to the tiles' pictures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewPlan {
    /// File names to delete: only those of Macs no longer saved.
    pub delete: Vec<String>,
    /// The saved Macs whose kept picture shows, in order.
    pub show: Vec<String>,
}

/// One section of the connect screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section<T> {
    pub title: Option<String>,
    pub items: Vec<T>,
}

fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

pub struct ConnectionList;

impl ConnectionList {
    pub const STYLE_KEY: &'static str = "connectionStyle";
    pub const GROUPS_KEY: &'static str = "macGroups";
    pub const PRESET_GROUPS: [&'static str; 3] = ["Home", "Work", "Personal"];
    /// The section of the Macs with no group, once any Mac has one. Never a group's own name.
    pub const OTHER_TITLE: &'static str = "Other";
    pub const MAX_GROUP_LENGTH: usize = 32;
    /// Two tile columns from this content width on (points).
    pub const TWO_COLUMN_MIN_WIDTH: f32 = 340.0;
    /// The arrangement of an install whose person never chose one.
    pub const DEFAULT_STYLE: Style = Style::Tiles;
    /// The picture's width at most, in points, and its scale: 640 pixels at most.
    pub const PREVIEW_POINTS: u32 = 320;
    pub const PREVIEW_SCALE: u32 = 2;
    /// The brightest a colour channel may be in a picture that counts as black.
    pub const BLACK_LEVEL: u32 = 24;

    /// The style to show: the stored choice, "list" or "tiles", else the default.
    pub fn stored_style(raw: Option<&str>) -> Style {
        match raw {
            Some("list") => Style::List,
            Some("tiles") => Style::Tiles,
            _ => Self::DEFAULT_STYLE,
        }
    }

    /// Whether a session's end keeps a picture of the Mac's screen for its tile: only while tiles
    /// are on, only from the app on screen, and only for a saved Mac.
    pub fn takes_preview(style: Style, foreground: bool, saved: bool) -> bool {
        style == Style::Tiles && foreground && saved
    }

    /// Whether a picture taken as a session ended is kept when it lands.
    pub fn keeps_decoded_picture(style: Style, saved: bool) -> bool {
        style == Style::Tiles && saved
    }

    /// A style never deletes a picture: with tiles every saved Mac with a file shows its picture;
    /// with the list none shows, and every file stays. Either way the files of Macs no longer
    /// saved go. `files`: the names in the pictures' folder.
    pub fn preview_plan(style: Style, files: &[String], saved_mac_ids: &BTreeSet<String>) -> PreviewPlan {
        let delete = Self::stale_preview_files(files, saved_mac_ids);
        if style != Style::Tiles {
            return PreviewPlan { delete, show: Vec::new() };
        }
        let mut show: Vec<String> = files
            .iter()
            .filter_map(|f| {
                let id = f.strip_suffix(".jpg")?;
                (saved_mac_ids.contains(id) && Self::preview_file_name(id).as_deref() == Some(f.as_str())).then(|| id.to_string())
            })
            .collect();
        show.sort();
        PreviewPlan { delete, show }
    }

    /// The picture's size in pixels for a video of `width` × `height` pixels: at most 640 wide,
    /// never wider than the video, in its shape, the height rounded down (at least a pixel).
    pub fn preview_pixels(width: i32, height: i32) -> Option<(i32, i32)> {
        if width < 1 || height < 1 {
            return None;
        }
        let w = ((Self::PREVIEW_POINTS * Self::PREVIEW_SCALE) as i32).min(width);
        let h = ((i64::from(height) * i64::from(w) / i64::from(width)) as i32).max(1);
        Some((w, h))
    }

    /// Whether a picture is black: no pixel with a colour channel brighter than `BLACK_LEVEL`.
    /// `pixels`: ARGB, `width` × `height` (alpha is not looked at).
    pub fn looks_black(pixels: &[u32], width: i32, height: i32) -> bool {
        if width <= 0 || height <= 0 || (pixels.len() as i64) < i64::from(width) * i64::from(height) {
            return true;
        }
        pixels[..(width * height) as usize]
            .iter()
            .all(|p| (p >> 16 & 0xFF) <= Self::BLACK_LEVEL && (p >> 8 & 0xFF) <= Self::BLACK_LEVEL && (p & 0xFF) <= Self::BLACK_LEVEL)
    }

    /// Safe file name for a saved Mac's picture (`<macID>.jpg`); `None` for anything that could
    /// traverse paths or is not a valid identifier.
    pub fn preview_file_name(mac_id: &str) -> Option<String> {
        if mac_id.is_empty() || mac_id.len() > 128 {
            return None;
        }
        if !mac_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return None;
        }
        Some(format!("{mac_id}.jpg"))
    }

    /// `.jpg` names in the pictures' folder whose Mac ID is not in `keeping`.
    pub fn stale_preview_files(files: &[String], keeping: &BTreeSet<String>) -> Vec<String> {
        files.iter().filter(|f| f.strip_suffix(".jpg").map(|id| !keeping.contains(id)).unwrap_or(false)).cloned().collect()
    }

    /// How many tile columns fit `width`: 2 from 340 points on, 1 on narrower screens or at the
    /// largest text sizes.
    pub fn columns(width: f32, accessible_text: bool) -> usize {
        if !accessible_text && width >= Self::TWO_COLUMN_MIN_WIDTH { 2 } else { 1 }
    }

    // MARK: Groups

    /// Cleans a group name: whitespace collapsed to single spaces, at most `MAX_GROUP_LENGTH`
    /// characters; a preset's name in any case is the preset ("work" is Work); `None` when blank
    /// or Other in any case.
    pub fn clean_group(raw: Option<&str>) -> Option<String> {
        let words: Vec<&str> = raw?.split_whitespace().collect();
        if words.is_empty() {
            return None;
        }
        let joined = words.join(" ");
        let name: String = joined.chars().take(Self::MAX_GROUP_LENGTH).collect::<String>().trim().to_string();
        if name.is_empty() || eq_ignore_case(&name, Self::OTHER_TITLE) {
            return None;
        }
        Some(Self::PRESET_GROUPS.iter().find(|p| eq_ignore_case(p, &name)).map(|p| p.to_string()).unwrap_or(name))
    }

    pub fn decode_groups(text: Option<&str>) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(Ok(Value::Object(o))) = text.map(serde_json::from_str::<Value>) else { return out };
        for (mac_id, v) in o {
            if mac_id.is_empty() || mac_id.len() > 128 {
                continue;
            }
            if let Some(g) = Self::clean_group(v.as_str()) {
                out.insert(mac_id, g);
            }
        }
        out
    }

    pub fn encode_groups(groups: &BTreeMap<String, String>) -> String {
        serde_json::to_string(groups).unwrap_or_else(|_| "{}".to_string())
    }

    /// Puts `mac_id` in `group`, or in none for `None`, blank or Other. A name already in use in
    /// another case ("home office" beside "Home Office") is that group.
    pub fn setting_group(group: Option<&str>, mac_id: &str, groups: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut next = groups.clone();
        if mac_id.is_empty() || mac_id.len() > 128 {
            return next;
        }
        match Self::clean_group(group) {
            Some(clean) => {
                let existing = groups.values().filter(|g| eq_ignore_case(g, &clean)).min().cloned();
                next.insert(mac_id.to_string(), existing.unwrap_or(clean));
            }
            None => {
                next.remove(mac_id);
            }
        }
        next
    }

    /// A saved record retired for a newer one of the same Mac gives its group to that one, unless
    /// it has its own.
    pub fn carrying_group(old: &str, new: &str, groups: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut next = groups.clone();
        if old == new {
            return next;
        }
        if let Some(group) = next.remove(old) {
            next.entry(new.to_string()).or_insert(group);
        }
        next
    }

    /// Only the groups of Macs still saved.
    pub fn keeping_saved(groups: &BTreeMap<String, String>, saved: &BTreeSet<String>) -> BTreeMap<String, String> {
        groups.iter().filter(|(k, _)| saved.contains(*k)).map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// The order groups come in: the presets in their order, then other names as Finder sorts
    /// them (case-insensitive, numbers by value).
    pub fn ordered(names: &BTreeSet<String>) -> Vec<String> {
        let mut out: Vec<String> = Self::PRESET_GROUPS.iter().filter(|p| names.contains(**p)).map(|p| p.to_string()).collect();
        let mut rest: Vec<String> = names.iter().filter(|n| !Self::PRESET_GROUPS.contains(&n.as_str())).cloned().collect();
        rest.sort_by(|a, b| Self::finder_compare(a, b).then_with(|| a.cmp(b)));
        out.extend(rest);
        out
    }

    /// The Group menu's names: every preset, then the other names in use.
    pub fn available_groups(groups: &BTreeMap<String, String>) -> Vec<String> {
        let mut names: BTreeSet<String> = Self::PRESET_GROUPS.iter().map(|p| p.to_string()).collect();
        names.extend(groups.values().filter_map(|g| Self::clean_group(Some(g))));
        Self::ordered(&names)
    }

    /// Groups `items` by each item's Mac ID in `groups`: the groups in `ordered`'s order, each
    /// keeping the items' own order, then Other for the rest. When no item has a group, a single
    /// section with no title.
    pub fn sections<T: Clone>(items: &[T], mac_id: impl Fn(&T) -> Option<String>, groups: &BTreeMap<String, String>) -> Vec<Section<T>> {
        if items.is_empty() {
            return Vec::new();
        }
        let mut by_group: BTreeMap<String, Vec<T>> = BTreeMap::new();
        let mut ungrouped = Vec::new();
        for item in items {
            match mac_id(item).and_then(|id| Self::clean_group(groups.get(&id).map(String::as_str))) {
                Some(g) => by_group.entry(g).or_default().push(item.clone()),
                None => ungrouped.push(item.clone()),
            }
        }
        if by_group.is_empty() {
            return vec![Section { title: None, items: items.to_vec() }];
        }
        let names: BTreeSet<String> = by_group.keys().cloned().collect();
        let mut out: Vec<Section<T>> =
            Self::ordered(&names).into_iter().filter_map(|n| by_group.remove(&n).map(|items| Section { title: Some(n), items })).collect();
        if !ungrouped.is_empty() {
            out.push(Section { title: Some(Self::OTHER_TITLE.to_string()), items: ungrouped });
        }
        out
    }

    /// Finder's order (`localizedStandardCompare`'s spirit): runs of digits by value, the rest
    /// ignoring case.
    pub fn finder_compare(a: &str, b: &str) -> Ordering {
        let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
                let (si, sj) = (i, j);
                while i < a.len() && a[i].is_ascii_digit() {
                    i += 1;
                }
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                let na: String = a[si..i].iter().collect::<String>().trim_start_matches('0').to_string();
                let nb: String = b[sj..j].iter().collect::<String>().trim_start_matches('0').to_string();
                if na.len() != nb.len() {
                    return na.len().cmp(&nb.len());
                }
                let c = na.cmp(&nb);
                if c != Ordering::Equal {
                    return c;
                }
            } else {
                let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
                let c = lower(a[i]).cmp(&lower(b[j]));
                if c != Ordering::Equal {
                    return c;
                }
                i += 1;
                j += 1;
            }
        }
        (a.len() - i).cmp(&(b.len() - j))
    }
}
