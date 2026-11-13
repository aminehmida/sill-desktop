//! The look, after Sill's own apps (iOSClient's `Palette` and bar): its colors, Inter for the
//! system font, and the controls egui has no equal of — the bar's buttons, a switch, a segmented
//! picker, grouped rows.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

pub const fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub const BLACK: Color32 = Color32::BLACK;
/// The top bar and the drawers.
pub const BAR: Color32 = hex(0x16181C);
/// Bar buttons, the search field, a selected row.
pub const CONTROL: Color32 = hex(0x23262C);
pub const CONTROL_HOVER: Color32 = hex(0x2B2F36);
/// A bar button whose thing is showing.
pub const CONTROL_OPEN: Color32 = hex(0x2C3550);
pub const ACCENT: Color32 = hex(0x9BB7FF);
pub const TEXT: Color32 = hex(0xEEF0F3);
pub const BAR_LABEL: Color32 = hex(0xC4C9D1);
pub const MUTED: Color32 = hex(0xA4ABB5);
/// The stream panel behind the video.
pub const PANEL: Color32 = hex(0x1F2126);
pub const THUMB_BODY: Color32 = hex(0x26282D);
pub const THUMB_TITLE_BAR: Color32 = hex(0x34373D);
pub const ICON_FALLBACK: Color32 = hex(0x3A3D44);
pub const WARNING: Color32 = hex(0xFFB86B);
pub const ERROR: Color32 = hex(0xFF8A80);
pub const SWITCH_ON: Color32 = hex(0x34C759);

pub fn white(alpha: f32) -> Color32 {
    Color32::from_white_alpha((alpha * 255.0) as u8)
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("inter".into(), egui::FontData::from_static(include_bytes!("../assets/fonts/Inter-Regular.ttf")).into());
    fonts.font_data.insert("inter-semibold".into(), egui::FontData::from_static(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")).into());
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "inter".into());
    let mut semi = vec!["inter-semibold".to_string()];
    semi.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(FontFamily::Name("semibold".into()), semi);
    ctx.set_fonts(fonts);

    // Dark whatever the system says, as Sill's own apps are: egui otherwise follows the system's
    // theme and keeps a second set of visuals for it.
    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = BLACK;
    v.window_fill = BAR;
    v.window_stroke = Stroke::new(1.0, white(0.08));
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(12);
    v.override_text_color = Some(TEXT);
    v.extreme_bg_color = CONTROL;
    v.selection.bg_fill = hex(0x3A4A78);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, white(0.08));
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(8);
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke = Stroke::new(1.0, TEXT);
    }
    v.widgets.inactive.weak_bg_fill = CONTROL;
    v.widgets.inactive.bg_fill = CONTROL;
    v.widgets.hovered.weak_bg_fill = CONTROL_HOVER;
    v.widgets.hovered.bg_fill = CONTROL_HOVER;
    v.widgets.active.weak_bg_fill = CONTROL_OPEN;
    v.widgets.active.bg_fill = CONTROL_OPEN;
    v.widgets.open.weak_bg_fill = CONTROL_OPEN;
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(12.0, 7.0);
        s.spacing.interact_size.y = 30.0;
        s.interaction.selectable_labels = false;
        for (style, font) in [
            (egui::TextStyle::Body, regular(14.0)),
            (egui::TextStyle::Button, regular(14.0)),
            (egui::TextStyle::Small, regular(11.5)),
            (egui::TextStyle::Heading, semibold(17.0)),
            (egui::TextStyle::Monospace, FontId::monospace(12.5)),
        ] {
            s.text_styles.insert(style, font);
        }
    });
}

/// The bar's glyphs, drawn (the original uses SF Symbols, which only Apple's systems have).
#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Search,
    Menus,
    Desktop,
    Gear,
    Pin,
    Close,
    Display,
}

pub fn icon(painter: &egui::Painter, kind: Icon, center: Pos2, size: f32, color: Color32) {
    let s = size / 20.0;
    let w = Stroke::new(1.6 * s.max(0.8), color);
    let p = |x: f32, y: f32| pos2(center.x + (x - 10.0) * s, center.y + (y - 10.0) * s);
    match kind {
        Icon::Search => {
            painter.circle_stroke(p(8.6, 8.6), 5.6 * s, w);
            painter.line_segment([p(12.8, 12.8), p(17.2, 17.2)], Stroke::new(2.0 * s, color));
        }
        Icon::Menus => {
            painter.rect_stroke(Rect::from_min_max(p(2.0, 3.0), p(18.0, 17.0)), 3.0 * s, w, StrokeKind::Inside);
            painter.line_segment([p(2.5, 7.4), p(17.5, 7.4)], w);
            painter.line_segment([p(5.0, 11.0), p(15.0, 11.0)], w);
            painter.line_segment([p(5.0, 14.0), p(12.0, 14.0)], w);
        }
        Icon::Desktop | Icon::Display => {
            painter.rect_stroke(Rect::from_min_max(p(1.5, 2.5), p(18.5, 13.8)), 2.4 * s, w, StrokeKind::Inside);
            if kind == Icon::Desktop {
                painter.rect_filled(Rect::from_min_max(p(3.4, 4.4), p(16.6, 11.9)), 1.0 * s, color.gamma_multiply(0.55));
            }
            painter.line_segment([p(10.0, 13.8), p(10.0, 16.8)], w);
            painter.line_segment([p(6.2, 17.2), p(13.8, 17.2)], Stroke::new(1.8 * s, color));
        }
        Icon::Gear => {
            let teeth = 8;
            let mut points = Vec::new();
            for i in 0..teeth * 4 {
                let a = std::f32::consts::TAU * (i as f32 + 0.5) / (teeth * 4) as f32;
                let r = if i % 4 < 2 { 8.4 } else { 6.5 };
                points.push(p(10.0 + r * a.cos(), 10.0 + r * a.sin()));
            }
            painter.add(Shape::closed_line(points, w));
            painter.circle_stroke(p(10.0, 10.0), 2.7 * s, w);
        }
        Icon::Pin => {
            // A push pin, leaning: head, body, point.
            painter.line_segment([p(7.2, 3.6), p(16.4, 12.8)], Stroke::new(2.0 * s, color));
            painter.add(Shape::closed_line(vec![p(9.0, 5.4), p(12.6, 9.0), p(11.4, 13.4), p(6.6, 8.6)], w));
            painter.line_segment([p(8.6, 11.4), p(3.6, 16.4)], Stroke::new(1.8 * s, color));
        }
        Icon::Close => {
            painter.line_segment([p(5.0, 5.0), p(15.0, 15.0)], Stroke::new(1.8 * s, color));
            painter.line_segment([p(15.0, 5.0), p(5.0, 15.0)], Stroke::new(1.8 * s, color));
        }
    }
}

/// What a bar button shows above its label.
pub enum Glyph<'a> {
    Icon(Icon),
    Text(&'a str),
}

pub const BAR_HEIGHT: f32 = 86.0;
pub const BAR_BUTTON: Vec2 = vec2(66.0, 66.0);

/// One of the bar's buttons: a glyph over an 11-point label, lit while its thing is showing.
pub fn bar_button(ui: &mut Ui, glyph: Glyph, label: &str, open: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(BAR_BUTTON, Sense::click());
    let fill = if open { CONTROL_OPEN } else if response.hovered() { CONTROL_HOVER } else { CONTROL };
    let ink = if open { ACCENT } else { TEXT };
    let painter = ui.painter();
    painter.rect_filled(rect, 16.0, fill);
    let top = pos2(rect.center().x, rect.top() + 26.0);
    match glyph {
        Glyph::Icon(kind) => icon(painter, kind, top, 22.0, ink),
        Glyph::Text(t) => {
            painter.text(top, Align2::CENTER_CENTER, t, semibold(19.0), ink);
        }
    }
    painter.text(pos2(rect.center().x, rect.bottom() - 15.0), Align2::CENTER_CENTER, label, regular(11.0), if open { ACCENT } else { BAR_LABEL });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A switch, as iOS draws one.
pub fn switch(ui: &mut Ui, on: bool, enabled: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(42.0, 25.0), if enabled { Sense::click() } else { Sense::hover() });
    let t = ui.ctx().animate_bool_with_time(response.id, on, 0.12);
    let track = if on { SWITCH_ON } else { hex(0x3A3D44) };
    let fade = if enabled { 1.0 } else { 0.4 };
    ui.painter().rect_filled(rect, rect.height() / 2.0, track.gamma_multiply(fade));
    let x = egui::lerp(rect.left() + 12.5..=rect.right() - 12.5, t);
    ui.painter().circle_filled(pos2(x, rect.center().y), 10.5, Color32::WHITE.gamma_multiply(fade));
    response
}

/// A segmented picker; returns the index picked this frame.
pub fn segmented(ui: &mut Ui, options: &[&str], selected: Option<usize>, enabled: bool) -> Option<usize> {
    let font = regular(12.5);
    let widths: Vec<f32> = options.iter().map(|o| ui.painter().layout_no_wrap(o.to_string(), font.clone(), TEXT).size().x + 22.0).collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, 28.0), Sense::hover());
    ui.painter().rect_filled(rect, 8.0, hex(0x2E3138));
    let mut picked = None;
    let mut x = rect.left() + 2.0;
    for (i, option) in options.iter().enumerate() {
        let cell = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(widths[i], 24.0));
        x += widths[i];
        let response = ui.interact(cell, ui.scope_id().with(("segment", options.first().copied().unwrap_or(""), i)), if enabled { Sense::click() } else { Sense::hover() });
        if selected == Some(i) {
            ui.painter().rect_filled(cell, 6.5, hex(0x5A5E68));
        }
        let ink = if !enabled { MUTED.gamma_multiply(0.6) } else if selected == Some(i) { Color32::WHITE } else { TEXT };
        ui.painter().text(cell.center(), Align2::CENTER_CENTER, option, font.clone(), ink);
        if response.clicked() && selected != Some(i) {
            picked = Some(i);
        }
    }
    picked
}

/// A settings group: rows on a slightly lighter card, hairlines between them.
pub fn group<R>(ui: &mut Ui, contents: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(CONTROL)
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(12, 2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_width(ui.available_width());
            contents(ui)
        })
        .inner
}

/// A row in a group: its label on the left, its control on the right. `first`: no hairline above.
pub fn row(ui: &mut Ui, first: bool, label: &str, control: impl FnOnce(&mut Ui)) {
    if !first {
        let r = ui.available_rect_before_wrap();
        ui.painter().line_segment([pos2(r.left(), r.top()), pos2(r.right(), r.top())], Stroke::new(1.0, white(0.08)));
    }
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 40.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.label(egui::RichText::new(label).font(regular(14.0)).color(TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

/// The small grey text under a group.
pub fn footnote(ui: &mut Ui, text: &str) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 14, right: 14, top: 6, bottom: 12 }).show(ui, |ui| {
        ui.label(egui::RichText::new(text).font(regular(12.5)).color(MUTED));
    });
}

/// A section's header inside a drawer.
pub fn header(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(regular(11.5)).color(MUTED));
}

/// A link in the accent color.
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), semibold(15.0), ACCENT);
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x, 44.0), Sense::click());
    let ink = if response.hovered() { Color32::WHITE } else { ACCENT };
    ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, text, semibold(15.0), ink);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The filled button of a card (Pair, Connect).
pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), semibold(15.0), BLACK);
    let size = vec2((galley.size().x + 24.0).max(88.0), 44.0);
    let (rect, response) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let fill = if !enabled { ACCENT.gamma_multiply(0.4) } else if response.hovered() { hex(0xB4C9FF) } else { ACCENT };
    ui.painter().rect_filled(rect, 12.0, fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, semibold(15.0), if enabled { BLACK } else { MUTED });
    response
}
