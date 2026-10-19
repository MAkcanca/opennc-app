//! The Rust engine must reproduce the Python reference engine (OpenNC repo, inference/engine.py) on a public clip.
//! Data: tests/data/p232_001.wav (VoiceBank-DEMAND noisy test clip, 48 kHz) and p232_001.ref.wav (Python output, 16-bit).
//! Model: OPENNC_MODEL_DIR or models/opennc-v1 (the release zip, unpacked).
use opennc::{wav, Engine};
use std::path::{Path, PathBuf};

fn model_dir() -> PathBuf {
    std::env::var_os("OPENNC_MODEL_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("models/opennc-v1"))
}

#[test]
fn matches_python_reference() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let input = wav::read_mono_48k(&data.join("p232_001.wav")).unwrap();
    let reference = wav::read_mono_48k(&data.join("p232_001.ref.wav")).unwrap();
    let mut engine = Engine::new(&model_dir()).expect("model dir (set OPENNC_MODEL_DIR)");
    let out = engine.process_signal(&input).unwrap();
    assert_eq!(out.len(), reference.len());
    let diffs: Vec<f32> = out.iter().zip(&reference).map(|(a, b)| (a - b).abs()).collect();
    let max = diffs.iter().cloned().fold(0f32, f32::max);
    let mean = diffs.iter().sum::<f32>() / diffs.len() as f32;
    let peak = reference.iter().map(|v| v.abs()).fold(0f32, f32::max);
    eprintln!("max |diff| {max:.2e}, mean |diff| {mean:.2e}, reference peak {peak:.3}");
    // 16-bit quantisation of the reference alone is 1.5e-5; onnxruntime kernels differ slightly between platforms and the
    // recurrent state amplifies that at a few frames, so allow a small max and a tight mean.
    assert!(mean < 1e-4, "mean |diff| {mean:.2e} too large");
    assert!(max < 2e-2, "max |diff| {max:.2e} too large");
}
