//! Audio capture — bot-free, like Granola, but the bytes never leave RAM.
//!
//! Two sources are captured concurrently and mixed:
//!   * mic     — cpal default input (you)
//!   * system  — platform loopback: CoreAudio process-tap (macOS 14.4+),
//!               WASAPI loopback (Windows), PipeWire monitor (Linux) (everyone else)
//!
//! Both are resampled to 16 kHz mono f32 — whisper.cpp's native format — and
//! pushed into a lock-free ring buffer that `transcribe` drains.
//!
//! RETENTION: samples live in heap buffers only. Unless the user enables
//! "keep raw audio", nothing in this module ever touches the disk.

mod loopback;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use ringbuf::{traits::*, HeapRb};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub const WHISPER_RATE: usize = 16_000;

/// Both capture callbacks (mic + loopback) feed the same single-producer ring
/// buffer, so the producer half lives behind a mutex they share.
pub(crate) type SharedProducer = Arc<Mutex<ringbuf::HeapProd<f32>>>;

/// One capture session = one meeting.
///
/// cpal streams are not `Send`, so a dedicated thread owns them for the whole
/// session; the session itself holds only the stop flag and thread handle,
/// which keeps `AppState` usable from Tauri's async commands.
pub struct CaptureSession {
    pub started: Instant,
    pub meeting_hint: Option<String>, // from local calendar, if matched
    stop_flag: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl CaptureSession {
    /// Begin capturing mic + system audio. Returns the session (owns the
    /// capture thread) and a consumer the transcription worker drains.
    pub fn begin(meeting_hint: Option<String>) -> Result<(Self, ringbuf::HeapCons<f32>)> {
        let rb = HeapRb::<f32>::new(WHISPER_RATE * 60 * 4); // 4 min headroom; drained continuously
        let (producer, consumer) = rb.split();
        let producer: SharedProducer = Arc::new(Mutex::new(producer));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let stop = stop_flag.clone();
        let worker = std::thread::spawn(move || match build_streams(producer, stop.clone()) {
            Ok(streams) => {
                let _ = ready_tx.send(Ok(()));
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                drop(streams); // audio ceases to exist
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        });
        ready_rx
            .recv()
            .context("capture thread exited during setup")??;

        Ok((
            Self {
                started: Instant::now(),
                meeting_hint,
                stop_flag,
                worker: Some(worker),
            },
            consumer,
        ))
    }

    /// Stop both streams. Buffers are dropped here — audio ceases to exist.
    pub fn finish(mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        log::info!(
            "capture stopped after {:?}; audio buffers dropped",
            self.started.elapsed()
        );
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        // A session dropped without finish() must still release the streams.
        self.stop_flag.store(true, Ordering::Relaxed);
    }
}

/// Built and owned by the capture thread — cpal streams must not cross threads.
fn build_streams(
    producer: SharedProducer,
    stop: Arc<AtomicBool>,
) -> Result<(cpal::Stream, Box<dyn std::any::Any>)> {
    use rubato::Resampler;

    // --- mic chain ---
    let host = cpal::default_host();
    let mic = host.default_input_device().context("no input device")?;
    let cfg = mic.default_input_config()?;
    let rate = cfg.sample_rate().0 as usize;
    let channels = cfg.channels() as usize;

    const CHUNK: usize = 1024;
    let mut resampler = rubato::FftFixedIn::<f32>::new(rate, WHISPER_RATE, CHUNK, 2, 1)?;
    let mic_producer = producer.clone();
    let mic_stop = stop.clone();
    // The resampler takes fixed-size frames; carry the remainder across callbacks.
    let mut pending: Vec<f32> = Vec::with_capacity(CHUNK * 4);
    let mic_stream = mic.build_input_stream(
        &cfg.into(),
        move |data: &[f32], _| {
            if mic_stop.load(Ordering::Relaxed) {
                return;
            }
            pending.extend(
                data.chunks(channels.max(1))
                    .map(|f| f.iter().sum::<f32>() / channels.max(1) as f32),
            );
            while pending.len() >= CHUNK {
                let frame: Vec<f32> = pending.drain(..CHUNK).collect();
                if let Ok(out) = resampler.process(&[frame], None) {
                    let _ = mic_producer.lock().push_slice(&out[0]); // drop on overflow, never block
                }
            }
        },
        |e| log::error!("mic stream error: {e}"),
        None,
    )?;
    mic_stream.play()?;

    // --- system loopback chain (platform-specific, see loopback.rs) ---
    let sys = loopback::start(producer, stop)?;

    Ok((mic_stream, sys))
}
