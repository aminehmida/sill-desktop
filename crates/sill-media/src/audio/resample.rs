//! The timeline's 48 kHz stereo at the output device's rate: a small polyphase windowed-sinc
//! resampler (32 taps, 256 phases, linear between phases), for a device that does not run at 48 kHz.
//! No allocation once it runs at a steady block size.

const TAPS: usize = 32;
const HALF: usize = TAPS / 2;
const PHASES: usize = 256;

pub struct Resampler {
    /// Input frames per output frame.
    step: f64,
    /// Where the next output frame is, in frames of `buf`.
    pos: f64,
    /// Input not yet behind every tap, interleaved stereo.
    buf: Vec<f32>,
    /// `PHASES + 1` rows of `TAPS` coefficients.
    table: Vec<f32>,
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-12 * sum {
        term *= (x / (2.0 * k)) * (x / (2.0 * k));
        sum += term;
        k += 1.0;
    }
    sum
}

impl Resampler {
    pub fn new(from_rate: f64, to_rate: f64) -> Self {
        let step = from_rate / to_rate;
        // The pass band ends under the lower of the two Nyquist rates.
        let cutoff = 0.95 * (to_rate / from_rate).min(1.0);
        let beta = 6.0;
        let mut table = vec![0f32; (PHASES + 1) * TAPS];
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let row = &mut table[p * TAPS..(p + 1) * TAPS];
            let mut sum = 0.0;
            let mut h = [0f64; TAPS];
            for (k, hk) in h.iter_mut().enumerate() {
                // Tap k is the input frame at floor(pos) − HALF + 1 + k; the output is `frac` past floor(pos).
                let x = k as f64 - (HALF as f64 - 1.0) - frac;
                let w = {
                    let r = x / HALF as f64;
                    if r.abs() >= 1.0 { 0.0 } else { bessel_i0(beta * (1.0 - r * r).sqrt()) / bessel_i0(beta) }
                };
                let s = if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * cutoff * x) };
                *hk = cutoff * s * w;
                sum += *hk;
            }
            for (k, c) in row.iter_mut().enumerate() {
                *c = (h[k] / sum) as f32;
            }
        }
        Resampler { step, pos: (HALF - 1) as f64, buf: vec![0.0; (HALF - 1) * 2], table }
    }

    fn buffered(&self) -> usize {
        self.buf.len() / 2
    }

    /// The input frames `process` needs to make `out_frames` now.
    pub fn input_needed(&self, out_frames: usize) -> usize {
        if out_frames == 0 {
            return 0;
        }
        let last = self.pos + (out_frames - 1) as f64 * self.step;
        (last.floor() as usize + HALF + 1).saturating_sub(self.buffered())
    }

    /// How far behind the next input frame the next output frame is, in input frames: the first
    /// frame of the next `process`'s input is heard this long after its first output.
    pub fn lag(&self) -> f64 {
        self.buffered() as f64 - self.pos
    }

    /// `input`: exactly `input_needed(out.len() / 2)` frames, interleaved stereo. `out`: interleaved
    /// stereo at the output's rate.
    pub fn process(&mut self, input: &[f32], out: &mut [f32]) {
        self.buf.extend_from_slice(input);
        let frames = self.buffered();
        for o in out.chunks_exact_mut(2) {
            let base = self.pos.floor();
            let i = base as usize;
            let phase = (self.pos - base) * PHASES as f64;
            let p = (phase as usize).min(PHASES - 1);
            let t = (phase - p as f64) as f32;
            let (mut l, mut r) = (0f32, 0f32);
            if i + HALF < frames && i + 1 >= HALF {
                let first = i + 1 - HALF;
                let a = &self.table[p * TAPS..(p + 1) * TAPS];
                let b = &self.table[(p + 1) * TAPS..(p + 2) * TAPS];
                let x = &self.buf[first * 2..(first + TAPS) * 2];
                for k in 0..TAPS {
                    let c = a[k] + (b[k] - a[k]) * t;
                    l += c * x[2 * k];
                    r += c * x[2 * k + 1];
                }
            }
            o[0] = l;
            o[1] = r;
            self.pos += self.step;
        }
        // What every tap has passed goes.
        let done = (self.pos.floor() as usize + 1).saturating_sub(HALF).min(frames);
        self.buf.drain(..done * 2);
        self.pos -= done as f64;
    }
}
