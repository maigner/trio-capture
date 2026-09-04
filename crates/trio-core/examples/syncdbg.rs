//! Debug helper: `syncdbg master.pcm clip.pcm...` (mono f32le at 8 kHz).
use std::sync::Arc;
use trio_core::sync::Master;

fn load(p: &str) -> Vec<f32> {
    let bytes = std::fs::read(p).expect("read");
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let t0 = std::time::Instant::now();
    let master = Master::new(Arc::new(load(&args[1])));
    println!(
        "master {:.1}s prepared in {:?}",
        master.duration(),
        t0.elapsed()
    );
    for p in &args[2..] {
        let clip = load(p);
        let t = std::time::Instant::now();
        let votes = master.votes(&clip);
        let cands = master.candidates_from_votes(&clip, &votes);
        println!(
            "== {p} ({:.1}s) in {:?}",
            clip.len() as f64 / 8000.0,
            t.elapsed()
        );
        for c in &cands {
            println!(
                "   candidate offset {:>9.3}  confidence {:.2}",
                c.offset, c.confidence
            );
        }
        if std::env::var("VOTES").is_ok() {
            for v in &votes {
                println!(
                    "   at {:>7.1}  offset {:>9.2}  score {:.2}",
                    v.at, v.offset, v.score
                );
            }
        }
    }
}
