// No console window behind the app on Windows (a debug build keeps it, for its log).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod adapters;
mod auto;
mod gfx;
mod keymap;
mod pipeline;
mod theme;
mod this_computer;
mod ui;
mod video_view;

use gfx::Gfx;
use keymap::ModifierLayout;
use pipeline::Pipeline;
use sill_core::protocol::{key_mods, ScrollPhase};
use sill_core::{Client, ClientConfig, ClientEvent, MouseButton, Phase};
use sill_media::audio::{MacSound, Sink};
use sill_media::video::Prefer;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use video_view::{Mailbox, SharedMailbox};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::PhysicalKey;
use winit::window::{CursorIcon, CustomCursor, Window, WindowId};

#[derive(Debug)]
enum Wake {
    Picture,
    Client,
}

struct Options {
    config_dir: PathBuf,
    prefer: Prefer,
    sound: Option<Sink>,
    allow_loopback: bool,
    discovery: bool,
    auto: Option<Vec<auto::Step>>,
    timeout: f64,
    size: (f64, f64),
}

struct App {
    options: Options,
    proxy: EventLoopProxy<Wake>,
    mailbox: SharedMailbox,
    pipeline: Arc<Pipeline>,
    client: Client,
    sound: Option<Arc<adapters::Sound>>,
    gfx: Option<Gfx>,
    ui: ui::Ui,
    runner: Option<auto::Runner>,
    shot_done: Arc<AtomicBool>,
    exit_code: i32,
    // Input, as the Mac has it.
    pressed: BTreeSet<u16>,
    mods: u64,
    pointer: Option<(f64, f64)>,
    buttons: (bool, bool),
    scrolling: bool,
    cursor_applied: Option<usize>,
    mac_cursor: Option<(usize, CustomCursor)>,
    // The viewport the Mac was last told, and one waiting to settle.
    viewport_sent: Option<(f32, f32)>,
    viewport_waiting: Option<((f32, f32), Instant)>,
    next_redraw: Option<Instant>,
    drawn: (Instant, u32, f32),
    title: String,
    /// The source streaming and since when, and the one this window was last shaped for.
    source_since: (sill_core::protocol::StreamSource, Instant),
    shaped_for: Option<sill_core::protocol::StreamSource>,
    shape_tries: u32,
    shaped_at: Instant,
    #[allow(clippy::type_complexity)]
    shape_seen: ((Option<(u32, u32)>, Option<(i32, i32)>), Instant),
}

fn modifier_bit(usage: u16) -> u64 {
    match usage {
        0xE0 | 0xE4 => key_mods::CONTROL,
        0xE1 | 0xE5 => key_mods::SHIFT,
        0xE2 | 0xE6 => key_mods::OPTION,
        0xE3 | 0xE7 => key_mods::COMMAND,
        _ => 0,
    }
}

impl App {
    fn layout_file(&self) -> PathBuf {
        self.options.config_dir.join("app.json")
    }

    fn release_keys(&mut self) {
        for usage in std::mem::take(&mut self.pressed) {
            self.mods &= !modifier_bit(usage);
            self.client.key(usage, false, self.mods);
        }
        self.mods = 0;
    }

    /// The pointer's place on the picture as fractions, held inside it.
    fn on_picture(&self, g: &Gfx) -> Option<(f64, f64)> {
        let rect = self.ui.video_rect?;
        let (x, y) = self.pointer?;
        let ppp = g.ctx.pixels_per_point() as f64;
        let fx = ((x / ppp - rect.min.x as f64) / rect.width() as f64).clamp(0.0, 1.0);
        let fy = ((y / ppp - rect.min.y as f64) / rect.height() as f64).clamp(0.0, 1.0);
        Some((fx, fy))
    }

    fn streaming(&self) -> bool {
        self.client.state().phase == Phase::Streaming
    }

    /// Input for the Mac; true when the event was the Mac's and egui is not to see it.
    fn forward(&mut self, event: &WindowEvent) -> bool {
        let Some(g) = &self.gfx else { return false };
        if !self.streaming() {
            return false;
        }
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                if g.ctx.egui_wants_keyboard_input() {
                    return false;
                }
                let PhysicalKey::Code(code) = event.physical_key else { return true };
                let Some(usage) = keymap::hid_usage(code).map(|u| self.ui.layout.apply(u)) else { return true };
                let down = event.state == ElementState::Pressed;
                if down && event.repeat && self.client.state().host_key_repeat >= 1 {
                    return true;
                }
                if down {
                    self.pressed.insert(usage);
                    self.mods |= modifier_bit(usage);
                } else {
                    if !self.pressed.remove(&usage) {
                        return true; // its down went elsewhere
                    }
                    self.mods &= !modifier_bit(usage);
                }
                self.client.key(usage, down, self.mods);
                true
            }
            WindowEvent::Focused(false) => {
                self.release_keys();
                false
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some((position.x, position.y));
                let held = self.buttons.0 || self.buttons.1;
                if self.ui.video_hovered || held {
                    if let Some((x, y)) = self.on_picture(g) {
                        self.client.mouse_move(x, y);
                    }
                }
                false
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let down = *state == ElementState::Pressed;
                let (which, held) = match button {
                    winit::event::MouseButton::Left => (MouseButton::Left, &mut self.buttons.0),
                    winit::event::MouseButton::Right => (MouseButton::Right, &mut self.buttons.1),
                    _ => return false,
                };
                if (down && !self.ui.video_hovered) || (!down && !*held) {
                    return false;
                }
                *held = down;
                if let Some((x, y)) = self.on_picture(g) {
                    self.client.mouse_button(which, down, x, y);
                }
                false
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                if !self.ui.video_hovered {
                    return false;
                }
                let Some((x, y)) = self.on_picture(g) else { return false };
                let rect = self.ui.video_rect.unwrap();
                let ppp = g.ctx.pixels_per_point() as f64;
                let state = self.client.state();
                let (dx, dy, pad) = match delta {
                    // A wheel's click: three lines of 16 points.
                    MouseScrollDelta::LineDelta(dx, dy) => (*dx as f64 * 48.0, *dy as f64 * 48.0, false),
                    MouseScrollDelta::PixelDelta(p) => (p.x / ppp, p.y / ppp, true),
                };
                let turn = if pad && state.prefs.reverse_scroll { -1.0 } else { 1.0 };
                if pad && *phase == TouchPhase::Started && !self.scrolling {
                    self.scrolling = true;
                    self.client.scroll_phase(ScrollPhase::Began, x, y);
                }
                if dx != 0.0 || dy != 0.0 {
                    self.client.scroll(x, y, turn * dx / rect.width() as f64, turn * dy / rect.height() as f64);
                }
                if self.scrolling && matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                    self.scrolling = false;
                    self.client.scroll_phase(ScrollPhase::Ended, x, y);
                }
                true
            }
            _ => false,
        }
    }

    /// The Mac's pointer picture over the stream, this computer's own elsewhere.
    fn apply_cursor(&mut self, event_loop: &ActiveEventLoop) {
        let Some(g) = &self.gfx else { return };
        let state = self.client.state();
        let wanted = match (&state.cursor, self.ui.video_hovered) {
            (Some(shape), true) => Some(Arc::as_ptr(shape) as usize),
            _ => None,
        };
        if wanted == self.cursor_applied {
            return;
        }
        self.cursor_applied = wanted;
        let Some(key) = wanted else {
            g.window.set_cursor(CursorIcon::Default);
            return;
        };
        if self.mac_cursor.as_ref().map(|c| c.0) != Some(key) {
            let shape = state.cursor.as_ref().unwrap();
            let scale = g.window.scale_factor();
            let made = image::load_from_memory(&shape.png).ok().and_then(|img| {
                let w = ((shape.width as f64 * scale).round() as u32).clamp(1, 256);
                let h = ((shape.height as f64 * scale).round() as u32).clamp(1, 256);
                let img = image::imageops::resize(&img.to_rgba8(), w, h, image::imageops::FilterType::Triangle);
                let hot_x = ((shape.hot_x as f64 * scale) as u32).min(w - 1) as u16;
                let hot_y = ((shape.hot_y as f64 * scale) as u32).min(h - 1) as u16;
                CustomCursor::from_rgba(img.into_raw(), w as u16, h as u16, hot_x, hot_y).ok()
            });
            match made {
                Some(source) => self.mac_cursor = Some((key, event_loop.create_custom_cursor(source))),
                None => {
                    g.window.set_cursor(CursorIcon::Default);
                    return;
                }
            }
        }
        g.window.set_cursor(self.mac_cursor.as_ref().unwrap().1.clone());
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let state = self.client.state();
        for event in self.client.drain_events() {
            match event {
                ClientEvent::Goodbye { .. } | ClientEvent::SessionEnded { .. } => self.release_keys(),
                ClientEvent::Paired { mac_id } => self.client.connect(&mac_id),
                // The Mac sizes the window picked to this window (1×) unless Aa says otherwise:
                // a window left at its own size is resampled to fit, which softens it.
                ClientEvent::Connected { .. } => self.client.set_text_scale(self.ui.text_scale.unwrap_or(Some(1.0))),
                _ => {}
            }
        }
        let Some(g) = &mut self.gfx else { return };
        let (uploaded, video_size) = g.video(|v| (v.uploaded, v.size)).unwrap_or((0, None));
        let mut capture = None;
        if let Some(runner) = &mut self.runner {
            if self.shot_done.swap(false, Ordering::Relaxed) {
                runner.shot_pending = false;
            }
            let outcome = runner.tick(&self.client, &state, uploaded);
            if let Some(what) = runner.ui_request.take() {
                self.ui.script(&what);
            }
            match outcome {
                auto::Outcome::Going => {}
                auto::Outcome::Shot(path) => {
                    let done = self.shot_done.clone();
                    capture = Some(egui::ScreenshotCallback::new(move |img: Arc<egui::ColorImage>| {
                        let bytes: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
                        if let Some(dir) = std::path::Path::new(&path).parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        let r = image::save_buffer(&path, &bytes, img.size[0] as u32, img.size[1] as u32, image::ColorType::Rgba8);
                        println!("BENCH shot {path} {}x{} {}", img.size[0], img.size[1], if r.is_ok() { "saved" } else { "FAILED" });
                        done.store(true, Ordering::Relaxed);
                    }));
                }
                auto::Outcome::Exit(code) => {
                    self.exit_code = code;
                    event_loop.exit();
                    return;
                }
            }
        }
        let frame = ui::Frame { client: &self.client, state: &state, video_size, decode: &self.pipeline.stats, drawn_fps: self.drawn.2 };
        let ui = &mut self.ui;
        let delay = g.frame(capture, |root| ui.show(root, &frame));
        let title = self.ui.title(&state);
        if title != self.title {
            g.window.set_title(&title);
            self.title = title;
        }
        self.drawn.1 += 1;
        if self.drawn.0.elapsed() >= Duration::from_secs(1) {
            self.drawn = (Instant::now(), 0, self.drawn.1 as f32 / self.drawn.0.elapsed().as_secs_f32());
        }
        let now = Instant::now();
        // A window alone that the Mac did not fit to this window (no virtual display): this window
        // takes the picture's shape instead, once per window picked, so no bars are left around it.
        if state.active != self.source_since.0 {
            self.source_since = (state.active, now);
        }
        // Up to three times per window picked or per resize by hand: the Mac may answer the new
        // size with one of its own (an app's smallest size, say).
        let seen = (video_size, self.ui.panel_size.map(|p| (p.x.round() as i32, p.y.round() as i32)));
        if seen != self.shape_seen.0 {
            self.shape_seen = (seen, now);
        }
        if let (true, Some((w, h)), Some(panel)) = (self.ui.immersive, video_size, self.ui.panel_size) {
            if self.shaped_for != Some(state.active) {
                self.shaped_for = Some(state.active);
                self.shape_tries = 0;
            }
            let settled = now.duration_since(self.source_since.1) > Duration::from_millis(1200) && now.duration_since(self.shape_seen.1) > Duration::from_millis(700);
            let off = (panel.x / panel.y) / (w as f32 / h as f32);
            let free = !g.window.is_maximized() && g.window.fullscreen().is_none();
            if !(0.995..=1.005).contains(&off) && free && self.shape_tries < 3 {
                if settled {
                    self.shape_tries += 1;
                    self.shaped_at = now;
                    let size = if off > 1.0 { (panel.y * w as f32 / h as f32, panel.y) } else { (panel.x, panel.x * h as f32 / w as f32) };
                    let _ = g.window.request_inner_size(winit::dpi::LogicalSize::new(size.0.round() as f64, size.1.round() as f64));
                }
                self.next_redraw = Some(now + Duration::from_millis(250));
            }
        } else if !self.ui.immersive {
            self.shaped_for = None;
        }
        // The Mac sizes the window to the panel: once the size has stood still for a moment.
        if let Some(size) = self.ui.panel_size {
            let size = (size.x.round(), size.y.round());
            if Some(size) != self.viewport_sent && self.viewport_waiting.map(|w| w.0) != Some(size) {
                self.viewport_waiting = Some((size, now));
            }
            if let Some((size, since)) = self.viewport_waiting {
                if now.duration_since(since) >= Duration::from_millis(150) || self.viewport_sent.is_none() {
                    self.client.set_viewport(size.0 as f64, size.1 as f64);
                    self.viewport_sent = Some(size);
                    self.viewport_waiting = None;
                }
            }
        } else {
            self.viewport_sent = None;
            self.viewport_waiting = None;
        }
        let mut next = delay.map(|d| now + d);
        if let Some(t) = self.next_redraw.take() {
            next = Some(next.map_or(t, |n| n.min(t)));
        }
        let soon = |next: &mut Option<Instant>, t: Instant| *next = Some(next.map_or(t, |n| n.min(t)));
        if self.viewport_waiting.is_some() {
            soon(&mut next, now + Duration::from_millis(160));
        }
        if self.runner.is_some() {
            soon(&mut next, now + Duration::from_millis(30));
        } else if state.phase == Phase::Streaming {
            soon(&mut next, now + Duration::from_secs(1));
        }
        self.next_redraw = next;
        if self.ui.prefs_changed {
            self.ui.prefs_changed = false;
            self.release_keys();
            let on = self.ui.layout == ModifierLayout::ControlIsCommand;
            let _ = std::fs::write(self.layout_file(), serde_json::json!({ "controlIsCommand": on, "keepBar": self.ui.keep_bar, "textScale": self.ui.text_scale.map(|s| s.unwrap_or(0.0)) }).to_string());
        }

        self.apply_cursor(event_loop);
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let (w, h) = self.options.size;
        let attrs = Window::default_attributes().with_title("Sill").with_inner_size(winit::dpi::LogicalSize::new(w, h));
        let window = Arc::new(event_loop.create_window(attrs).expect("a window"));
        if let Some(hz) = window.current_monitor().and_then(|m| m.refresh_rate_millihertz()) {
            let hz = hz as f64 / 1000.0;
            self.client.set_display_fps(hz.round() as u32);
            if let Some(s) = &self.sound {
                s.0.set_refresh_hz(hz);
            }
        }
        match Gfx::new(window, event_loop.owned_display_handle(), self.mailbox.clone()) {
            Ok(g) => {
                theme::install(&g.ctx);
                self.gfx = Some(g);
            }
            Err(e) => {
                eprintln!("Sill could not open its window's GPU surface: {e}");
                self.exit_code = 2;
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, _: Wake) {
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.next_redraw {
            Some(t) if t <= Instant::now() => {
                self.next_redraw = None;
                if let Some(g) = &self.gfx {
                    g.window.request_redraw();
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if self.gfx.is_none() {
            return;
        }
        let taken = self.forward(&event);
        let g = self.gfx.as_mut().unwrap();
        if !taken {
            let response = g.input.on_window_event(&g.window, &event);
            if response.repaint {
                g.window.request_redraw();
            }
        }
        match event {
            WindowEvent::CloseRequested => {
                self.client.disconnect();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                g.resized(size.width, size.height);
                // A resize by hand: the window may take the picture's shape again.
                if self.shaped_at.elapsed() > Duration::from_millis(600) {
                    self.shape_tries = 0;
                }
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {}
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "sill [--config-dir DIR] [--decoder auto|software|NAME] [--no-sound | --null-sound] [--no-discovery]\n     [--allow-loopback] [--size WxH] [--auto SCRIPT] [--timeout SECONDS]"
    );
    std::process::exit(2)
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut options = Options {
        config_dir: dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("sill-desktop"),
        prefer: Prefer::Auto,
        sound: Some(Sink::Device),
        allow_loopback: false,
        discovery: true,
        auto: None,
        timeout: 30.0,
        size: (1180.0, 760.0),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config-dir" => options.config_dir = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--decoder" => {
                options.prefer = match args.next().as_deref() {
                    Some("software") => Prefer::Software,
                    Some("auto") => Prefer::Auto,
                    Some(n) => Prefer::Named(n.into()),
                    None => usage(),
                }
            }
            "--no-sound" => options.sound = None,
            "--null-sound" => options.sound = Some(Sink::Null),
            "--no-discovery" => options.discovery = false,
            "--allow-loopback" => options.allow_loopback = true,
            "--timeout" => options.timeout = args.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "--size" => {
                options.size = args
                    .next()
                    .and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
                    .unwrap_or_else(|| usage())
            }
            "--auto" => match auto::parse(&args.next().unwrap_or_else(|| usage())) {
                Ok(steps) => options.auto = Some(steps),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2)
                }
            },
            _ => usage(),
        }
    }
    let _ = std::fs::create_dir_all(&options.config_dir);
    let event_loop = EventLoop::<Wake>::with_user_event().build().expect("an event loop");
    let proxy = event_loop.create_proxy();
    let mailbox: SharedMailbox = Arc::new(Mutex::new(Mailbox::default()));
    let wake = proxy.clone();
    let pipeline = Pipeline::new(options.prefer.clone(), mailbox.clone(), move || {
        let _ = wake.send_event(Wake::Picture);
    });
    let sound = options.sound.take().map(|sink| Arc::new(adapters::Sound(MacSound::with_sink(sink))));
    let mut config = ClientConfig::new(options.config_dir.clone(), pipeline.clone());
    config.device_name = gethostname::gethostname().to_string_lossy().trim_end_matches(".local").to_string();
    config.device_model = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    // What the Mac checks for compatibility: the Sill release this client matches, without this
    // app's own pre-release suffix.
    config.app_version = env!("CARGO_PKG_VERSION").split('-').next().unwrap_or("0.6.1").to_string();
    config.sound = sound.clone().map(|s| s as Arc<dyn sill_core::client::SoundPlayer>);
    config.clipboard = Some(Arc::new(adapters::Clipboard::new()));
    config.allow_loopback = options.allow_loopback;
    config.discovery = options.discovery;
    let wake = proxy.clone();
    config.waker = Some(Arc::new(move || {
        let _ = wake.send_event(Wake::Client);
    }));
    let client = match Client::new(config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Sill could not start: {e}");
            std::process::exit(2);
        }
    };
    pipeline.set_feedback(client.video_feedback());
    if options.discovery {
        client.start_discovery();
    }
    let mut ui = ui::Ui::default();
    ui.me.loopback_is_other = options.allow_loopback;
    if let Ok(text) = std::fs::read_to_string(options.config_dir.join("app.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if v["controlIsCommand"].as_bool() == Some(true) {
                ui.layout = ModifierLayout::ControlIsCommand;
            }
            ui.keep_bar = v["keepBar"].as_bool() == Some(true);
            ui.text_scale = v["textScale"].as_f64().map(|s| (s > 0.0).then_some(s));
        }
    }
    let runner = options.auto.take().map(|steps| auto::Runner::new(steps, options.timeout));
    let mut app = App {
        options,
        proxy,
        mailbox,
        pipeline,
        client,
        sound,
        gfx: None,
        ui,
        runner,
        shot_done: Arc::default(),
        exit_code: 0,
        pressed: BTreeSet::new(),
        mods: 0,
        pointer: None,
        buttons: (false, false),
        scrolling: false,
        cursor_applied: None,
        mac_cursor: None,
        viewport_sent: None,
        viewport_waiting: None,
        next_redraw: None,
        drawn: (Instant::now(), 0, 0.0),
        title: "Sill".into(),
        source_since: (sill_core::protocol::StreamSource::None, Instant::now()),
        shaped_for: None,
        shape_tries: 0,
        shaped_at: Instant::now(),
        shape_seen: ((None, None), Instant::now()),
    };
    event_loop.run_app(&mut app).expect("the event loop");
    let _ = &app.proxy;
    std::process::exit(app.exit_code);
}
