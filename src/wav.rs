//! Small wav helpers shared by the CLI and the tests.
use crate::SR;
use anyhow::{bail, Context, Result};
use std::path::Path;

/// Reads a wav as mono f32 in [-1, 1]; requires 48 kHz (resample elsewhere).
pub fn read_mono_48k(path: &Path) -> Result<Vec<f32>> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = r.spec();
    if spec.sample_rate != SR {
        bail!("{}: expected {SR} Hz, got {} Hz (resample first)", path.display(), spec.sample_rate);
    }
    let ch = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u32 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    Ok(samples.chunks_exact(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect())
}

/// Writes mono 16-bit 48 kHz.
pub fn write_mono_48k_i16(path: &Path, audio: &[f32]) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for v in audio {
        w.write_sample((v.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
    }
    Ok(w.finalize()?)
}
