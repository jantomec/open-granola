//! Measures whether the speaker-embedding diarizer actually separates voices.
//!
//! Usage: cargo run --example diarize_check -- <speaker-embed.onnx> <wav-dir>
//!
//! The wav dir holds 16 kHz mono 16-bit files named `<voice>_<n>.wav`.
//! Reports within-voice vs cross-voice cosine similarity and the labels the
//! online clusterer assigns when utterances arrive interleaved.

use open_granola_lib::diarize::SpeakerDiarizer;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let model = args.next().expect("arg 1: speaker model path");
    let dir = args.next().expect("arg 2: wav dir");

    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
        .collect();
    // Interleave voices the way a conversation would.
    files.sort_by_key(|p| {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let (voice, n) = stem.rsplit_once('_').unwrap_or((stem.as_str(), "0"));
        (n.parse::<u32>().unwrap_or(0), voice.to_string())
    });

    let mut diarizer = SpeakerDiarizer::load(model.as_ref())?;
    let mut embeddings: Vec<(String, Vec<f32>)> = Vec::new();
    let mut labels: Vec<(String, u8)> = Vec::new();

    for path in &files {
        let mut reader = hound::WavReader::open(path)?;
        assert_eq!(reader.spec().sample_rate, 16_000, "expect 16 kHz input");
        assert_eq!(reader.spec().channels, 1, "expect mono input");
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect();
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let voice = name.rsplit_once('_').unwrap().0.to_string();
        let emb = diarizer
            .embed(&samples)
            .expect("utterance long enough to embed");
        embeddings.push((voice.clone(), emb));
        let label = diarizer.assign(&samples).expect("assign");
        labels.push((name, label));
    }

    let cos = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
    let (mut within, mut cross) = (Vec::new(), Vec::new());
    for i in 0..embeddings.len() {
        for j in (i + 1)..embeddings.len() {
            let sim = cos(&embeddings[i].1, &embeddings[j].1);
            if embeddings[i].0 == embeddings[j].0 {
                within.push(sim);
            } else {
                cross.push(sim);
            }
        }
    }
    let stats = |v: &[f32]| {
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let min = v.iter().cloned().fold(f32::MAX, f32::min);
        let max = v.iter().cloned().fold(f32::MIN, f32::max);
        (mean, min, max)
    };
    let (wm, wmin, wmax) = stats(&within);
    let (cm, cmin, cmax) = stats(&cross);
    println!("within-voice cosine: mean {wm:.3}  min {wmin:.3}  max {wmax:.3}  (n={})", within.len());
    println!("cross-voice cosine:  mean {cm:.3}  min {cmin:.3}  max {cmax:.3}  (n={})", cross.len());
    println!("separation margin (within.min - cross.max): {:.3}", wmin - cmax);
    println!("\nonline cluster labels (interleaved order):");
    for (name, label) in &labels {
        println!("  {name} -> speaker {label}");
    }

    // Verdict: every voice maps to exactly one label and labels don't collide.
    let mut by_voice: std::collections::HashMap<&str, std::collections::HashSet<u8>> =
        Default::default();
    for (name, label) in &labels {
        by_voice
            .entry(name.rsplit_once('_').unwrap().0)
            .or_default()
            .insert(*label);
    }
    let pure = by_voice.values().all(|s| s.len() == 1);
    let distinct: std::collections::HashSet<_> =
        by_voice.values().flatten().collect();
    let ok = pure && distinct.len() == by_voice.len();
    println!("\nVERDICT: {}", if ok { "PASS" } else { "FAIL" });
    std::process::exit(if ok { 0 } else { 1 });
}
