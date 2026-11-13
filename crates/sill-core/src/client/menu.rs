//! One connection's view of the streamed app's menu bar (iOSClient/MacMenuState.swift): the top
//! level the Mac sends after this device's subscription, each menu's items fetched when it opens
//! (with the title it was shown under), and choices. A new version of the tree settles whatever
//! waits with "The menus changed…". Pure: the client sends what it returns and keeps the clock.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};

use crate::protocol::{kind, MacMenu, MacMenuItem};

/// One menu opened on this version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Menu {
    Loading,
    Items { items: Vec<MacMenuItem>, more: Option<i64>, stale: bool, note: Option<String> },
    Failed(String),
}

/// What to send: kind 27 (fetch) or 25 (press).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuRequest {
    pub kind: u8,
    pub token: i64,
    pub version: Option<i64>,
    pub id: Option<String>,
    pub title: Option<String>,
}

/// What a kind 24 was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuOutcome {
    TopLevel,
    MenuItems,
    Pressed { ok: bool, note: Option<String> },
    Ignored,
}

static NEXT_TOKEN: AtomicI64 = AtomicI64::new(1);

#[derive(Debug, Default)]
pub struct MenuState {
    version: Option<i64>,
    app: Option<String>,
    top: Vec<MacMenuItem>,
    top_note: Option<String>,
    top_stale: bool,
    /// Menus opened on this version, by id.
    menus: BTreeMap<String, Menu>,
    /// token → (menu id, asked at)
    waiting: HashMap<i64, (String, u64)>,
    /// token → asked at
    choices: HashMap<i64, u64>,
}

impl MenuState {
    pub const CHANGED: &'static str = "The menus changed. Open the menu again.";

    pub fn new() -> MenuState {
        MenuState::default()
    }

    pub fn version(&self) -> Option<i64> {
        self.version
    }
    pub fn app(&self) -> Option<&str> {
        self.app.as_deref()
    }
    pub fn top(&self) -> &[MacMenuItem] {
        &self.top
    }
    pub fn top_note(&self) -> Option<&str> {
        self.top_note.as_deref()
    }
    pub fn top_stale(&self) -> bool {
        self.top_stale
    }
    pub fn menus(&self) -> &BTreeMap<String, Menu> {
        &self.menus
    }

    /// The subscription to the top level.
    pub fn subscribe(&mut self) -> MenuRequest {
        MenuRequest { kind: kind::FETCH_MENU, token: NEXT_TOKEN.fetch_add(1, Ordering::SeqCst), version: None, id: None, title: None }
    }

    /// Ask for one menu's items; `None` when it is already loading. `now`: monotonic ms.
    pub fn open(&mut self, item: &MacMenuItem, now: u64) -> Option<MenuRequest> {
        let id = item.id.clone()?;
        if self.menus.get(&id) == Some(&Menu::Loading) && self.waiting.values().any(|w| w.0 == id) {
            return None;
        }
        self.menus.insert(id.clone(), Menu::Loading);
        let token = NEXT_TOKEN.fetch_add(1, Ordering::SeqCst);
        self.waiting.insert(token, (id.clone(), now));
        Some(MenuRequest { kind: kind::FETCH_MENU, token, version: self.version, id: Some(id), title: item.title.clone() })
    }

    pub fn choose(&mut self, item: &MacMenuItem, now: u64) -> Option<MenuRequest> {
        let id = item.id.clone()?;
        if !item.enabled || item.separator || item.submenu || self.choices.len() >= 16 {
            return None;
        }
        let token = NEXT_TOKEN.fetch_add(1, Ordering::SeqCst);
        self.choices.insert(token, now);
        Some(MenuRequest { kind: kind::PRESS_MENU_ITEM, token, version: self.version, id: Some(id), title: item.title.clone() })
    }

    pub fn receive(&mut self, m: &MacMenu) -> MenuOutcome {
        let unknown = |a: i64| !self.waiting.contains_key(&a) && !self.choices.contains_key(&a);
        let Some(answering) = m.answering.filter(|a| !(m.menus.is_some() && unknown(*a))) else {
            let Some(menus) = &m.menus else { return MenuOutcome::Ignored };
            if m.version != self.version {
                // A new tree: every menu shown or asked for under the old one says so.
                for menu in self.menus.values_mut() {
                    *menu = Menu::Failed(Self::CHANGED.to_string());
                }
                self.waiting.clear();
            }
            self.version = m.version;
            self.app = m.app.clone();
            self.top = menus.clone();
            self.top_stale = m.stale;
            self.top_note = m.note.clone();
            return MenuOutcome::TopLevel;
        };
        if let Some((id, _)) = self.waiting.remove(&answering) {
            let menu = match (&m.items, m.version, self.version) {
                // An answer from another tree than the one the menu was asked under.
                (_, Some(v), Some(mine)) if v != mine => Menu::Failed(Self::CHANGED.to_string()),
                (Some(items), _, _) => Menu::Items { items: items.clone(), more: m.more, stale: m.stale, note: m.note.clone() },
                (None, _, _) => Menu::Failed(m.note.clone().unwrap_or_else(|| Self::CHANGED.to_string())),
            };
            self.menus.insert(id, menu);
            return MenuOutcome::MenuItems;
        }
        if self.choices.remove(&answering).is_some() {
            return MenuOutcome::Pressed { ok: m.pressed == Some(true), note: m.note.clone() };
        }
        MenuOutcome::Ignored
    }

    /// Requests unanswered past the timeout settle as failed; whether anything changed.
    pub fn expire(&mut self, now: u64, timeout_ms: u64) -> bool {
        let mut changed = false;
        let menus = &mut self.menus;
        self.waiting.retain(|_, (id, at)| {
            if now.saturating_sub(*at) > timeout_ms {
                menus.insert(id.clone(), Menu::Failed("The Mac didn’t answer in time.".to_string()));
                changed = true;
                false
            } else {
                true
            }
        });
        self.choices.retain(|_, at| now.saturating_sub(*at) <= timeout_ms);
        changed
    }

    pub fn reset(&mut self) {
        *self = MenuState::default();
    }
}
