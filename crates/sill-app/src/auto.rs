//! The bench's script: `--auto "pair:127.0.0.1:47455,123456789012;wait:streaming;select:first;…"`.
//! Steps run in order, each when the one before is done; the app prints `BENCH …` lines and exits
//! with 1 when the script does not finish in time.

use sill_core::protocol::{key_mods, SettingsChange, StreamSource};
use sill_core::{Client, ClientState, MouseButton, Phase};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum Step {
    Pair(String, String),
    Connect,
    WaitStreaming,
    WaitPictures(u64),
    WaitIdle,
    Sleep(f64),
    Select(String),
    Press(u16, u64),
    Text(String),
    Click(f64, f64),
    Scroll(f64),
    Fps(i64),
    Paste(String),
    Disconnect,
    /// Open something of the window: apps, settings, bar, add, nothing.
    Ui(String),
    Shot(String),
    Exit,
}

pub fn parse(script: &str) -> Result<Vec<Step>, String> {
    script
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let (name, arg) = s.split_once(':').unwrap_or((s, ""));
            let num = |a: &str| a.parse::<f64>().map_err(|_| format!("not a number in {s}"));
            Ok(match name {
                "pair" => {
                    let (address, code) = arg.rsplit_once(',').ok_or(format!("pair:ADDRESS,CODE, not {s}"))?;
                    Step::Pair(address.into(), code.into())
                }
                "connect" => Step::Connect,
                "wait" => match arg.split_once(',').unwrap_or((arg, "")) {
                    ("streaming", _) => Step::WaitStreaming,
                    ("idle", _) => Step::WaitIdle,
                    ("pictures", n) => Step::WaitPictures(num(n)? as u64),
                    _ => return Err(format!("wait:streaming, wait:idle or wait:pictures,N, not {s}")),
                },
                "sleep" => Step::Sleep(num(arg)?),
                "select" => Step::Select(arg.into()),
                "press" => {
                    let (usage, mods) = arg.split_once(',').unwrap_or((arg, ""));
                    let mut m = 0;
                    for c in mods.chars() {
                        m |= match c {
                            's' => key_mods::SHIFT,
                            'c' => key_mods::CONTROL,
                            'o' => key_mods::OPTION,
                            'm' => key_mods::COMMAND,
                            _ => return Err(format!("modifiers are s, c, o, m, not {c}")),
                        }
                    }
                    Step::Press(num(usage)? as u16, m)
                }
                "text" => Step::Text(arg.into()),
                "click" => {
                    let (x, y) = arg.split_once(',').ok_or(format!("click:X,Y, not {s}"))?;
                    Step::Click(num(x)?, num(y)?)
                }
                "scroll" => Step::Scroll(num(arg)?),
                "fps" => Step::Fps(num(arg)? as i64),
                "paste" => Step::Paste(arg.into()),
                "disconnect" => Step::Disconnect,
                "ui" => Step::Ui(arg.into()),
                "shot" => Step::Shot(arg.into()),
                "exit" => Step::Exit,
                _ => return Err(format!("unknown step {s}")),
            })
        })
        .collect()
}

pub enum Outcome {
    /// Still running: look again soon.
    Going,
    /// Take a screenshot of the next frame into this file, then carry on.
    Shot(String),
    Exit(i32),
}

pub struct Runner {
    steps: Vec<Step>,
    at: usize,
    since: Instant,
    started: Instant,
    limit: Duration,
    /// A screenshot is being written: the script waits for it.
    pub shot_pending: bool,
    /// What the script wants open in the window, for the window to take.
    pub ui_request: Option<String>,
}

impl Runner {
    pub fn new(steps: Vec<Step>, limit: f64) -> Self {
        Self { steps, at: 0, since: Instant::now(), started: Instant::now(), limit: Duration::from_secs_f64(limit), shot_pending: false, ui_request: None }
    }

    pub fn tick(&mut self, client: &Client, state: &ClientState, pictures: u64) -> Outcome {
        loop {
            if self.shot_pending {
                return Outcome::Going;
            }
            let Some(step) = self.steps.get(self.at).cloned() else { return Outcome::Exit(0) };
            if self.started.elapsed() > self.limit {
                println!("BENCH TIMEOUT at step {} {:?}: phase {:?}, status {:?}, error {:?}, pair error {:?}, pictures {pictures}",
                    self.at, step, state.phase, state.status, state.last_error, state.pair_error);
                return Outcome::Exit(1);
            }
            let done = match &step {
                Step::Pair(address, code) => {
                    client.pair_with_code(address, code);
                    true
                }
                Step::Connect => match state.saved_macs.first() {
                    Some(mac) => {
                        client.connect(&mac.mac_id);
                        true
                    }
                    None => false,
                },
                Step::WaitStreaming => state.phase == Phase::Streaming,
                Step::WaitIdle => state.phase == Phase::Idle,
                Step::WaitPictures(n) => pictures >= *n,
                Step::Sleep(s) => self.since.elapsed().as_secs_f64() >= *s,
                Step::Select(what) => {
                    let source = match what.as_str() {
                        "desktop" => Some(StreamSource::Desktop),
                        "first" => state.windows.first().map(|w| StreamSource::Window(w.id)),
                        id => id.parse().ok().map(StreamSource::Window),
                    };
                    match source {
                        Some(s) => {
                            client.select_source(s);
                            true
                        }
                        None => false,
                    }
                }
                Step::Press(usage, mods) => {
                    client.press(*usage, *mods);
                    true
                }
                Step::Text(t) => {
                    client.text(t);
                    true
                }
                Step::Click(x, y) => {
                    client.mouse_move(*x, *y);
                    client.mouse_button(MouseButton::Left, true, *x, *y);
                    client.mouse_button(MouseButton::Left, false, *x, *y);
                    true
                }
                Step::Scroll(dy) => {
                    client.scroll(0.5, 0.5, 0.0, *dy);
                    true
                }
                Step::Fps(fps) => {
                    client.change_settings(SettingsChange { max_fps: Some(*fps), ..Default::default() });
                    true
                }
                Step::Paste(t) => {
                    client.paste_text(t);
                    true
                }
                Step::Disconnect => {
                    client.disconnect();
                    true
                }
                Step::Ui(what) => {
                    self.ui_request = Some(what.clone());
                    true
                }
                Step::Shot(path) => {
                    println!("BENCH ok {} {:?}", self.at, step);
                    self.at += 1;
                    self.since = Instant::now();
                    self.shot_pending = true;
                    return Outcome::Shot(path.clone());
                }
                Step::Exit => {
                    println!("BENCH done pictures={pictures} seconds={:.2}", self.started.elapsed().as_secs_f64());
                    return Outcome::Exit(0);
                }
            };
            if !done {
                return Outcome::Going;
            }
            println!("BENCH ok {} {:?}", self.at, step);
            self.at += 1;
            self.since = Instant::now();
        }
    }
}
