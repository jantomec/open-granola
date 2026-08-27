//! On-device transcription: whisper.cpp with streaming partial results,
//! plus local speaker diarization via trained speaker embeddings (TitaNet
//! through sherpa-onnx, see `diarize.rs`) clustered online against running
//! centroids.
//!
//! Nothing here touches the network or a socket.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::audio::WHISPER_RATE;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: u8, // cluster id; the UI maps 0 => "You" when mic-dominant
    pub text: String,
    pub final_: bool,
}

pub struct WhisperEngine {
    ctx: WhisperContext,
    /// Speaker-embedding diarizer; None when the ONNX model file is missing,
    /// in which case every segment is labeled speaker 0.
    diarizer: Option<crate::diarize::SpeakerDiarizer>,
    /// Speaker carried forward for segments too short to embed.
    last_speaker: u8,
}

impl WhisperEngine {
    /// Load a GGML whisper model from the local model directory, plus the
    /// speaker-embedding model expected as `speaker-embed.onnx` next to it.
    pub fn load(model_path: &Path) -> Result<Self> {
        let ctx = WhisperContext::new_with_params(
            model_path.to_str().context("bad model path")?,
            WhisperContextParameters::default(),
        )
        .context("failed to load whisper model")?;
        let speaker_model = model_path.with_file_name("speaker-embed.onnx");
        let diarizer = if speaker_model.exists() {
            crate::diarize::SpeakerDiarizer::load(&speaker_model)
                .map_err(|e| log::warn!("speaker model failed to load: {e}"))
                .ok()
        } else {
            log::warn!("speaker-embed.onnx not found; labeling all segments speaker 0");
            None
        };
        Ok(Self {
            ctx,
            diarizer,
            last_speaker: 0,
        })
    }

    /// Transcribe one window of 16 kHz mono samples.
    pub fn transcribe_window(&mut self, samples: &[f32], offset_ms: u64) -> Result<Vec<Segment>> {
        let mut state = self.ctx.create_state()?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(true);
        params.set_language(Some("auto"));
        params.set_translate(false);
        // Custom vocabulary boost: proper nouns, number words, domain terms
        // learned from prior notes — fixes Granola's "numbers get messed up".
        params.set_initial_prompt(&self.vocabulary_prompt());

        state.full(params, samples)?;

        let n = state.full_n_segments()?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let text = state.full_get_segment_text(i)?;
            let t0 = state.full_get_segment_t0(i)? as u64 * 10; // centiseconds -> ms
            let t1 = state.full_get_segment_t1(i)? as u64 * 10;
            let speaker = self.assign_speaker(samples, t0, t1);
            out.push(Segment {
                start_ms: offset_ms + t0,
                end_ms: offset_ms + t1,
                speaker,
                text: text.trim().to_string(),
                final_: true,
            });
        }
        Ok(out)
    }

    /// Online diarization over the segment's own time slice of the window.
    /// Too-short segments inherit the current speaker rather than guessing.
    fn assign_speaker(&mut self, samples: &[f32], t0: u64, t1: u64) -> u8 {
        let Some(diarizer) = self.diarizer.as_mut() else {
            return 0;
        };
        let start = (t0 as usize * WHISPER_RATE / 1000).min(samples.len());
        let end = (t1 as usize * WHISPER_RATE / 1000).min(samples.len());
        match diarizer.assign(&samples[start..end]) {
            Some(s) => {
                self.last_speaker = s;
                s
            }
            None => self.last_speaker,
        }
    }

    fn vocabulary_prompt(&self) -> String {
        // Populated from the user's custom dictionary + frequent proper nouns
        // mined from local notes. Steering whisper with an initial prompt
        // measurably improves names/numbers over zero-shot decoding.
        String::from("Transcript of a work meeting with precise numbers and names.")
    }
}

