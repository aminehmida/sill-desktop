//! iOSClient/DiscoveryPolicy.swift's issue #69 rules, against Tests/checks/policy/sleep.swift's
//! cases (DiscoveryPolicyTest.kt).

use sill_core::net::{DiscoveryPolicy as P, ReconnectClock, ReconnectStep};

const QUIET: &str = "Mac mini isn’t answering. It may be asleep. Sill keeps trying, or tap it to try now.";
const STOPPED: &str = "Stopped trying to reach Mac mini. Tap it to try again.";
const RECONNECTING: &str = "Reconnecting to Mac mini…";

#[test]
fn browser_retry_backs_off() {
    assert_eq!(vec![1.0, 2.0, 4.0, 8.0, 16.0, 30.0, 30.0], (1..=7).map(P::browser_retry_delay).collect::<Vec<_>>());
    assert_eq!((1.0, 1.0), (P::browser_retry_delay(0), P::browser_retry_delay(-3)));
    assert!((1..=200).all(|n| P::browser_retry_delay(n) <= 30.0));
    assert_eq!(1, P::browser_failures(0, 10.0, 10.5));
    assert_eq!(2, P::browser_failures(1, 10.0, 10.5));
    assert_eq!(6, P::browser_failures(5, 10.0, 39.9));
    assert_eq!(1, P::browser_failures(5, 10.0, 40.0));
    assert_eq!(1, P::browser_failures(-2, 10.0, 11.0));
}

#[test]
fn rebuild_browser() {
    let r = |failed_at: Option<f64>, failures: i32, now: f64, fg: bool, back: bool| {
        let d = P::rebuild_browser(failed_at, failures, back, fg, now);
        (d.now, d.recheck_at)
    };
    assert_eq!((true, None), r(None, 0, 5.0, true, true));
    assert_eq!((true, None), r(Some(10.0), 3, 10.0, true, true));
    assert_eq!((false, None), r(None, 0, 5.0, true, false));
    assert_eq!((false, None), r(None, 4, 500.0, true, false));
    assert_eq!((false, Some(11.0)), r(Some(10.0), 1, 10.0, true, false));
    assert_eq!((false, Some(11.0)), r(Some(10.0), 1, 10.999, true, false));
    assert_eq!((true, None), r(Some(10.0), 1, 11.0, true, false));
    assert_eq!((false, Some(14.0)), r(Some(10.0), 3, 13.9, true, false));
    assert_eq!((false, Some(40.0)), r(Some(10.0), 6, 39.0, true, false));
    assert_eq!((false, Some(40.0)), r(Some(10.0), 9, 39.0, true, false));
    assert_eq!((false, None), r(Some(10.0), 1, 11.0, false, false));
    assert_eq!((false, None), r(Some(10.0), 1, 1e6, false, false));
}

#[test]
fn browser_results_settle() {
    let kept = vec![("A", "old"), ("B", "old")];
    let res = |k: &[(&'static str, &'static str)], fresh: &[(&'static str, &'static str)], ends: Option<f64>, now: f64| -> Vec<String> {
        P::browser_results(k, fresh, |r| r.0.to_string(), ends, now).into_iter().map(|r| format!("{}@{}", r.0, r.1)).collect()
    };
    assert_eq!(vec!["A@new"], res(&[], &[("A", "new")], None, 5.0));
    assert_eq!(vec!["A@new", "B@old"], res(&kept, &[("A", "new")], Some(103.0), 100.0));
    assert_eq!(vec!["A@old", "B@old"], res(&kept, &[], Some(103.0), 100.0));
    assert_eq!(vec!["B@new", "A@old"], res(&kept, &[("B", "new")], Some(103.0), 102.9));
    assert_eq!(vec!["A@new"], res(&kept, &[("A", "new")], Some(103.0), 103.0));
    assert_eq!(vec!["C@new", "A@old", "B@old"], res(&kept, &[("C", "new")], Some(f64::INFINITY), 1e9));
    assert_eq!(3.0, P::REBUILD_SETTLE);
}

#[test]
fn redial_row_and_its_lines() {
    let d = |failed: u32, last: Option<f64>, now: f64, fg: bool| {
        let d = P::redial_row(failed, last, fg, now);
        (d.now, d.recheck_at)
    };
    assert_eq!((true, None), d(0, None, 0.0, true));
    assert_eq!((true, None), d(0, Some(50.0), 51.0, true));
    assert_eq!((false, Some(15.0)), d(1, Some(10.0), 12.0, true));
    assert_eq!((false, Some(15.0)), d(1, Some(10.0), 14.999, true));
    assert_eq!((true, None), d(1, Some(10.0), 15.0, true));
    assert_eq!((false, Some(25.0)), d(2, Some(15.0), 24.0, true));
    assert_eq!((true, None), d(2, Some(15.0), 25.0, true));
    assert_eq!((false, Some(45.0)), d(3, Some(25.0), 44.0, true));
    assert_eq!((true, None), d(3, Some(25.0), 45.0, true));
    assert_eq!((false, Some(75.0)), d(4, Some(45.0), 74.0, true));
    assert_eq!((false, Some(130.0)), d(7, Some(100.0), 129.0, true));
    assert_eq!((true, None), d(11, Some(100.0), 130.0, true));
    assert_eq!((false, None), d(12, Some(100.0), 1000.0, true));
    assert_eq!((false, None), d(30, Some(100.0), 1000.0, true));
    assert_eq!((false, None), d(0, None, 0.0, false));
    assert_eq!((false, None), d(5, Some(0.0), 1000.0, false));
    assert!((0..=2).all(|n| P::reconnect_line("Mac mini", n) == RECONNECTING));
    assert!((3..=11).all(|n| P::reconnect_line("Mac mini", n) == QUIET));
    assert_eq!(STOPPED, P::reconnect_line("Mac mini", 12));
    assert_eq!(vec![2.0, 4.0, 8.0, 10.0, 10.0], (1..=5).map(P::remote_retry_delay).collect::<Vec<_>>());
}

// MARK: the reconnect clock, as the client runs it

#[derive(Default)]
struct Run {
    dials: Vec<f64>,
    connected_at: Option<f64>,
    lines: Vec<(f64, String)>,
    quiet_at: Option<f64>,
}

struct Model {
    until: f64,
    answers_from: f64,
    listing: Box<dyn Fn(f64) -> Option<f64>>,
    background: Option<(f64, f64)>,
    dial_for: f64,
}

impl Default for Model {
    fn default() -> Self {
        Model { until: 600.0, answers_from: f64::INFINITY, listing: Box::new(|_| Some(0.0)), background: None, dial_for: 5.0 }
    }
}

/// The glue as a model (sleep.swift's): a look every 50 ms, a dial the Mac answers connects at
/// once, one it does not fails `dial_for` s later. `listing(t)`: the row's listing, `None` while
/// not listed.
fn reconnect(m: Model) -> Run {
    let mut run = Run::default();
    let mut clock = ReconnectClock::new("M1", "Mac mini", 0.0, true);
    let mut in_flight_until: Option<f64> = None;
    let mut was_background = false;
    let mut t = 0.0f64;
    while t <= m.until && run.connected_at.is_none() {
        let fg = !m.background.map(|(a, b)| t >= a && t <= b).unwrap_or(false);
        if !fg {
            was_background = true;
        }
        if fg && was_background {
            was_background = false;
            clock.came_forward(t);
        }
        if let Some(until) = in_flight_until {
            if t >= until {
                in_flight_until = None;
                clock.dial_failed(t);
            }
        }
        if in_flight_until.is_none() {
            let line = match clock.next((m.listing)(t), fg, t) {
                ReconnectStep::Dial { line } => {
                    run.dials.push(t);
                    if t >= m.answers_from {
                        run.connected_at = Some(t);
                    } else {
                        in_flight_until = Some(t + m.dial_for);
                    }
                    line
                }
                ReconnectStep::Wait { line, .. } => line,
            };
            if run.lines.last().map(|l| l.1.as_str()) != Some(line.as_str()) {
                run.lines.push((t, line));
            }
            if clock.quiet() && run.quiet_at.is_none() {
                run.quiet_at = Some(t);
            }
        }
        t = (t * 20.0 + 1.0).round() / 20.0;
    }
    run
}

#[test]
fn a_host_stopped_for_twenty_seconds_is_reached_at_the_third_dial() {
    let stop = reconnect(Model { answers_from: 14.0, ..Default::default() });
    assert_eq!(vec![0.0, 5.0, 15.0], stop.dials);
    assert_eq!(Some(15.0), stop.connected_at);
    assert_eq!(vec![RECONNECTING], stop.lines.iter().map(|l| l.1.as_str()).collect::<Vec<_>>());
}

#[test]
fn a_mac_a_sleep_proxy_answers_for_is_dialed_twelve_times_then_left() {
    let asleep = reconnect(Model { until: 3600.0, ..Default::default() });
    assert_eq!(vec![0.0, 5.0, 15.0, 35.0, 65.0, 95.0, 125.0, 155.0, 185.0, 215.0, 245.0, 275.0], asleep.dials);
    assert_eq!(vec![0.0, 20.0, 280.0], asleep.lines.iter().map(|l| l.0).collect::<Vec<_>>());
    assert_eq!(vec![RECONNECTING, QUIET, STOPPED], asleep.lines.iter().map(|l| l.1.as_str()).collect::<Vec<_>>());
    assert!(asleep.dials.windows(2).all(|w| w[1] - w[0] >= 5.0));
    assert_eq!(Some(20.0), asleep.quiet_at, "the connect screen from the quiet line on");
    assert_eq!(None, asleep.connected_at);
}

#[test]
fn a_mac_that_wakes_or_is_listed_afresh_is_connected_to() {
    assert_eq!(Some(125.0), reconnect(Model { answers_from: 100.0, ..Default::default() }).connected_at);
    let relisted = reconnect(Model {
        answers_from: 50.0,
        listing: Box::new(|t| if t < 49.0 { Some(0.0) } else if t < 50.0 { None } else { Some(50.0) }),
        ..Default::default()
    });
    assert_eq!(Some(&50.0), relisted.dials.last());
    assert_eq!(Some(50.0), relisted.connected_at);
    // Past its twelve dials, the Mac listed afresh (it woke and registered again) is dialed at once.
    let late = reconnect(Model {
        until: 3600.0,
        answers_from: 1000.0,
        listing: Box::new(|t| if t < 1000.0 { Some(0.0) } else { Some(1000.0) }),
        ..Default::default()
    });
    assert_eq!(Some(1000.0), late.connected_at);
}

#[test]
fn no_dial_in_the_background_and_the_return_starts_again() {
    let away = reconnect(Model { until: 3600.0, background: Some((40.0, 300.0)), ..Default::default() });
    assert!(!away.dials.iter().any(|d| (40.0..=300.0).contains(d)));
    let back: Vec<f64> = away.dials.iter().copied().filter(|d| *d > 300.0).collect();
    assert_eq!(12, back.len());
    assert_eq!(4 + 12, away.dials.len());
    assert!(back.iter().take(3).zip([300.05, 305.05, 315.05]).all(|(a, b)| (a - b).abs() < 0.06));
}

#[test]
fn not_listed_the_remote_dials_back_off_for_two_minutes_then_wait_for_the_row() {
    let remote = reconnect(Model { listing: Box::new(|_| None), dial_for: 1.0, ..Default::default() });
    assert_eq!(vec![0.0, 3.0, 8.0, 17.0, 28.0, 39.0, 50.0, 61.0, 72.0, 83.0, 94.0, 105.0, 116.0], remote.dials);
    assert_eq!(vec![(0.0, RECONNECTING.to_string()), (120.0, STOPPED.to_string())], remote.lines);
    // The Mac listed at 300 s (it woke at home): dialed at once, and connected.
    let woke = reconnect(Model {
        answers_from: 300.0,
        listing: Box::new(|t| if t < 300.0 { None } else { Some(300.0) }),
        dial_for: 1.0,
        ..Default::default()
    });
    assert_eq!(Some(300.0), woke.connected_at);
}

#[test]
fn the_stream_stays_up_only_while_the_reconnect_is_a_blip() {
    let mut clock = ReconnectClock::new("M1", "Mac mini", 0.0, true);
    assert!(clock.shows_stream());
    for i in 0..3 {
        assert!(matches!(clock.next(Some(0.0), true, f64::from(i) * 100.0), ReconnectStep::Dial { .. }));
        assert!(clock.shows_stream(), "the dial under way is not counted");
        clock.dial_failed(f64::from(i) * 100.0 + 5.0);
    }
    assert!(clock.quiet() && !clock.shows_stream());
    assert!(!ReconnectClock::new("M1", "Mac mini", 0.0, false).shows_stream());
    let mut refused = ReconnectClock::new("M1", "Mac mini", 0.0, true);
    refused.answered_for_good();
    assert!(refused.quiet());
    assert_eq!(ReconnectStep::Wait { at: None, line: STOPPED.to_string() }, refused.next(None, true, 1.0));
    refused.came_forward(500.0);
    assert_eq!(ReconnectStep::Dial { line: RECONNECTING.to_string() }, refused.next(None, true, 500.0));
}
