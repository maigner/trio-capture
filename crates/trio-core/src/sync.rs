//! Audio-based sync: where does a clip's audio sit inside the master WAV?
//!
//! The clip is cut into short chunks. Every chunk is matched against the
//! whole master by normalized cross-correlation of loudness envelopes (an
//! FFT per chunk, the master spectrum is computed once). Chunks then vote:
//! the offset most chunks agree on wins, and the share of agreeing chunks is
//! the confidence. This is robust to partial overlap (a clip that starts
//! before the master), to sections the master does not contain, and to a
//! song that appears twice (soundcheck and show), because a wrong peak only
//! ever convinces a few chunks.
//!
//! Clips of one camera are then arranged jointly ([`arrange`]): they were
//! recorded one after another, so they can never overlap, and the best
//! non-overlapping combination of candidates is chosen.

use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::Arc;

/// Sample rate expected for all PCM handed to this module.
pub const SYNC_RATE: u32 = 8000;
/// Envelope bin: 10 ms.
const HOP: usize = 80;
const ENV_RATE: f64 = SYNC_RATE as f64 / HOP as f64;
/// Chunk length and stride in envelope bins (15 s / 10 s).
const CHUNK_BINS: usize = 1500;
const CHUNK_STRIDE: usize = 1000;
const MIN_CHUNK_BINS: usize = 500;
/// Votes within this distance belong to the same candidate (±0.5 s).
const CLUSTER_TOL: i64 = 50;
/// A chunk whose best correlation is weaker than this did not match anything.
const MIN_CHUNK_SCORE: f32 = 0.2;
const MAX_CANDIDATES: usize = 4;
/// Candidates below this confidence are not used to place a clip.
pub const MIN_CONFIDENCE: f32 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncResult {
    /// Seconds into the master where the clip starts. Negative = clip starts earlier.
    pub offset: f64,
    /// 0..1: share of the clip's chunks that agree on this offset.
    pub confidence: f32,
}

/// The master WAV, prepared once for matching many clips against it.
pub struct Master {
    pcm: Arc<Vec<f32>>,
    env: Vec<f32>,
    spec: Vec<Complex<f32>>,
    n: usize,
    /// Prefix sums of the envelope and of its square, for local variance.
    sum1: Vec<f64>,
    sum2: Vec<f64>,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
}

/// One chunk's verdict, kept for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct ChunkVote {
    /// Clip-relative start of the chunk in seconds.
    pub at: f64,
    /// Offset of the clip start implied by this chunk's best match.
    pub offset: f64,
    /// Normalized correlation at that match, 0..1.
    pub score: f32,
}

impl Master {
    pub fn new(pcm: Arc<Vec<f32>>) -> Self {
        let mut env = envelope(&pcm, HOP);
        let nb = env.len();
        let mean = env.iter().sum::<f32>() / nb.max(1) as f32;
        for e in &mut env {
            *e -= mean;
        }
        let n = (nb + CHUNK_BINS).next_power_of_two().max(2);
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        let mut spec: Vec<Complex<f32>> = env.iter().map(|&x| Complex::new(x, 0.0)).collect();
        spec.resize(n, Complex::new(0.0, 0.0));
        fft.process(&mut spec);
        let mut sum1 = Vec::with_capacity(nb + 1);
        let mut sum2 = Vec::with_capacity(nb + 1);
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        sum1.push(0.0);
        sum2.push(0.0);
        for &x in &env {
            s1 += x as f64;
            s2 += (x as f64) * (x as f64);
            sum1.push(s1);
            sum2.push(s2);
        }
        Self {
            pcm,
            env,
            spec,
            n,
            sum1,
            sum2,
            fft,
            ifft,
        }
    }

    pub fn duration(&self) -> f64 {
        self.pcm.len() as f64 / SYNC_RATE as f64
    }

    /// Variance-like energy of the master envelope over `[start, start+len)`,
    /// clipped to the valid range.
    fn local_energy(&self, start: i64, len: usize) -> f64 {
        let nb = self.env.len() as i64;
        let a = start.clamp(0, nb) as usize;
        let b = (start + len as i64).clamp(0, nb) as usize;
        if b <= a {
            return 0.0;
        }
        let m = (b - a) as f64;
        let s1 = self.sum1[b] - self.sum1[a];
        let s2 = self.sum2[b] - self.sum2[a];
        (s2 - s1 * s1 / m).max(0.0)
    }

    /// Best offset for one chunk of clip envelope: (lag in bins, normalized score).
    fn match_chunk(&self, chunk: &[f32]) -> Option<(i64, f32)> {
        let m = chunk.len();
        let nb = self.env.len();
        let mean = chunk.iter().sum::<f32>() / m as f32;
        let mut fa: Vec<Complex<f32>> =
            chunk.iter().map(|&x| Complex::new(x - mean, 0.0)).collect();
        let norm_a = fa.iter().map(|c| c.re * c.re).sum::<f32>().sqrt();
        if norm_a < 1e-6 {
            return None;
        }
        fa.resize(self.n, Complex::new(0.0, 0.0));
        self.fft.process(&mut fa);
        for (x, y) in fa.iter_mut().zip(&self.spec) {
            *x = x.conj() * *y;
        }
        self.ifft.process(&mut fa);
        let scale = 1.0 / self.n as f32;

        // Only lags where the whole chunk lies inside the master: a partial
        // overlap breaks the normalization and produces bogus peaks.
        if m > nb {
            return None;
        }
        // Guard against silent master stretches inflating the score.
        let floor = 0.05 * m as f64 * self.mean_energy();
        let mut best = (0i64, f32::MIN);
        for lag in 0..=(nb - m) as i64 {
            let c = fa[lag as usize].re * scale;
            let e = self.local_energy(lag, m).max(floor);
            let v = c / (norm_a * e.sqrt() as f32);
            if v > best.1 {
                best = (lag, v);
            }
        }
        Some(best)
    }

    fn mean_energy(&self) -> f64 {
        let nb = self.env.len();
        if nb == 0 {
            return 1e-6;
        }
        (self.sum2[nb] / nb as f64).max(1e-6)
    }

    /// Every chunk's vote for the clip's offset.
    pub fn votes(&self, clip: &[f32]) -> Vec<ChunkVote> {
        let env = envelope(clip, HOP);
        let mut out = Vec::new();
        let mut start = 0usize;
        while start + MIN_CHUNK_BINS <= env.len() {
            let end = (start + CHUNK_BINS).min(env.len());
            if let Some((lag, score)) = self.match_chunk(&env[start..end]) {
                out.push(ChunkVote {
                    at: start as f64 / ENV_RATE,
                    offset: (lag - start as i64) as f64 / ENV_RATE,
                    score,
                });
            }
            if end == env.len() {
                break;
            }
            start += CHUNK_STRIDE;
        }
        out
    }

    /// Candidate offsets for `clip`, best first. Empty when nothing matched.
    pub fn candidates(&self, clip: &[f32]) -> Vec<SyncResult> {
        let votes = self.votes(clip);
        self.candidates_from_votes(clip, &votes)
    }

    pub fn candidates_from_votes(&self, clip: &[f32], votes: &[ChunkVote]) -> Vec<SyncResult> {
        let clip_len = clip.len() as f64 / SYNC_RATE as f64;
        let master_len = self.duration();
        let chunk_len = CHUNK_BINS as f64 / ENV_RATE;
        let tol = CLUSTER_TOL as f64 / ENV_RATE;
        let mut used = vec![false; votes.len()];
        let mut out = Vec::new();

        for _ in 0..MAX_CANDIDATES {
            // Densest cluster of unused, matching votes.
            let mut best: Option<(usize, f32)> = None;
            for (i, v) in votes.iter().enumerate() {
                if used[i] || v.score < MIN_CHUNK_SCORE {
                    continue;
                }
                let sum: f32 = votes
                    .iter()
                    .enumerate()
                    .filter(|(j, w)| {
                        !used[*j]
                            && w.score >= MIN_CHUNK_SCORE
                            && (w.offset - v.offset).abs() <= tol
                    })
                    .map(|(_, w)| w.score)
                    .sum();
                if best.map(|b| sum > b.1).unwrap_or(true) {
                    best = Some((i, sum));
                }
            }
            let Some((center_i, _)) = best else { break };
            let center = votes[center_i].offset;
            let members: Vec<usize> = (0..votes.len())
                .filter(|&j| {
                    !used[j]
                        && votes[j].score >= MIN_CHUNK_SCORE
                        && (votes[j].offset - center).abs() <= tol
                })
                .collect();
            for &j in &members {
                used[j] = true;
            }

            // Offset at the clip start: robust line fit through the members
            // absorbs clock drift; a lone member just gives its own value.
            let offset = fit_offset_at_start(votes, &members);

            // Confidence: matching share of the chunks that could overlap the master.
            let overlapping: Vec<usize> = (0..votes.len())
                .filter(|&j| {
                    let s = votes[j].at + offset;
                    let e = (votes[j].at + chunk_len).min(clip_len) + offset;
                    s >= 0.0 && e <= master_len
                })
                .collect();
            let denom: f32 = overlapping
                .iter()
                .map(|&j| votes[j].score.max(MIN_CHUNK_SCORE))
                .sum();
            let num: f32 = members.iter().map(|&j| votes[j].score).sum();
            let confidence = if denom > 0.0 {
                (num / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };

            let offset = self.refine(clip, votes, &members, offset);
            out.push(SyncResult { offset, confidence });
        }
        out.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap());
        out
    }

    /// Sub-frame precision: correlate raw PCM of the strongest matching
    /// chunks in a ±20 ms window around the envelope result.
    fn refine(&self, clip: &[f32], votes: &[ChunkVote], members: &[usize], offset: f64) -> f64 {
        let mut strongest: Vec<usize> = members.to_vec();
        strongest.sort_by(|&a, &b| votes[b].score.partial_cmp(&votes[a].score).unwrap());
        strongest.truncate(3);
        if strongest.is_empty() {
            return offset;
        }
        let rate = SYNC_RATE as f64;
        let seg_len = 10 * SYNC_RATE as usize;
        let center = (offset * rate).round() as i64;
        let radius = 2 * HOP as i64;
        let master = &self.pcm;
        let mut best = (center, f32::MIN);
        for lag in (center - radius)..=(center + radius) {
            let mut total = 0.0f32;
            let mut n = 0usize;
            for &j in &strongest {
                let a0 = (votes[j].at * rate) as usize;
                let a1 = (a0 + seg_len).min(clip.len());
                for i in a0..a1 {
                    let k = i as i64 + lag;
                    if k < 0 || k >= master.len() as i64 {
                        continue;
                    }
                    total += clip[i] * master[k as usize];
                    n += 1;
                }
            }
            if n > 0 {
                let v = total / n as f32;
                if v > best.1 {
                    best = (lag, v);
                }
            }
        }
        best.0 as f64 / rate
    }
}

/// Offset at clip time 0 from a set of votes. Least squares in `at` when
/// there are enough members and the slope is plausible drift, else median.
fn fit_offset_at_start(votes: &[ChunkVote], members: &[usize]) -> f64 {
    let mut offs: Vec<f64> = members.iter().map(|&j| votes[j].offset).collect();
    offs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = offs[offs.len() / 2];
    if members.len() < 4 {
        return median;
    }
    let n = members.len() as f64;
    let mx = members.iter().map(|&j| votes[j].at).sum::<f64>() / n;
    let my = members.iter().map(|&j| votes[j].offset).sum::<f64>() / n;
    let sxy: f64 = members
        .iter()
        .map(|&j| (votes[j].at - mx) * (votes[j].offset - my))
        .sum();
    let sxx: f64 = members.iter().map(|&j| (votes[j].at - mx).powi(2)).sum();
    if sxx <= 0.0 {
        return median;
    }
    let slope = sxy / sxx;
    // More than 500 ppm is not clock drift; trust the median instead.
    if slope.abs() > 5e-4 {
        return median;
    }
    my - slope * mx
}

/// RMS envelope in the log domain so quiet passages still count.
pub fn envelope(samples: &[f32], hop: usize) -> Vec<f32> {
    samples
        .chunks(hop)
        .map(|c| {
            let rms = (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt();
            (rms + 1e-4).ln()
        })
        .collect()
}

/// Locate `clip` inside `master` (both mono at [`SYNC_RATE`]); best candidate only.
pub fn find_offset(clip: &[f32], master: &[f32]) -> Option<SyncResult> {
    if clip.len() < MIN_CHUNK_BINS * HOP || master.len() < MIN_CHUNK_BINS * HOP {
        return None;
    }
    let m = Master::new(Arc::new(master.to_vec()));
    m.candidates(clip).into_iter().next()
}

/// What [`arrange`] needs to know about a clip besides its candidates.
#[derive(Debug, Clone, Copy)]
pub struct ClipInfo {
    pub duration: f64,
    /// Recording start in seconds on any camera-consistent clock.
    pub start_time: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// Matched by audio.
    Audio,
    /// No usable match; positioned from timestamps next to a matched sibling.
    Timestamp,
    /// No match and nothing to lean on; offset left unchanged.
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub struct Arranged {
    pub offset: f64,
    pub confidence: f32,
    pub placement: Placement,
}

/// Consecutive clips may overlap by this much (metadata rounding).
const OVERLAP_TOL: f64 = 0.5;
/// Bonus when the found gap between two clips agrees with their timestamps.
const TIMESTAMP_BONUS: f32 = 0.15;
const TIMESTAMP_TOL: f64 = 3.0;

/// Pick one candidate per clip so that clips (in recording order) never
/// overlap, maximizing total confidence. `current` supplies fallback offsets.
pub fn arrange(
    clips: &[ClipInfo],
    candidates: &[Vec<SyncResult>],
    current: &[f64],
) -> Vec<Arranged> {
    let n = clips.len();
    let usable: Vec<Vec<SyncResult>> = candidates
        .iter()
        .map(|c| {
            c.iter()
                .filter(|r| r.confidence >= MIN_CONFIDENCE)
                .copied()
                .collect()
        })
        .collect();

    // Dynamic programme over clips in recording order. A state is the last
    // placed clip and its candidate (`None` = nothing placed yet); each layer
    // maps states after clip i to (score, state before clip i).
    type State = Option<(usize, usize)>;
    type Layer = std::collections::HashMap<State, (f32, State)>;
    let mut layers: Vec<Layer> = Vec::with_capacity(n);
    let mut cur: Layer = Layer::new();
    cur.insert(None, (0.0, None));

    for i in 0..n {
        let mut next = Layer::new();
        let mut relax = |key: State, score: f32, from: State| {
            let e = next.entry(key).or_insert((f32::MIN, None));
            if score > e.0 {
                *e = (score, from);
            }
        };
        for (&state, &(score, _)) in &cur {
            // Skip clip i: it will be placed from timestamps later.
            relax(state, score, state);
            for (ci, cand) in usable[i].iter().enumerate() {
                let mut s = score + cand.confidence;
                if let Some((j, cj)) = state {
                    let prev = usable[j][cj];
                    let span: f64 = (j..i).map(|k| clips[k].duration).sum();
                    if cand.offset < prev.offset + span - OVERLAP_TOL {
                        continue;
                    }
                    if let (Some(a), Some(b)) = (clips[j].start_time, clips[i].start_time) {
                        if ((cand.offset - prev.offset) - (b - a)).abs() <= TIMESTAMP_TOL {
                            s += TIMESTAMP_BONUS;
                        }
                    }
                }
                relax(Some((i, ci)), s, state);
            }
        }
        layers.push(next.clone());
        cur = next;
    }

    let mut chosen: Vec<Option<usize>> = vec![None; n];
    let mut state: State = cur
        .iter()
        .max_by(|a, b| a.1 .0.partial_cmp(&b.1 .0).unwrap())
        .map(|(s, _)| *s)
        .unwrap_or(None);
    for i in (0..n).rev() {
        if let Some((k, ci)) = state {
            if k == i {
                chosen[i] = Some(ci);
            }
        }
        state = layers[i][&state].1;
    }

    // Fill unplaced clips from timestamps relative to the nearest placed sibling.
    let mut out: Vec<Arranged> = (0..n)
        .map(|i| match chosen[i] {
            Some(ci) => Arranged {
                offset: usable[i][ci].offset,
                confidence: usable[i][ci].confidence,
                placement: Placement::Audio,
            },
            None => Arranged {
                offset: current.get(i).copied().unwrap_or(0.0),
                confidence: 0.0,
                placement: Placement::Unknown,
            },
        })
        .collect();
    let placed: Vec<usize> = (0..n).filter(|&i| chosen[i].is_some()).collect();
    for i in 0..n {
        if chosen[i].is_some() || placed.is_empty() {
            continue;
        }
        let anchor = placed
            .iter()
            .copied()
            .min_by_key(|&j| (j as i64 - i as i64).unsigned_abs())
            .unwrap();
        let by_ts = match (clips[anchor].start_time, clips[i].start_time) {
            (Some(a), Some(b)) => Some(out[anchor].offset + (b - a)),
            _ => None,
        };
        let offset = by_ts.unwrap_or_else(|| {
            if i > anchor {
                out[anchor].offset + (anchor..i).map(|k| clips[k].duration).sum::<f64>()
            } else {
                out[anchor].offset - (i..anchor).map(|k| clips[k].duration).sum::<f64>()
            }
        });
        out[i] = Arranged {
            offset,
            confidence: 0.0,
            placement: Placement::Timestamp,
        };
    }
    out
}

/// Recording start times for a camera's clips from their `creation_time`
/// stamps. Some phones stamp the end of the recording instead; that shows
/// as a gap shorter than the clip, and then all stamps are shifted back.
pub fn start_times(creation: &[Option<f64>], durations: &[f64]) -> Vec<Option<f64>> {
    let ends_stamped = creation
        .windows(2)
        .zip(durations)
        .any(|(w, &d)| match (w[0], w[1]) {
            (Some(a), Some(b)) => b - a < d - 1.0,
            _ => false,
        });
    creation
        .iter()
        .zip(durations)
        .map(|(c, &d)| c.map(|t| if ends_stamped { t - d } else { t }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    /// Music-like signal: white-noise carrier under a slowly varying,
    /// aperiodic loudness envelope (low-passed noise).
    fn music(n: usize, seed: u64) -> Vec<f32> {
        let rate = SYNC_RATE as usize;
        let carrier = noise(n, seed);
        let slow = noise(n, seed.wrapping_mul(31) + 1);
        let mut env = 0.0f32;
        let alpha = 1.0 / (0.15 * rate as f32); // ~150 ms time constant
        carrier
            .iter()
            .zip(&slow)
            .map(|(c, s)| {
                env += alpha * (s - env);
                c * (0.05 + (env.abs() * 40.0).min(1.0))
            })
            .collect()
    }

    #[test]
    fn finds_clip_inside_master() {
        let rate = SYNC_RATE as usize;
        let master = music(120 * rate, 42);
        // Clip: 40 s starting at 23.4 s, with independent noise and gain.
        let start = (23.4 * rate as f64) as usize;
        let extra = noise(40 * rate, 7);
        let clip: Vec<f32> = master[start..start + 40 * rate]
            .iter()
            .zip(&extra)
            .map(|(m, e)| 0.5 * m + 0.15 * e)
            .collect();
        let r = find_offset(&clip, &master).unwrap();
        assert!((r.offset - 23.4).abs() < 0.002, "offset {}", r.offset);
        assert!(r.confidence > 0.8, "confidence {}", r.confidence);
    }

    #[test]
    fn handles_clip_starting_before_master() {
        let rate = SYNC_RATE as usize;
        let long = music(120 * rate, 3);
        let master = long[30 * rate..].to_vec();
        let clip = long[..60 * rate].to_vec();
        let r = find_offset(&clip, &master).unwrap();
        assert!((r.offset + 30.0).abs() < 0.002, "offset {}", r.offset);
        assert!(r.confidence > 0.8, "confidence {}", r.confidence);
    }

    #[test]
    fn repeated_song_does_not_fool_the_vote() {
        // Master = A B A' where A' repeats A; clip = A B, which only fits at 0.
        let rate = SYNC_RATE as usize;
        let a = music(40 * rate, 11);
        let b = music(40 * rate, 12);
        let mut master = a.clone();
        master.extend_from_slice(&b);
        master.extend_from_slice(&a);
        let mut clip = a.clone();
        clip.extend_from_slice(&b);
        let m = Master::new(Arc::new(master));
        let c = m.candidates(&clip);
        assert!((c[0].offset).abs() < 0.002, "{c:?}");
        assert!(c[0].confidence > c.get(1).map(|x| x.confidence).unwrap_or(0.0));
    }

    #[test]
    fn arrange_rejects_overlap() {
        let clips = [
            ClipInfo {
                duration: 100.0,
                start_time: Some(0.0),
            },
            ClipInfo {
                duration: 50.0,
                start_time: Some(120.0),
            },
        ];
        // Clip 0's strongest candidate would overlap clip 1's strong match.
        let cands = vec![
            vec![
                SyncResult {
                    offset: 300.0,
                    confidence: 0.5,
                },
                SyncResult {
                    offset: 200.0,
                    confidence: 0.4,
                },
            ],
            vec![SyncResult {
                offset: 320.0,
                confidence: 0.9,
            }],
        ];
        let r = arrange(&clips, &cands, &[0.0, 0.0]);
        assert_eq!(r[0].offset, 200.0);
        assert_eq!(r[1].offset, 320.0);
        assert_eq!(r[0].placement, Placement::Audio);
    }

    #[test]
    fn arrange_places_unmatched_by_timestamp() {
        let clips = [
            ClipInfo {
                duration: 100.0,
                start_time: Some(1000.0),
            },
            ClipInfo {
                duration: 50.0,
                start_time: Some(1130.0),
            },
        ];
        let cands = vec![
            vec![],
            vec![SyncResult {
                offset: 500.0,
                confidence: 0.7,
            }],
        ];
        let r = arrange(&clips, &cands, &[0.0, 0.0]);
        assert_eq!(r[0].placement, Placement::Timestamp);
        assert!((r[0].offset - 370.0).abs() < 1e-9);
        assert_eq!(r[0].confidence, 0.0);
    }

    #[test]
    fn detects_end_stamped_creation_times() {
        // Android: stamps 1000 and 1500 with a 900 s first clip → stamps are ends.
        let s = start_times(&[Some(1000.0), Some(1500.0)], &[900.0, 400.0]);
        assert_eq!(s, vec![Some(100.0), Some(1100.0)]);
        // GoPro: stamps 1000 and 2000 with a 900 s first clip → starts.
        let s = start_times(&[Some(1000.0), Some(2000.0)], &[900.0, 400.0]);
        assert_eq!(s, vec![Some(1000.0), Some(2000.0)]);
    }
}
