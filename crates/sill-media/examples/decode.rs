//! Decodes an Annex B .h265 file and prints the time per picture: `decode FILE [auto|software|NAME]`.
use sill_media::video::*;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = args.next().expect("a .h265 file");
    let prefer = match args.next().as_deref() {
        None | Some("auto") => Prefer::Auto,
        Some("software") => Prefer::Software,
        Some(n) => Prefer::Named(n.into()),
    };
    let data = std::fs::read(&file).unwrap();
    let units = split_access_units(&data);
    let mut dec = VideoDecoder::new(&prefer).unwrap();
    let mut times = Vec::new();
    let (mut pictures, mut size, mut sum) = (0u32, (0, 0), 0u64);
    let mut kept: Vec<Picture> = Vec::new();
    let all = Instant::now();
    for (i, u) in units.iter().enumerate() {
        let t = Instant::now();
        let r = dec.decode(u, i as f64, &mut |p| {
            pictures += 1;
            size = (p.width, p.height);
            sum += p.y[p.y.len() / 2] as u64 + p.uv[p.uv.len() / 2] as u64;
            kept.push(p);
        });
        if let Err(e) = r {
            eprintln!("unit {i}: {e}");
        }
        for p in kept.drain(..) {
            dec.recycle(p);
        }
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = times.len();
    println!(
        "{} units, {} keyframes, {pictures} pictures {}x{} via {} in {:.0} ms: median {:.2} ms, p95 {:.2} ms, max {:.2} ms (check {sum})",
        n, units.iter().filter(|u| is_keyframe(u)).count(), size.0, size.1, dec.backend(), all.elapsed().as_secs_f64() * 1000.0,
        times[n / 2], times[n * 95 / 100], times[n - 1]
    );
}
