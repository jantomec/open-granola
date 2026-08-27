//! Smoke test for the macOS system-audio tap: capture for a few seconds
//! while something plays through the speakers, then report what arrived.
//!
//!     cargo run --example tap_smoke [seconds]
//!
//! Prints sample count, RMS and peak of the captured 16 kHz mono stream.
//! Silence (rms exactly 0) with audio playing means TCC denied the capture.
//!
//! IMPORTANT: run from a bare terminal and macOS will deny silently — the
//! System Audio Recording permission needs a bundle identity carrying
//! `NSAudioCaptureUsageDescription` before it will even prompt. Wrap the
//! binary in a minimal .app (Info.plist with that key + CFBundleIdentifier,
//! binary in Contents/MacOS/), `codesign -s -` it, and launch via
//! `open -W --stdout out.log --stderr err.log TapSmoke.app --args 10`.
//! The real app is fine: Tauri embeds src-tauri/Info.plist into every build.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ringbuf::traits::*;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::new().filter_level(log::LevelFilter::Info).init();
    let secs: u64 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(4);

    let rb = ringbuf::HeapRb::<f32>::new(16_000 * 60);
    let (producer, mut consumer) = rb.split();
    let producer = Arc::new(parking_lot::Mutex::new(producer));
    let stop = Arc::new(AtomicBool::new(false));

    let handle = open_granola_lib::audio::loopback::start(producer, stop.clone())?;
    println!("tap running — capturing {secs} s of system audio…");
    std::thread::sleep(std::time::Duration::from_secs(secs));
    stop.store(true, Ordering::Relaxed);
    drop(handle);

    let mut samples = Vec::new();
    while let Some(s) = consumer.try_pop() {
        samples.push(s);
    }
    let n = samples.len();
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / n.max(1) as f32).sqrt();
    let peak = samples.iter().fold(0f32, |a, s| a.max(s.abs()));
    println!(
        "captured {n} samples = {:.2} s at 16 kHz | rms {rms:.6} | peak {peak:.6}",
        n as f32 / 16_000.0
    );
    Ok(())
}
