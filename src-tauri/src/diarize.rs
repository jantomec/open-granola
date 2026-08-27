//! Speaker diarization: real speaker embeddings (NeMo TitaNet-small via
//! sherpa-onnx), clustered online against running centroids.
//!
//! Replaces the earlier spectral-envelope heuristic, which captured loudness
//! and room tone rather than voice identity. Fully on-device, like everything
//! else in this crate — the model is a local ONNX file in the library folder.

use anyhow::{Context, Result};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use std::path::Path;

/// Segments shorter than this carry too little voice to embed reliably.
const MIN_SAMPLES: usize = 16_000 * 2 / 5; // 0.4 s at 16 kHz
/// Cosine similarity above which a segment joins an existing speaker cluster.
const SAME_SPEAKER: f32 = 0.5;

pub struct SpeakerDiarizer {
    extractor: SpeakerEmbeddingExtractor,
    centroids: Vec<Vec<f32>>,
}

impl SpeakerDiarizer {
    pub fn load(model_path: &Path) -> Result<Self> {
        let config = SpeakerEmbeddingExtractorConfig {
            model: Some(
                model_path
                    .to_str()
                    .context("bad speaker model path")?
                    .to_string(),
            ),
            num_threads: 2,
            debug: false,
            provider: None,
        };
        let extractor = SpeakerEmbeddingExtractor::create(&config)
            .context("failed to load speaker embedding model")?;
        Ok(Self {
            extractor,
            centroids: Vec::new(),
        })
    }

    /// L2-normalized speaker embedding of one 16 kHz mono segment, or None
    /// when the segment is too short to embed.
    pub fn embed(&self, samples: &[f32]) -> Option<Vec<f32>> {
        if samples.len() < MIN_SAMPLES {
            return None;
        }
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(16_000, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        Some(normalized(&self.extractor.compute(&stream)?))
    }

    /// Assign one segment to a speaker cluster, opening a new cluster when
    /// nothing similar enough exists yet. None = segment too short to judge.
    pub fn assign(&mut self, samples: &[f32]) -> Option<u8> {
        let emb = self.embed(samples)?;
        let mut best: Option<(usize, f32)> = None;
        for (i, c) in self.centroids.iter().enumerate() {
            let sim = dot(&emb, c);
            if best.map(|(_, s)| sim > s).unwrap_or(true) {
                best = Some((i, sim));
            }
        }
        Some(match best {
            Some((i, sim)) if sim > SAME_SPEAKER => {
                // nudge centroid toward the new observation (EMA, alpha 0.1)
                let c = &mut self.centroids[i];
                for (cv, ev) in c.iter_mut().zip(&emb) {
                    *cv = *cv * 0.9 + ev * 0.1;
                }
                *c = normalized(c);
                i as u8
            }
            _ => {
                self.centroids.push(emb);
                (self.centroids.len() - 1) as u8
            }
        })
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalized(v: &[f32]) -> Vec<f32> {
    let n = dot(v, v).sqrt().max(1e-6);
    v.iter().map(|x| x / n).collect()
}
