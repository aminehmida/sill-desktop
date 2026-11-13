//! What the window shows, after Sill's own apps: the list of Macs to connect to, and a Mac's
//! stream under the bar (Apps, the windows' thumbnails, Menus, Aa, Desktop, Settings). A window
//! on its own fills this window edge to edge, as if the app ran here: the bar comes down when the
//! pointer rests at the top edge.

use crate::keymap::ModifierLayout;
use crate::pipeline::DecodeStats;
use crate::theme::{self, Glyph, Icon, ACCENT, BAR, BAR_HEIGHT, CONTROL, MUTED, TEXT};
use crate::video_view::VideoPaint;
use egui::{pos2, vec2, Align, Align2, Color32, Layout, Pos2, Rect, Response, RichText, Sense, Stroke, StrokeKind};
use sill_core::client::Menu;
use sill_core::protocol::{window_actions, LockedMac, MacMenuItem, QualityPreset, SettingsChange, StreamSource, TextScale};
use sill_core::{Client, ClientState, Phase};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Open {
    #[default]
    Nothing,
    Apps,
    Settings,
    TextSize,
}

pub struct Ui {
    pub me: crate::this_computer::ThisComputer,
    /// The address typed is this computer's own.
    typed_self: bool,
    // The list of Macs.
    address: String,
    code: String,
    home_code: String,
    adding: bool,
    // The stream.
    open: Open,
    search: String,
    bar_visible: bool,
    top_since: Option<Instant>,
    away_since: Option<Instant>,
    menu_asked: Option<String>,
    menus_open: bool,
    bar_forced: bool,
    last_pointer: Option<Pos2>,
    handle_clicked: bool,
    hint_until: Option<Instant>,
    hinted: bool,
    icons: HashMap<String, egui::TextureHandle>,
    thumbs: HashMap<u32, (usize, egui::TextureHandle)>,
    /// Where the picture is, in points, and whether the pointer is on it (nothing over it).
    pub video_rect: Option<Rect>,
    pub video_hovered: bool,
    /// The stream panel's size in points: what the Mac is asked to size the window to.
    pub panel_size: Option<egui::Vec2>,
    /// A window alone fills this window: the bar is out of the way.
    pub immersive: bool,
    pub layout: ModifierLayout,
    /// The bar stays even over a window.
    pub keep_bar: bool,
    /// The text size picked with Aa, kept between sessions: nil until one is picked, then a scale or Off.
    pub text_scale: Option<Option<f64>>,
    pub prefs_changed: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            me: crate::this_computer::ThisComputer::new(),
            typed_self: false,
            address: String::new(),
            code: String::new(),
            home_code: String::new(),
            adding: false,
            open: Open::Nothing,
            search: String::new(),
            bar_visible: false,
            top_since: None,
            away_since: None,
            menu_asked: None,
            menus_open: false,
            bar_forced: false,
            last_pointer: None,
            handle_clicked: false,
            hint_until: None,
            hinted: false,
            icons: HashMap::new(),
            thumbs: HashMap::new(),
            video_rect: None,
            video_hovered: false,
            panel_size: None,
            immersive: false,
            layout: ModifierLayout::default(),
            keep_bar: false,
            text_scale: None,
            prefs_changed: false,
        }
    }
}

pub struct Frame<'a> {
    pub client: &'a Client,
    pub state: &'a ClientState,
    pub video_size: Option<(u32, u32)>,
    pub decode: &'a DecodeStats,
    pub drawn_fps: f32,
}

const SITE: &str = "getsill.app";
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

fn clean_code(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).take(12).collect()
}

fn group_code(s: &str) -> String {
    let digits = clean_code(s);
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn short(text: &str, most: usize) -> String {
    if text.chars().count() > most {
        text.chars().take(most - 1).chain(['…']).collect()
    } else {
        text.to_string()
    }
}

/// The 48-point field of the pairing cards.
fn pairing_field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> Response {
    egui::Frame::NONE
        .fill(CONTROL)
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(14, 0))
        .show(ui, |ui| {
            let width = ui.available_width();
            ui.add_sized(
                vec2(width, 48.0),
                egui::TextEdit::singleline(text)
                    .frame(egui::Frame::NONE)
                    .font(theme::regular(15.0))
                    .text_color(TEXT)
                    .align(egui::Align2::LEFT_CENTER)
                    .hint_text(RichText::new(hint).color(MUTED).font(theme::regular(15.0))),
            )
        })
        .inner
}

fn problem(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(format!("⚠ {text}")).font(theme::regular(13.0)).color(theme::WARNING));
}

/// A two-letter tile for an app without an icon, or the icon itself.
fn app_icon(ui: &egui::Ui, rect: Rect, texture: Option<&egui::TextureHandle>, name: &str) {
    match texture {
        Some(t) => {
            egui::Image::new(t).corner_radius(rect.width() * 0.29).paint_at(ui, rect);
        }
        None => {
            ui.painter().rect_filled(rect, rect.width() * 0.29, theme::ICON_FALLBACK);
            let letters: String = name.chars().filter(|c| c.is_alphanumeric()).take(2).collect();
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, letters, theme::semibold((rect.width() * 0.42).round()), Color32::WHITE);
        }
    }
}

enum Lead<'a> {
    Mac,
    App(Option<&'a egui::TextureHandle>, &'a str),
}

/// A row of a drawer or of the list of Macs: a 32-point icon, a title, a word at the far end.
/// A Mac that is this computer: listed, so nobody wonders where it went, and not to be picked.
fn own_row(ui: &mut egui::Ui, name: &str) {
    ui.scope(|ui| {
        ui.multiply_opacity(0.45);
        ui.add_enabled_ui(false, |ui| drawer_row(ui, Lead::Mac, name, "This Mac", false, 50.0));
    })
    .response
    .on_hover_text("This is the Mac you are using. Sill connects to another one.");
}

fn drawer_row(ui: &mut egui::Ui, lead: Lead, title: &str, trailing: &str, lit: bool, height: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    // The row's own 10-point inset, around what was allocated: titles line up with the text above.
    let rect = rect.expand2(vec2(if ui.data(|d| d.get_temp::<bool>(egui::Id::NULL).unwrap_or(false)) { 10.0 } else { 0.0 }, 0.0));
    if lit {
        ui.painter().rect_filled(rect, 10.0, CONTROL);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 10.0, CONTROL.gamma_multiply(0.6));
    }
    let icon = Rect::from_center_size(pos2(rect.left() + 10.0 + 16.0, rect.center().y), vec2(32.0, 32.0));
    match lead {
        Lead::Mac => {
            ui.painter().rect_filled(icon, 9.0, theme::ICON_FALLBACK);
            theme::icon(ui.painter(), Icon::Display, icon.center(), 17.0, Color32::WHITE);
        }
        Lead::App(texture, name) => app_icon(ui, icon, texture, name),
    }
    let far = ui.painter().text(pos2(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, trailing, theme::regular(13.0), MUTED);
    let room = (far.left() - 12.0 - (icon.right() + 12.0)).max(20.0);
    let galley = ui.painter().layout(title.to_string(), theme::regular(15.0), TEXT, f32::INFINITY);
    let galley = if galley.size().x > room {
        let keep = ((title.chars().count() as f32 * room / galley.size().x) as usize).max(4);
        ui.painter().layout_no_wrap(short(title, keep), theme::regular(15.0), TEXT)
    } else {
        galley
    };
    ui.painter().galley(pos2(icon.right() + 12.0, rect.center().y - galley.size().y / 2.0), galley, TEXT);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn box_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(BAR)
        .corner_radius(20.0)
        .stroke(Stroke::new(1.0, theme::white(0.10)))
        .inner_margin(14.0)
        .shadow(egui::Shadow { offset: [0, 24], blur: 30, spread: 0, color: Color32::from_black_alpha(166) })
}

impl Ui {
    pub fn show(&mut self, root: &mut egui::Ui, f: &Frame) {
        self.video_rect = None;
        self.video_hovered = false;
        self.panel_size = None;
        self.immersive = false;
        let full = root.max_rect();
        root.painter().rect_filled(full, 0.0, Color32::BLACK);
        match &f.state.phase {
            Phase::Streaming => self.stream(root, f),
            Phase::Connecting { mac_name, reconnecting } => {
                self.reset_stream();
                self.connecting(root, f, mac_name, *reconnecting)
            }
            Phase::Idle => {
                self.reset_stream();
                self.macs(root, f)
            }
        }
    }

    /// The bench's hand on the window: what a person would open with the pointer.
    pub fn script(&mut self, what: &str) {
        match what {
            "apps" => self.open = Open::Apps,
            "settings" => self.open = Open::Settings,
            "textsize" => self.open = Open::TextSize,
            "bar" => self.bar_forced = true,
            "add" => self.adding = true,
            _ => {
                self.open = Open::Nothing;
                self.bar_forced = false;
                self.adding = false;
            }
        }
    }

    fn reset_stream(&mut self) {
        self.open = Open::Nothing;
        self.bar_visible = false;
        self.search.clear();
        self.thumbs.clear();
        self.hinted = false;
        self.hint_until = None;
    }

    /// The window's title: the Mac window's own while it streams alone.
    pub fn title(&self, s: &ClientState) -> String {
        if s.phase != Phase::Streaming {
            return "Sill".to_string();
        }
        match s.active {
            StreamSource::Window(id) => match s.windows.iter().find(|w| w.id == id) {
                Some(w) if w.title.is_empty() || w.title == w.app_name => w.app_name.clone(),
                Some(w) => format!("{} — {}", w.title, w.app_name),
                None => s.mac_name.clone(),
            },
            StreamSource::Desktop => format!("{} — Desktop", s.mac_name),
            StreamSource::None => s.mac_name.clone(),
        }
    }

    // MARK: the list of Macs

    fn column<R>(&mut self, root: &mut egui::Ui, id: &str, add: impl FnOnce(&mut Self, &mut egui::Ui) -> R) {
        let full = root.max_rect();
        let width = if full.width() < 412.0 { (full.width() - 32.0).max(200.0) } else { 380.0 };
        // Centered once its height is known: laid out off screen first.
        let key = egui::Id::unique(("column height", id));
        let last: f32 = root.ctx().data(|d| d.get_temp(key)).unwrap_or(260.0);
        let room = full.height() - 72.0;
        let top = if last + 16.0 > room { 16.0 } else { ((full.height() - last) / 2.0 - 20.0).max(16.0) };
        let rect = Rect::from_min_size(pos2(full.center().x - width / 2.0, full.top() + top), vec2(width, (room - top).max(100.0)));
        let mut ui = root.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::LEFT)));
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(&mut ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let before = ui.cursor().top();
            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 0)).show(ui, |ui| add(self, ui));
            let height = ui.cursor().top() - before;
            ui.ctx().data_mut(|d| d.insert_temp(key, height));
        });
        // The footer.
        let foot = Rect::from_min_max(pos2(rect.left() + 10.0, full.bottom() - 62.0), pos2(rect.right() - 10.0, full.bottom() - 14.0));
        let mut ui = root.new_child(egui::UiBuilder::new().max_rect(foot).layout(Layout::top_down(Align::LEFT)));
        ui.spacing_mut().item_spacing = vec2(0.0, 6.0);
        // The Mac's app is Sill's own, so its download is theirs; this client is not, so its
        // source and its issues are this project's, not Sill's support or privacy pages.
        let line = |ui: &mut egui::Ui, text: &str, link: &str, url: &str| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).font(theme::regular(13.0)).color(MUTED));
                if ui.add(egui::Label::new(RichText::new(link).font(theme::regular(13.0)).color(ACCENT)).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            });
        };
        line(&mut ui, "Needs the free Sill app on your Mac. ", &format!("Get it at {SITE}"), &format!("https://{SITE}/download"));
        line(&mut ui, "An unofficial client, not made by Sill. ", "Source and Issues", REPOSITORY);
    }

    fn connecting(&mut self, root: &mut egui::Ui, f: &Frame, mac: &str, reconnecting: bool) {
        let status = f.state.status.clone();
        let client = f.client.clone();
        let mac = mac.to_string();
        self.column(root, "connecting", move |_, ui| {
            ui.data_mut(|d| d.insert_temp(egui::Id::NULL, true));
            ui.label(RichText::new("Connect to a Mac").font(theme::semibold(17.0)).color(TEXT));
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0).color(MUTED));
                let line = status.unwrap_or_else(|| if reconnecting { format!("Reconnecting to {mac}…") } else { format!("Connecting to {mac}…") });
                ui.label(RichText::new(line).font(theme::regular(13.0)).color(MUTED));
            });
            drawer_row(ui, Lead::Mac, &mac, "", true, 50.0);
            ui.add_space(6.0);
            if theme::link(ui, "Cancel").clicked() {
                client.disconnect();
            }
        });
    }

    fn macs(&mut self, root: &mut egui::Ui, f: &Frame) {
        let s = f.state;
        let client = f.client;
        if s.home_ask.is_some() {
            self.adding = false;
        }
        self.column(root, "macs", |me, ui| {
            ui.data_mut(|d| d.insert_temp(egui::Id::NULL, true));
            let title = match (&s.home_ask, me.adding) {
                (Some(ask), _) => format!("Pair with {}", ask.found.service_name),
                (None, true) => "Add a Mac".to_string(),
                _ => "Connect to a Mac".to_string(),
            };
            ui.label(RichText::new(title).font(theme::semibold(17.0)).color(TEXT));
            if let Some(ask) = &s.home_ask {
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{} is showing a code. Type it here.", ask.found.service_name)).font(theme::regular(13.0)).color(TEXT));
                ui.add_space(4.0);
                let shown = group_code(&me.home_code);
                let mut text = shown.clone();
                let field = pairing_field(ui, &mut text, "0000 0000 0000");
                if text != shown {
                    me.home_code = clean_code(&text);
                }
                field.request_focus();
                let code = clean_code(&me.home_code);
                if let Some(e) = &s.pair_error {
                    problem(ui, e);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 18.0;
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (theme::primary_button(ui, "Pair", code.len() == 12 && !s.pairing).clicked() || enter) && code.len() == 12 {
                        client.pair_home_code(&code);
                    }
                    if theme::link(ui, "Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        me.home_code.clear();
                        client.cancel_home_ask();
                    }
                    if s.pairing {
                        ui.add(egui::Spinner::new().size(14.0).color(MUTED));
                    }
                });
                return;
            }
            if me.adding {
                me.add_card(ui, f);
                return;
            }
            let status = match (&s.status, &s.asking) {
                (Some(t), _) => t.clone(),
                (None, Some(mac)) => format!("Pairing with {mac}…"),
                (None, None) if s.saved_macs.is_empty() && s.unpaired_home_macs.is_empty() => "Looking for Macs on this network…".to_string(),
                (None, None) => "Looking for Macs…".to_string(),
            };
            ui.label(RichText::new(status).font(theme::regular(13.0)).color(MUTED));
            if let Some(e) = &s.last_error {
                if s.status.as_deref() != Some(e.as_str()) {
                    problem(ui, e);
                }
            }
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            // Rows keep the column's full width: the 10-point inset is the row's own.
            me.me.refresh();
            // This computer's own Sill goes last, apart: the list leads with what can be picked.
            let mut own_names: Vec<&str> = Vec::new();
            for mac in &s.saved_macs {
                let own = mac.addresses.iter().any(|a| me.me.is(&a.host)) || mac.typed_addresses.iter().any(|a| me.me.is(a));
                if own {
                    own_names.push(&mac.name);
                    continue;
                }
                let route = if s.saved_macs_at_home.contains(&mac.mac_id) { "Wi-Fi" } else { "Remote" };
                let row = drawer_row(ui, Lead::Mac, &mac.name, route, false, 50.0);
                if row.clicked() {
                    client.connect(&mac.mac_id);
                }
                row.context_menu(|ui| {
                    if ui.button("Connect").clicked() {
                        client.connect(&mac.mac_id);
                        ui.close();
                    }
                    if ui.button(RichText::new(format!("Forget {}", mac.name)).color(theme::ERROR)).clicked() {
                        client.forget(&mac.mac_id);
                        ui.close();
                    }
                });
            }
            for mac in &s.unpaired_home_macs {
                if me.me.is(&mac.host) {
                    if !own_names.contains(&mac.service_name.as_str()) {
                        own_names.push(&mac.service_name);
                    }
                    continue;
                }
                let asking = s.asking.as_deref() == Some(mac.service_name.as_str());
                if drawer_row(ui, Lead::Mac, &mac.service_name, "Not paired", asking, 50.0).clicked() {
                    me.home_code.clear();
                    client.connect_home(&mac.service_name);
                }
            }
            ui.add_space(8.0);
            let add = ui.add(egui::Label::new(RichText::new("Add a Mac…").font(theme::semibold(15.0)).color(ACCENT)).sense(Sense::click()));
            if add.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                me.adding = true;
                client.clear_pair_error();
            }
            // This computer's own Sill: under everything that can be picked, apart.
            if !own_names.is_empty() {
                ui.add_space(14.0);
                for name in own_names {
                    own_row(ui, name);
                }
            }
        });
    }

    fn add_card(&mut self, ui: &mut egui::Ui, f: &Frame) {
        let s = f.state;
        ui.add_space(4.0);
        ui.label(RichText::new("Pair once, and Sill reaches your Mac from anywhere, through your VPN or the internet.").font(theme::regular(13.0)).color(MUTED));
        ui.label(RichText::new("On your Mac, choose Pair iPhone or iPad… in the Sill menu. Type the address and code that Sill shows there.").font(theme::regular(13.0)).color(TEXT));
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 8.0;
        pairing_field(ui, &mut self.address, "100.101.102.103 or mac.example.net");
        let shown = group_code(&self.code);
        let mut text = shown.clone();
        pairing_field(ui, &mut text, "0000 0000 0000");
        if text != shown {
            self.code = clean_code(&text);
        }
        if self.typed_self {
            problem(ui, "That address is this Mac. Type the address of another Mac.");
        } else if let Some(e) = &s.pair_error {
            problem(ui, e);
        }
        if s.pairing {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0).color(MUTED));
                ui.label(RichText::new("Pairing…").font(theme::regular(13.0)).color(MUTED));
            });
        }
        let code = clean_code(&self.code);
        let ready = code.len() == 12 && !self.address.trim().is_empty() && !s.pairing;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (theme::primary_button(ui, "Pair", ready).clicked() || enter) && ready {
                f.client.clear_pair_error();
                self.me.refresh();
                self.typed_self = self.me.is_typed(self.address.trim());
                if !self.typed_self {
                    f.client.pair_with_code(self.address.trim(), &code);
                }
            }
            if theme::link(ui, "Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.adding = false;
                self.typed_self = false;
                self.code.clear();
                f.client.clear_pair_error();
            }
        });
    }

    // MARK: the stream

    fn icon(&mut self, ctx: &egui::Context, s: &ClientState, bundle: &str) -> Option<egui::TextureHandle> {
        if let Some(t) = self.icons.get(bundle) {
            return Some(t.clone());
        }
        let png = s.icons.get(bundle)?;
        let img = image::load_from_memory(png).ok()?.to_rgba8();
        let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img);
        let t = ctx.load_texture(format!("icon {bundle}"), color, egui::TextureOptions::LINEAR);
        self.icons.insert(bundle.to_string(), t.clone());
        Some(t)
    }

    fn thumb(&mut self, ctx: &egui::Context, s: &ClientState, id: u32) -> Option<egui::TextureHandle> {
        let jpeg = s.thumbnails.get(&id)?;
        let key = std::sync::Arc::as_ptr(jpeg) as usize;
        if let Some((k, t)) = self.thumbs.get(&id) {
            if *k == key {
                return Some(t.clone());
            }
        }
        let img = image::load_from_memory(jpeg).ok()?.to_rgba8();
        let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img);
        let t = ctx.load_texture(format!("thumb {id}"), color, egui::TextureOptions::LINEAR);
        self.thumbs.insert(id, (key, t.clone()));
        Some(t)
    }

    fn toggle(&mut self, what: Open) {
        self.open = if self.open == what { Open::Nothing } else { what };
        self.search.clear();
    }

    fn stream(&mut self, root: &mut egui::Ui, f: &Frame) {
        let s = f.state;
        let ctx = root.ctx().clone();
        let full = root.max_rect();
        let now = Instant::now();
        let window_alone = matches!(s.active, StreamSource::Window(_)) && s.lock_shade.is_none();
        self.immersive = window_alone && !self.keep_bar;

        // The bar: always there over the Desktop; over a window alone, while the pointer rests
        // at the top edge or is on it, or something of it is open.
        let pointer = ctx.pointer_latest_pos().filter(|_| ctx.input(|i| i.pointer.has_pointer()));
        let busy = self.open != Open::Nothing || self.menus_open || self.bar_forced;
        if self.bar_forced {
            self.bar_visible = true;
        }
        if !self.immersive {
            self.bar_visible = true;
            self.top_since = None;
            self.away_since = None;
        } else if self.bar_visible {
            let on_bar = pointer.is_some_and(|p| p.y <= full.top() + BAR_HEIGHT + 10.0 && full.contains(p));
            if on_bar || busy {
                self.away_since = None;
            } else if now.duration_since(*self.away_since.get_or_insert(now)) > Duration::from_millis(450) {
                self.bar_visible = false;
                self.away_since = None;
            }
        } else {
            // Resting in the top strip, or leaving the window through its top edge.
            let at_top = pointer.is_some_and(|p| p.y <= full.top() + 4.0 && p.x >= full.left() && p.x <= full.right());
            let left_by_top = pointer.is_none() && self.last_pointer.is_some_and(|p| p.y <= full.top() + 30.0);
            if left_by_top || self.handle_clicked {
                self.bar_visible = true;
                self.top_since = None;
            } else if !at_top {
                self.top_since = None;
            } else if now.duration_since(*self.top_since.get_or_insert(now)) > Duration::from_millis(120) {
                self.bar_visible = true;
                self.top_since = None;
            }
        }
        self.handle_clicked = false;
        if pointer.is_some() || !ctx.input(|i| i.pointer.has_pointer()) {
            self.last_pointer = pointer.filter(|_| ctx.input(|i| i.pointer.has_pointer()));
        }
        if self.top_since.is_some() || self.away_since.is_some() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        let shown = ctx.animate_bool_with_time(egui::Id::unique("bar shown"), self.bar_visible, 0.16);
        let bar_top = full.top() - (1.0 - shown) * (BAR_HEIGHT + 4.0);
        let under_bar = full.top() + BAR_HEIGHT;

        // The picture.
        let content = if self.immersive { full } else { Rect::from_min_max(pos2(full.left(), under_bar), full.max) };
        let panel = if self.immersive { content } else { content.shrink(8.0) };
        self.panel_size = Some(panel.size());
        if !self.immersive {
            root.painter().rect(panel, 12.0, theme::PANEL, Stroke::new(1.0, theme::white(0.09)), StrokeKind::Inside);
        }
        let streaming = s.active != StreamSource::None;
        let picture = match f.video_size {
            Some((w, h)) if streaming => {
                // One picture pixel on one screen pixel whenever the sizes agree (to a few pixels:
                // the Mac rounds a window's size its own way), and always on whole screen pixels:
                // anything else resamples the picture and softens its text.
                let ppp = ctx.pixels_per_point();
                let (pw, ph) = (panel.width() * ppp, panel.height() * ppp);
                let scale = (pw / w as f32).min(ph / h as f32);
                let scale = if (0.985..=1.015).contains(&scale) { 1.0 } else { scale };
                let (dw, dh) = ((w as f32 * scale).round(), (h as f32 * scale).round());
                let left = ((panel.left() * ppp) + ((pw - dw) / 2.0).max(0.0)).round();
                let top = ((panel.top() * ppp) + ((ph - dh) / 2.0).max(0.0)).round();
                Rect::from_min_size(pos2(left / ppp, top / ppp), vec2(dw / ppp, dh / ppp)).intersect(panel)
            }
            _ => panel,
        };
        let response = root.allocate_rect(picture, Sense::click_and_drag());
        if streaming {
            root.painter().add(egui_wgpu::Callback::new_paint_callback(picture, VideoPaint));
        }
        if f.video_size.is_some() && streaming {
            self.video_rect = Some(picture);
            self.video_hovered = response.contains_pointer() && s.lock_shade.is_none() && self.open == Open::Nothing;
        }
        if !streaming && s.lock_shade.is_none() {
            root.painter().text(panel.center(), Align2::CENTER_CENTER, "Choose a window in the bar, or Desktop.", theme::regular(15.0), MUTED);
        }
        if let Some(shade) = s.lock_shade {
            root.painter().rect_filled(panel, if self.immersive { 0.0 } else { 12.0 }, theme::PANEL);
            let title = LockedMac::title(&s.mac_name, shade);
            let detail = LockedMac::detail(shade, s.host_settings.as_ref().and_then(|h| h.session_paired));
            let at = panel.center();
            root.painter().text(pos2(at.x, at.y - 12.0), Align2::CENTER_CENTER, title, theme::semibold(17.0), TEXT);
            if let Some(d) = detail {
                let galley = root.painter().layout(d.to_string(), theme::regular(15.0), MUTED, 440.0f32.min(panel.width() - 48.0));
                root.painter().galley(pos2(at.x - galley.size().x / 2.0, at.y + 6.0), galley, MUTED);
            }
        }
        if s.prefs.hud {
            let st = s.stats.clone().unwrap_or_default();
            let size = f.video_size.map(|(w, h)| format!("{w}×{h}")).unwrap_or_else(|| "no video".into());
            let ms = f.decode.last_us.load(Ordering::Relaxed) as f32 / 1000.0;
            let line = format!(
                "{} fps · age {}/{} ms · rtt {}/{} ms · {size} · {} {ms:.1} ms · drawn {:.0}",
                st.fps, st.frame_age_ms, st.frame_age_max_ms, st.rtt_ms, st.rtt_max_ms, f.decode.backend.lock().unwrap(), f.drawn_fps
            );
            let font = egui::FontId::monospace(11.0);
            let galley = root.painter().layout_no_wrap(line, font, Color32::WHITE);
            let pill = Rect::from_min_size(pos2(panel.right() - 8.0 - galley.size().x - 16.0, panel.top() + 8.0), galley.size() + vec2(16.0, 6.0));
            root.painter().rect_filled(pill, pill.height() / 2.0, Color32::from_black_alpha(140));
            root.painter().galley(pill.min + vec2(8.0, 3.0), galley, Color32::WHITE);
        }
        // The handle: a small pill at the top centre, always there over a window alone; a click
        // brings the bar down.
        if self.immersive && !self.bar_visible {
            let pill = Rect::from_center_size(pos2(full.center().x, full.top() + 9.0), vec2(56.0, 10.0));
            let handle = egui::Area::new(egui::Id::unique("handle")).order(egui::Order::Foreground).fixed_pos(pill.min).show(&ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(pill.size(), Sense::click());
                let near = pointer.is_some_and(|p| p.y <= full.top() + 60.0);
                let fill = if response.hovered() { ACCENT } else if near { theme::white(0.75) } else { theme::white(0.38) };
                ui.painter().rect_filled(rect.shrink2(vec2(0.0, 2.5)), 3.0, Color32::from_black_alpha(110));
                ui.painter().rect_filled(rect.shrink2(vec2(1.0, 3.5)), 2.0, fill);
                response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Show the bar")
            });
            if handle.inner.clicked() {
                self.handle_clicked = true;
                ctx.request_repaint();
            }
        }
        // Once, the way to the bar.
        if self.immersive && !self.hinted {
            self.hinted = true;
            self.hint_until = Some(now + Duration::from_secs(4));
        }
        if let Some(until) = self.hint_until {
            if now < until && self.immersive && !self.bar_visible {
                let text = "For the bar, move the pointer to the top edge or click the handle there.";
                let galley = root.painter().layout_no_wrap(text.to_string(), theme::regular(13.0), TEXT);
                let pill = Rect::from_center_size(pos2(full.center().x, full.top() + 22.0 + 15.0), galley.size() + vec2(28.0, 14.0));
                let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("hint"));
                let painter = ctx.layer_painter(layer);
                painter.rect_filled(pill, pill.height() / 2.0, BAR.gamma_multiply(0.92));
                painter.galley(pill.min + vec2(14.0, 7.0), galley, TEXT);
                ctx.request_repaint_after(Duration::from_millis(250));
            } else if now >= until {
                self.hint_until = None;
            }
        }

        // What is open under the bar: a click beside it closes it.
        if self.open != Open::Nothing {
            let below = Rect::from_min_max(pos2(full.left(), under_bar), full.max);
            let catcher = egui::Area::new(egui::Id::unique("beside")).order(egui::Order::Middle).fixed_pos(below.min).show(&ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(below.size(), Sense::click());
                if self.open == Open::Apps {
                    ui.painter().rect_filled(rect, 0.0, Color32::from_black_alpha(148));
                }
                response
            });
            if catcher.inner.clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.open = Open::Nothing;
            }
        }
        if shown > 0.0 {
            self.bar(&ctx, f, Rect::from_min_size(pos2(full.left(), bar_top), vec2(full.width(), BAR_HEIGHT)));
        }
        match self.open {
            Open::Apps => self.apps(&ctx, f, Rect::from_min_max(pos2(full.left() + 22.0, under_bar + 8.0), pos2(full.left() + 22.0 + 380.0, full.bottom() - 14.0))),
            Open::Settings => self.settings(&ctx, f, Rect::from_min_max(pos2(full.right() - 22.0 - 360.0, under_bar + 8.0), pos2(full.right() - 22.0, full.bottom() - 14.0))),
            Open::TextSize | Open::Nothing => {}
        }
    }

    fn bar(&mut self, ctx: &egui::Context, f: &Frame, rect: Rect) {
        let s = f.state;
        egui::Area::new(egui::Id::unique("bar")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.set_clip_rect(rect.expand2(vec2(0.0, 20.0)));
            let (_, _) = ui.allocate_exact_size(rect.size(), Sense::click());
            ui.painter().rect_filled(rect, 0.0, BAR);
            let inner = Rect::from_min_max(pos2(rect.left() + 22.0, rect.top() + 10.0), pos2(rect.right() - 22.0, rect.bottom() - 10.0));
            let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
            let ui = &mut bar;
            ui.spacing_mut().item_spacing.x = 12.0;
            if theme::bar_button(ui, Glyph::Icon(Icon::Search), "Apps", self.open == Open::Apps).clicked() {
                self.toggle(Open::Apps);
            }
            let menus = s.has_menus && !s.menu.top.is_empty();
            let pin = matches!(s.active, StreamSource::Window(_));
            let right_count = 3 + menus as usize + pin as usize;
            let right_width = right_count as f32 * (theme::BAR_BUTTON.x + 12.0);
            let strip_width = (ui.available_width() - right_width).max(60.0);
            ui.allocate_ui_with_layout(vec2(strip_width, 66.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_clip_rect(ui.max_rect().expand2(vec2(0.0, 10.0)).intersect(rect.expand(20.0)));
                egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    ui.add_space(8.0);
                    let windows = s.windows.clone();
                    for w in &windows {
                        self.thumbnail(ui, f, w);
                    }
                    ui.add_space(8.0);
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if theme::bar_button(ui, Glyph::Icon(Icon::Gear), "Settings", self.open == Open::Settings).clicked() {
                    self.toggle(Open::Settings);
                }
                if pin {
                    let label = if self.keep_bar { "Pinned" } else { "Pin" };
                    let hint = if self.keep_bar { "Let the bar hide over a window" } else { "Keep the bar over a window" };
                    if theme::bar_button(ui, Glyph::Icon(Icon::Pin), label, self.keep_bar).on_hover_text(hint).clicked() {
                        self.keep_bar = !self.keep_bar;
                        self.prefs_changed = true;
                    }
                }
                if theme::bar_button(ui, Glyph::Icon(Icon::Desktop), "Desktop", s.active == StreamSource::Desktop).clicked() {
                    self.open = Open::Nothing;
                    f.client.select_source(StreamSource::Desktop);
                }
                self.text_size(ui, f);
                self.menus_open = false;
                if menus {
                    let button = theme::bar_button(ui, Glyph::Icon(Icon::Menus), "Menus", false);
                    if button.clicked() {
                        self.open = Open::Nothing;
                    }
                    let popup = egui::Popup::menu(&button).show(|ui| self.menu_list(ui, f));
                    self.menus_open = popup.is_some();
                    if !self.menus_open {
                        self.menu_asked = None;
                    }
                }
            });
        });
    }

    fn thumbnail(&mut self, ui: &mut egui::Ui, f: &Frame, w: &sill_core::protocol::WindowInfo) {
        let s = f.state;
        let size = vec2(104.0, 66.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let selected = s.active == StreamSource::Window(w.id);
        let texture = self.thumb(ui.ctx(), s, w.id);
        match &texture {
            Some(t) => {
                // Aspect fill.
                let [tw, th] = t.size();
                let (ta, ra) = (tw as f32 / th.max(1) as f32, size.x / size.y);
                let uv = if ta > ra {
                    let keep = ra / ta;
                    Rect::from_min_max(pos2((1.0 - keep) / 2.0, 0.0), pos2((1.0 + keep) / 2.0, 1.0))
                } else {
                    let keep = ta / ra;
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, keep))
                };
                egui::Image::new(t).uv(uv).corner_radius(10.0).paint_at(ui, rect);
            }
            None => {
                ui.painter().rect_filled(rect, 10.0, theme::THUMB_BODY);
                let strip = Rect::from_min_size(rect.min, vec2(rect.width(), 9.0));
                ui.painter().rect_filled(strip, egui::CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, theme::THUMB_TITLE_BAR);
            }
        }
        if response.hovered() && !selected {
            ui.painter().rect_filled(rect, 10.0, theme::white(0.06));
        }
        ui.painter().rect_stroke(rect, 10.0, Stroke::new(1.0, theme::white(0.12)), StrokeKind::Inside);
        if selected {
            ui.painter().rect_stroke(rect.expand(5.0), 15.0, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
        }
        // The app's badge, bottom-leading, on a ring of the bar's color.
        let badge = Rect::from_min_size(pos2(rect.left() - 6.0, rect.bottom() - 24.0 + 6.0), vec2(24.0, 24.0));
        ui.painter().rect_filled(badge.expand(2.0), 9.0, BAR);
        let icon = self.icon(ui.ctx(), s, &w.bundle_id);
        app_icon(ui, badge, icon.as_ref(), &w.app_name);
        let title = if w.title.is_empty() { w.app_name.clone() } else { format!("{} — {}", w.app_name, w.title) };
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(short(&title, 80));
        if response.clicked() {
            self.open = Open::Nothing;
            f.client.select_source(StreamSource::Window(w.id));
        }
        response.context_menu(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.horizontal(|ui| {
                for (color, label, action) in [
                    (theme::hex(0xFF5F57), "Close", window_actions::CLOSE),
                    (theme::hex(0xFEBC2E), "Minimize", window_actions::MINIMIZE),
                    (theme::hex(0x28C840), "Full Screen", window_actions::FULL_SCREEN),
                ] {
                    let (cell, light) = ui.allocate_exact_size(vec2(60.0, 46.0), Sense::click());
                    ui.painter().circle_filled(pos2(cell.center().x, cell.top() + 14.0), 12.0, if light.hovered() { color } else { color.gamma_multiply(0.9) });
                    ui.painter().text(pos2(cell.center().x, cell.bottom() - 8.0), Align2::CENTER_CENTER, label, theme::regular(10.0), theme::BAR_LABEL);
                    if light.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        f.client.window_command(w.id, action);
                        ui.close();
                    }
                }
            });
        });
    }

    fn text_size(&mut self, ui: &mut egui::Ui, f: &Frame) {
        let s = f.state;
        let usable = TextScale::selectable(s.active);
        let running = s.host_settings.as_ref().and_then(|h| h.stream.as_ref()).map(|r| r.on_virtual_display);
        let offered = TextScale::offers_off(s.shown_settings.as_ref().map(|x| x.virtual_display), s.active, running);
        let position = TextScale::position(s.text_scale, offered);
        let title = TextScale::title(position);
        let open = self.open == Open::TextSize;
        let button = ui.scope(|ui| {
            if !usable {
                ui.multiply_opacity(0.35);
            }
            theme::bar_button(ui, Glyph::Text("Aa"), &title, open)
        }).inner;
        if button.clicked() && usable {
            self.toggle(Open::TextSize);
        }
        let button = if usable { button } else { button.on_hover_text(TextScale::UNAVAILABLE_HINT) };
        if open {
            let detents = TextScale::detents(offered);
            let at = pos2(button.rect.center().x, button.rect.bottom() + 16.0);
            egui::Area::new(egui::Id::unique("text size")).order(egui::Order::Foreground).pivot(Align2::CENTER_TOP).fixed_pos(at).show(ui.ctx(), |ui| {
                egui::Frame::NONE.fill(theme::CONTROL_OPEN).corner_radius(16.0).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                    let labels: Vec<String> = detents.iter().map(|d| TextScale::title(*d)).collect();
                    let refs: Vec<&str> = labels.iter().map(|l| l.as_str()).collect();
                    let current = detents.iter().position(|d| (*d - position).abs() < 0.01);
                    if let Some(i) = theme::segmented(ui, &refs, current, true) {
                        let scale = TextScale::scale_at(detents[i]);
                        f.client.set_text_scale(scale);
                        self.text_scale = Some(scale);
                        self.prefs_changed = true;
                        self.open = Open::Nothing;
                    }
                });
            });
        }
    }

    fn menu_list(&mut self, ui: &mut egui::Ui, f: &Frame) {
        let s = f.state;
        ui.set_min_width(180.0);
        if let Some(note) = &s.menu.top_note {
            ui.label(RichText::new(note).font(theme::regular(12.0)).color(MUTED));
            ui.separator();
        }
        for top in &s.menu.top {
            let Some(title) = top.title.clone() else { continue };
            if !top.enabled || s.menu.top_stale {
                ui.add_enabled(false, egui::Button::new(title));
                continue;
            }
            ui.menu_button(title, |ui| self.menu_items(ui, f, top));
        }
    }

    fn menu_items(&mut self, ui: &mut egui::Ui, f: &Frame, parent: &MacMenuItem) {
        let Some(id) = parent.id.clone() else { return };
        ui.set_min_width(220.0);
        let state = f.state.menu.menus.get(&id);
        if self.menu_asked.as_deref() != Some(id.as_str()) && !matches!(state, Some(Menu::Loading)) {
            // Fetched fresh each time it opens.
            self.menu_asked = Some(id.clone());
            f.client.open_menu(parent);
        }
        match state {
            None | Some(Menu::Loading) => {
                ui.add(egui::Spinner::new().size(14.0).color(MUTED));
            }
            Some(Menu::Failed(why)) => {
                ui.label(RichText::new(why).color(MUTED));
            }
            Some(Menu::Items { items, more, note, .. }) => {
                if let Some(note) = note {
                    ui.label(RichText::new(note).font(theme::regular(12.0)).color(MUTED));
                    ui.separator();
                }
                if items.is_empty() {
                    ui.label(RichText::new("No items").color(MUTED));
                }
                for item in items {
                    if item.separator {
                        ui.separator();
                        continue;
                    }
                    let title = item.title.clone().unwrap_or_default();
                    let marked = match item.mark.as_deref() {
                        Some("on") => format!("✓ {title}"),
                        Some("mixed") => format!("– {title}"),
                        _ => title,
                    };
                    if item.submenu && item.enabled {
                        ui.menu_button(marked, |ui| self.menu_items(ui, f, item));
                        continue;
                    }
                    let mut button = egui::Button::new(marked);
                    if let Some(key) = &item.key {
                        button = button.shortcut_text(key.clone());
                    }
                    if ui.add_enabled(item.enabled, button).clicked() {
                        f.client.choose_menu_item(item);
                        ui.close();
                    }
                }
                if let Some(n) = more {
                    ui.label(RichText::new(format!("{n} more on the Mac")).color(MUTED));
                }
            }
        }
    }

    fn apps(&mut self, ctx: &egui::Context, f: &Frame, rect: Rect) {
        let s = f.state;
        ctx.data_mut(|d| d.insert_temp(egui::Id::NULL, false));
        egui::Area::new(egui::Id::unique("apps")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            box_frame().show(ui, |ui| {
                ui.set_width(rect.width() - 28.0);
                ui.set_max_height(rect.height() - 28.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                if let Some(shade) = s.lock_shade {
                    ui.label(RichText::new(LockedMac::title(&s.mac_name, shade)).font(theme::regular(15.0)).color(TEXT));
                    return;
                }
                // The search field.
                egui::Frame::NONE.fill(CONTROL).corner_radius(12.0).inner_margin(egui::Margin::symmetric(14, 0)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let (r, _) = ui.allocate_exact_size(vec2(18.0, 48.0), Sense::hover());
                        theme::icon(ui.painter(), Icon::Search, r.center(), 18.0, MUTED);
                        let width = ui.available_width();
                        let field = ui.add_sized(
                            vec2(width, 48.0),
                            egui::TextEdit::singleline(&mut self.search)
                                .frame(egui::Frame::NONE)
                                .font(theme::regular(15.0))
                                .align(egui::Align2::LEFT_CENTER)
                                .hint_text(RichText::new(format!("Search apps on {}", s.mac_name)).color(MUTED).font(theme::regular(15.0))),
                        );
                        field.request_focus();
                    });
                });
                let needle = self.search.trim().to_lowercase();
                let matches = |name: &str| needle.is_empty() || name.to_lowercase().contains(&needle);
                // Open now: the apps of the window list, in its order.
                let mut open: Vec<(String, String, Vec<u32>)> = Vec::new();
                for w in &s.windows {
                    match open.iter_mut().find(|a| a.0 == w.bundle_id) {
                        Some(a) => a.2.push(w.id),
                        None => open.push((w.bundle_id.clone(), w.app_name.clone(), vec![w.id])),
                    }
                }
                let mut all: Vec<&sill_core::protocol::AppInfo> = s.apps.iter().filter(|a| !open.iter().any(|o| o.0 == a.bundle_id) && matches(&a.name)).collect();
                all.sort_by_key(|a| a.name.to_lowercase());
                open.retain(|a| matches(&a.1));
                let first = open.first().map(|a| a.2[0]).map(Pick::Window).or_else(|| all.first().map(|a| Pick::Launch(a.bundle_id.clone())));
                let mut pick = None;
                egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    if !open.is_empty() {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space(10.0);
                            ui.label(RichText::new("Open now").font(theme::regular(13.0)).color(MUTED));
                        });
                        for (bundle, name, ids) in &open {
                            let icon = self.icon(ui.ctx(), s, bundle);
                            let owns = matches!(s.active, StreamSource::Window(id) if ids.contains(&id));
                            let count = if ids.len() == 1 { "1 window".to_string() } else { format!("{} windows", ids.len()) };
                            if drawer_row(ui, Lead::App(icon.as_ref(), name), name, &count, owns, 50.0).clicked() {
                                // Its first window, or the next one when it is already showing.
                                let next = match s.active {
                                    StreamSource::Window(id) => ids.iter().position(|x| *x == id).map(|i| ids[(i + 1) % ids.len()]),
                                    _ => None,
                                };
                                pick = Some(Pick::Window(next.unwrap_or(ids[0])));
                            }
                        }
                    }
                    if !all.is_empty() {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.add_space(10.0);
                            ui.label(RichText::new("All apps").font(theme::regular(13.0)).color(MUTED));
                        });
                        for app in &all {
                            let icon = self.icon(ui.ctx(), s, &app.bundle_id);
                            if drawer_row(ui, Lead::App(icon.as_ref(), &app.name), &app.name, "", false, 46.0).clicked() {
                                pick = Some(Pick::Launch(app.bundle_id.clone()));
                            }
                        }
                    }
                });
                if pick.is_none() && !needle.is_empty() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    pick = first;
                }
                match pick {
                    Some(Pick::Window(id)) => {
                        f.client.select_source(StreamSource::Window(id));
                        self.open = Open::Nothing;
                    }
                    Some(Pick::Launch(bundle)) => {
                        f.client.launch_app(&bundle);
                        self.open = Open::Nothing;
                    }
                    None => {}
                }
            });
        });
    }

    fn settings(&mut self, ctx: &egui::Context, f: &Frame, rect: Rect) {
        let s = f.state;
        let mac = if s.mac_name.is_empty() { "the Mac" } else { s.mac_name.as_str() };
        let change = |c: SettingsChange| f.client.change_settings(c);
        egui::Area::new(egui::Id::unique("settings")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            box_frame().show(ui, |ui| {
                ui.set_width(rect.width() - 28.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                // The header.
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(short(if s.mac_name.is_empty() { "Mac" } else { &s.mac_name }, 30)).font(theme::semibold(17.0)).color(TEXT));
                        let route = match (&s.route, s.current_mac.as_ref().is_some_and(|m| s.saved_macs_at_home.contains(&m.mac_id))) {
                            (Some(sill_core::client::Route::Tailscale), _) => " · Remote",
                            (_, true) => " · Wi-Fi",
                            _ => "",
                        };
                        let running = s.host_settings.as_ref().and_then(|h| h.stream.as_ref());
                        let line = match running {
                            Some(r) => format!("{}×{} · {} fps · {} Mbps{route}", r.width, r.height, r.fps, r.mbps),
                            None => format!("Not streaming{route}"),
                        };
                        ui.label(RichText::new(line).font(theme::regular(13.0)).color(MUTED));
                        if running.is_some_and(|r| r.on_virtual_display) {
                            ui.label(RichText::new("On the virtual display").font(theme::regular(13.0)).color(MUTED));
                        }
                        if let Some(st) = &s.stats {
                            if s.route == Some(sill_core::client::Route::Tailscale) {
                                ui.label(RichText::new(format!("Connected through Tailscale · {} ms", st.rtt_ms)).font(theme::regular(13.0)).color(MUTED));
                            }
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        ui.add_space(4.0);
                        if ui.add(egui::Label::new(RichText::new("Done").font(theme::semibold(16.0)).color(ACCENT)).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            self.open = Open::Nothing;
                        }
                    });
                });
                ui.add_space(12.0);
                let foot = 54.0;
                egui::ScrollArea::vertical()
                    .max_height((rect.height() - 28.0 - 70.0 - foot).max(120.0))
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if let (true, Some(shown)) = (s.has_settings, &s.shown_settings) {
                            let host = s.host_settings.as_ref();
                            if host.is_some_and(|h| h.software_encoder) {
                                theme::footnote(ui, &format!("{mac}’s hardware encoder is busy or not responding, so for now streams run at up to 60 fps at Standard."));
                            }
                            theme::group(ui, |ui| {
                                theme::row(ui, true, "Quality", |ui| {
                                    let current = QualityPreset::ALL.iter().find(|p| p.bitrate() == shown.bitrate);
                                    let text = match current {
                                        Some(p) => format!("{} — {} Mbps", p.title(), p.bitrate() / 1_000_000),
                                        None => format!("{} Mbps", shown.bitrate / 1_000_000),
                                    };
                                    egui::ComboBox::from_id_salt("quality").selected_text(RichText::new(text).color(ACCENT)).show_ui(ui, |ui| {
                                        for p in QualityPreset::ALL {
                                            if ui.selectable_label(p.bitrate() == shown.bitrate, format!("{} — {} Mbps", p.title(), p.bitrate() / 1_000_000)).clicked() {
                                                change(SettingsChange { bitrate: Some(p.bitrate()), ..Default::default() });
                                            }
                                        }
                                    });
                                });
                                theme::row(ui, false, "Resolution", |ui| {
                                    let at = if (shown.capture_scale - 2.0).abs() < 0.01 { Some(0) } else if (shown.capture_scale - 1.0).abs() < 0.01 { Some(1) } else { None };
                                    if let Some(i) = theme::segmented(ui, &["Retina", "Standard"], at, true) {
                                        change(SettingsChange { capture_scale: Some(if i == 0 { 2.0 } else { 1.0 }), ..Default::default() });
                                    }
                                });
                                theme::row(ui, false, "Frame Rate", |ui| {
                                    let at = match shown.max_fps {
                                        60 => Some(0),
                                        120 => Some(1),
                                        _ => None,
                                    };
                                    if let Some(i) = theme::segmented(ui, &["60 fps", "120 fps"], at, true) {
                                        change(SettingsChange { max_fps: Some(if i == 0 { 60 } else { 120 }), ..Default::default() });
                                    }
                                });
                            });
                            theme::footnote(ui, "Quality is per 60 fps; a 120 fps stream gets twice as much. Ultra and Extreme need a wired or very fast Wi‑Fi link; if the picture lags, step down.");
                            theme::group(ui, |ui| {
                                theme::row(ui, true, "Prioritize Encoding Speed", |ui| {
                                    if theme::switch(ui, shown.prioritize_speed, true).clicked() {
                                        change(SettingsChange { prioritize_speed: Some(!shown.prioritize_speed), ..Default::default() });
                                    }
                                });
                            });
                            theme::footnote(ui, &format!("Lower latency when {mac} is busy, at a softer picture."));
                            let available = host.is_some_and(|h| h.virtual_display_available);
                            theme::group(ui, |ui| {
                                theme::row(ui, true, "Virtual Display", |ui| {
                                    if theme::switch(ui, shown.virtual_display, available || shown.virtual_display).clicked() {
                                        change(SettingsChange { virtual_display: Some(!shown.virtual_display), ..Default::default() });
                                    }
                                });
                            });
                            match host.and_then(|h| h.virtual_display_note.as_ref()) {
                                Some(note) => theme::footnote(ui, note),
                                None => theme::footnote(ui, &format!("The window you pick moves onto an invisible display on {mac} while it streams, so it keeps updating when covered, and it takes this window's size. Applies to every device streaming from {mac}. The stream restarts for a moment.")),
                            }
                            theme::group(ui, |ui| {
                                let has_sound = shown.send_audio.is_some();
                                let on = shown.send_audio.unwrap_or(false);
                                theme::row(ui, true, "Send Audio  (Beta)", |ui| {
                                    if theme::switch(ui, on, has_sound).clicked() {
                                        change(SettingsChange { send_audio: Some(!on), ..Default::default() });
                                    }
                                });
                                if has_sound {
                                    theme::row(ui, false, "Mute", |ui| {
                                        let muted = s.prefs.sound_muted;
                                        if theme::switch(ui, muted, true).clicked() {
                                            f.client.update_prefs(move |p| p.sound_muted = !muted);
                                        }
                                    });
                                }
                            });
                            let note = host.and_then(|h| h.audio_note.clone());
                            let text = match (shown.send_audio, note) {
                                (None, _) => format!("Beta. Update Sill on {mac} to hear its sound on this computer."),
                                (Some(true), Some(note)) => note,
                                (Some(true), None) => format!("Beta. Every connected device hears what streams, and {mac} keeps playing it too. Mute silences it on this computer alone."),
                                (Some(false), _) => format!("Beta. Turn this on to hear what streams on every connected device; {mac} keeps playing it too."),
                            };
                            theme::footnote(ui, &text);
                        } else {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(14.0).color(MUTED));
                                ui.label(RichText::new("Loading settings…").font(theme::regular(13.0)).color(MUTED));
                            });
                            ui.add_space(12.0);
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            theme::header(ui, "This computer");
                        });
                        ui.add_space(6.0);
                        theme::group(ui, |ui| {
                            theme::row(ui, true, "Keep the Bar over a Window", |ui| {
                                if theme::switch(ui, self.keep_bar, true).clicked() {
                                    self.keep_bar = !self.keep_bar;
                                    self.prefs_changed = true;
                                }
                            });
                        });
                        theme::footnote(ui, "Off, a window on its own fills this window, as if its app ran here. For the bar, move the pointer to the top edge or click the handle there.");
                        theme::group(ui, |ui| {
                            theme::row(ui, true, "Ctrl is ⌘", |ui| {
                                let on = self.layout == ModifierLayout::ControlIsCommand;
                                if theme::switch(ui, on, true).clicked() {
                                    self.layout = if on { ModifierLayout::ByPosition } else { ModifierLayout::ControlIsCommand };
                                    self.prefs_changed = true;
                                }
                            });
                        });
                        theme::footnote(ui, &format!("On: Ctrl+C copies on {mac}, as on Windows and Linux, and the Windows key is ⌃. Off: keys go by their place, as a PC keyboard plugged into a Mac: the Windows key is ⌘."));
                        if s.host_clipboard >= 1 {
                            theme::group(ui, |ui| {
                                theme::row(ui, true, "Share Clipboard", |ui| {
                                    let on = s.prefs.share_clipboard;
                                    if theme::switch(ui, on, true).clicked() {
                                        f.client.update_prefs(move |p| p.share_clipboard = !on);
                                    }
                                });
                            });
                            theme::footnote(ui, &format!("What you copy here can be pasted on {mac}, and what you copy there, here."));
                        }
                        theme::group(ui, |ui| {
                            theme::row(ui, true, "Reverse Scrolling", |ui| {
                                let on = s.prefs.reverse_scroll;
                                if theme::switch(ui, on, true).clicked() {
                                    f.client.update_prefs(move |p| p.reverse_scroll = !on);
                                }
                            });
                            theme::row(ui, false, "Show the Stream's Numbers", |ui| {
                                let on = s.prefs.hud;
                                if theme::switch(ui, on, true).clicked() {
                                    f.client.update_prefs(move |p| p.hud = !on);
                                }
                            });
                        });
                        theme::footnote(ui, "Reverse Scrolling turns the touchpad’s two-finger scroll around; a wheel keeps its direction.");
                        if let Some(v) = &s.host_version {
                            theme::footnote(ui, &format!("Sill for Mac {v} · Sill Desktop {}", env!("CARGO_PKG_VERSION")));
                        }
                    });
                // The foot.
                ui.add_space(10.0);
                let (r, response) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
                ui.painter().rect_filled(r, 12.0, if response.hovered() { theme::CONTROL_HOVER } else { CONTROL });
                theme::icon(ui.painter(), Icon::Close, pos2(r.left() + 14.0 + 9.0, r.center().y), 13.0, ACCENT);
                ui.painter().circle_stroke(pos2(r.left() + 14.0 + 9.0, r.center().y), 9.0, Stroke::new(1.4, ACCENT));
                ui.painter().text(pos2(r.left() + 14.0 + 26.0, r.center().y), Align2::LEFT_CENTER, short(&format!("Disconnect from {mac}"), 38), theme::regular(15.0), ACCENT);
                if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.open = Open::Nothing;
                    f.client.disconnect();
                }
            });
        });
    }
}

enum Pick {
    Window(u32),
    Launch(String),
}

