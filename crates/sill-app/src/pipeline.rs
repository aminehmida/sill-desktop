//! From the session to the screen: the client hands frames over on its reader thread, a decode
//! thread turns them into pictures, and the newest one waits in the mailbox for the next redraw.

use crate::video_view::SharedMailbox;
use sill_core::protocol::ParameterSets;
use sill_core::{VideoFeedback, VideoSink};
use sill_media::video::{Prefer, VideoDecoder};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

enum Job {
    Params(Vec<u8>),
    Frame(Vec<u8>, bool, f64),
    Reset,
}

/// What the HUD shows of the decoder.
#[derive(Default)]
pub struct DecodeStats {
    pub pictures: AtomicU64,
    pub errors: AtomicU64,
    /// Microseconds the last picture took to decode.
    pub last_us: AtomicU64,
    pub backend: Mutex<String>,
}

pub struct Pipeline {
    tx: Mutex<Sender<Job>>,
    waiting: Arc<AtomicUsize>,
    feedback: Arc<Mutex<Option<VideoFeedback>>>,
    pub stats: Arc<DecodeStats>,
}

/// Frames waiting for the decoder before it gives up on them and asks for a keyframe.
const LONGEST_QUEUE: usize = 45;

impl Pipeline {
    pub fn new(prefer: Prefer, mailbox: SharedMailbox, wake: impl Fn() + Send + 'static) -> Arc<Self> {
        let (tx, rx) = channel::<Job>();
        let waiting = Arc::new(AtomicUsize::new(0));
        let feedback: Arc<Mutex<Option<VideoFeedback>>> = Arc::default();
        let stats: Arc<DecodeStats> = Arc::default();
        let (w, f, s) = (waiting.clone(), feedback.clone(), stats.clone());
        std::thread::Builder::new()
            .name("sill decode".into())
            .spawn(move || {
                let mut decoder: Option<VideoDecoder> = None;
                let mut params: Vec<u8> = Vec::new();
                let mut nal_length = 4usize;
                let mut need_keyframe = true;
                let interrupted = |f: &Arc<Mutex<Option<VideoFeedback>>>| {
                    if let Some(fb) = f.lock().unwrap().as_ref() {
                        fb.picture_interrupted();
                    }
                };
                for job in rx {
                    match job {
                        Job::Reset => {
                            decoder = None;
                            params.clear();
                            need_keyframe = true;
                            let mut m = mailbox.lock().unwrap();
                            m.newest = None;
                            m.cleared = true;
                            drop(m);
                            wake();
                        }
                        Job::Params(payload) => {
                            if let Some(sets) = ParameterSets::parse(&payload) {
                                nal_length = payload.first().copied().unwrap_or(4) as usize;
                                params = sets.annex_b();
                                need_keyframe = true;
                            }
                        }
                        Job::Frame(data, keyframe, pts) => {
                            let behind = w.fetch_sub(1, Ordering::Relaxed);
                            if behind > LONGEST_QUEUE && !need_keyframe {
                                log::warn!("the decoder is {behind} frames behind: waiting for a keyframe");
                                need_keyframe = true;
                                if let Some(d) = decoder.as_mut() {
                                    d.flush();
                                }
                                interrupted(&f);
                            }
                            if need_keyframe && !keyframe {
                                continue;
                            }
                            let Some(mut unit) = ParameterSets::to_annex_b(&data, nal_length) else {
                                s.errors.fetch_add(1, Ordering::Relaxed);
                                continue;
                            };
                            if keyframe {
                                let mut with = params.clone();
                                with.append(&mut unit);
                                unit = with;
                            }
                            if decoder.is_none() {
                                match VideoDecoder::new(&prefer) {
                                    Ok(d) => {
                                        *s.backend.lock().unwrap() = d.backend().to_string();
                                        decoder = Some(d);
                                    }
                                    Err(e) => {
                                        log::error!("no HEVC decoder: {e}");
                                        s.errors.fetch_add(1, Ordering::Relaxed);
                                        continue;
                                    }
                                }
                            }
                            let d = decoder.as_mut().unwrap();
                            {
                                let mut m = mailbox.lock().unwrap();
                                for p in m.shown.drain(..) {
                                    d.recycle(p);
                                }
                            }
                            let started = Instant::now();
                            let mut got = Vec::new();
                            let result = d.decode(&unit, pts, &mut |p| got.push(p));
                            *s.backend.lock().unwrap() = d.backend().to_string();
                            match result {
                                Err(e) => {
                                    log::warn!("decoding: {e}");
                                    s.errors.fetch_add(1, Ordering::Relaxed);
                                    d.flush();
                                    need_keyframe = true;
                                    interrupted(&f);
                                }
                                Ok(()) => {
                                    if got.is_empty() {
                                        continue;
                                    }
                                    s.last_us.store(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                                    s.pictures.fetch_add(got.len() as u64, Ordering::Relaxed);
                                    if keyframe {
                                        need_keyframe = false;
                                        if let Some(fb) = f.lock().unwrap().as_ref() {
                                            fb.keyframe_shown();
                                        }
                                    }
                                    let mut m = mailbox.lock().unwrap();
                                    for p in got {
                                        if let Some(old) = m.newest.replace(p) {
                                            m.skipped += 1;
                                            d.recycle(old);
                                        }
                                    }
                                    drop(m);
                                    wake();
                                }
                            }
                        }
                    }
                }
            })
            .expect("the decode thread");
        Arc::new(Self { tx: Mutex::new(tx), waiting, feedback, stats })
    }

    /// The client's way to hear that the picture was interrupted or a keyframe was shown.
    pub fn set_feedback(&self, feedback: VideoFeedback) {
        *self.feedback.lock().unwrap() = Some(feedback);
    }

    fn send(&self, job: Job) {
        let _ = self.tx.lock().unwrap().send(job);
    }
}

impl VideoSink for Pipeline {
    fn parameter_sets(&self, payload: &[u8]) {
        self.send(Job::Params(payload.to_vec()));
    }

    fn frame(&self, data: &[u8], keyframe: bool, host_timestamp: f64) {
        self.waiting.fetch_add(1, Ordering::Relaxed);
        self.send(Job::Frame(data.to_vec(), keyframe, host_timestamp));
    }

    fn reset(&self) {
        self.send(Job::Reset);
    }
}
