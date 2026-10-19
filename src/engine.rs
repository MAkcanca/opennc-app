//! Streaming OpenNC engine: the released per-frame ONNX graph with explicit recurrent state.
//! Port of the Python reference (`inference/engine.py` in the OpenNC model repo); same numerics (f64 spectrum path).
//! 48 kHz, 10 ms hop, 20 ms window, no lookahead.
use crate::npy;
use anyhow::{Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::path::Path;
use std::sync::Arc;

pub const SR: u32 = 48_000;
pub const FFT: usize = 960;
pub const HOP: usize = 480;
pub const BINS: usize = 481;
pub const N_DF: usize = 160;
pub const DF_ORDER: usize = 5;
pub const N_ERB: usize = 32;

#[derive(serde::Deserialize)]
struct Meta {
    erb_widths: Vec<usize>,
    alpha: f64,
}

pub struct Engine {
    session: Session,
    window: Vec<f64>,
    band_idx: Vec<usize>,
    alpha: f64,
    /// 0..=100; below 100 the band gains are blended toward 1 (less suppression).
    pub strength: f32,
    /// Output silence gate: frames under `gate_close_db` become exact zeros, ramp up to `gate_open_db`.
    pub gate_close_db: f32,
    pub gate_open_db: f32,
    // signal state
    frame: Vec<f64>,
    erb_state: [f64; N_ERB],
    unit_state: [f64; N_DF],
    hist: [[Complex<f64>; N_DF]; DF_ORDER],
    oa: Vec<f64>,
    // model state, flattened row-major
    erb_hist: Vec<f32>,
    spec_hist: Vec<f32>,
    c0_hist: Vec<f32>,
    h_enc: Vec<f32>,
    h_erb: Vec<f32>,
    h_df: Vec<f32>,
    // fft
    fwd: Arc<dyn RealToComplex<f64>>,
    inv: Arc<dyn ComplexToReal<f64>>,
    x_time: Vec<f64>,
    spec: Vec<Complex<f64>>,
    y_time: Vec<f64>,
    scratch_f: Vec<Complex<f64>>,
    scratch_i: Vec<Complex<f64>>,
}

impl Engine {
    /// `model_dir` holds dfn_stream.onnx, window960.npy, unit_norm_init160.npy, stream_meta.json (the release zip).
    pub fn new(model_dir: &Path) -> Result<Self> {
        let meta: Meta = serde_json::from_slice(&std::fs::read(model_dir.join("stream_meta.json")).context("stream_meta.json")?)?;
        let mut band_idx = vec![0usize];
        for w in &meta.erb_widths {
            band_idx.push(band_idx.last().unwrap() + w);
        }
        anyhow::ensure!(band_idx.len() == N_ERB + 1 && *band_idx.last().unwrap() == BINS, "unexpected ERB layout");
        let window: Vec<f64> = npy::load_f32_1d(&model_dir.join("window960.npy"))?.iter().map(|&v| v as f64).collect();
        let unit: Vec<f32> = npy::load_f32_1d(&model_dir.join("unit_norm_init160.npy"))?;
        anyhow::ensure!(window.len() == FFT && unit.len() == N_DF, "unexpected support file sizes");
        // ort's builder errors carry the builder back (not Send), so they are flattened to text for anyhow.
        fn oe<R>(e: ort::Error<R>) -> anyhow::Error {
            anyhow::anyhow!("onnxruntime: {e}")
        }
        // onnxruntime allows one environment per process: create it once, reuse it for every engine.
        static ENV: std::sync::OnceLock<std::sync::Arc<ort::environment::Environment>> = std::sync::OnceLock::new();
        let env = match ENV.get() {
            Some(e) => e.clone(),
            None => {
                let e = ort::init().build().map_err(oe)?;
                ENV.get_or_init(|| std::sync::Arc::new(e)).clone()
            }
        };
        let session = Session::builder(&env)
            .map_err(oe)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(oe)?
            .with_intra_threads(1)
            .map_err(oe)?
            .with_inter_threads(1)
            .map_err(oe)?
            .commit_from_file(model_dir.join("dfn_stream.onnx"))
            .map_err(oe)
            .context("loading dfn_stream.onnx")?;
        let mut planner = RealFftPlanner::<f64>::new();
        let fwd = planner.plan_fft_forward(FFT);
        let inv = planner.plan_fft_inverse(FFT);
        let mut erb_state = [0f64; N_ERB];
        for (i, v) in erb_state.iter_mut().enumerate() {
            *v = -60.0 - 30.0 * i as f64 / (N_ERB - 1) as f64; // numpy linspace(-60, -90, 32)
        }
        let mut unit_state = [0f64; N_DF];
        for (i, v) in unit_state.iter_mut().enumerate() {
            *v = unit[i] as f64;
        }
        Ok(Self {
            scratch_f: vec![Complex::default(); fwd.get_scratch_len()],
            scratch_i: vec![Complex::default(); inv.get_scratch_len()],
            session,
            window,
            band_idx,
            alpha: meta.alpha,
            strength: 100.0,
            gate_close_db: -85.0,
            gate_open_db: -80.0,
            frame: vec![0.0; FFT],
            erb_state,
            unit_state,
            hist: [[Complex::default(); N_DF]; DF_ORDER],
            oa: vec![0.0; FFT],
            erb_hist: vec![0.0; 2 * N_ERB],
            spec_hist: vec![0.0; 2 * 2 * N_DF],
            c0_hist: vec![0.0; 64 * 4 * N_DF],
            h_enc: vec![0.0; 512],
            h_erb: vec![0.0; 2 * 512],
            h_df: vec![0.0; 2 * 512],
            fwd,
            inv,
            x_time: vec![0.0; FFT],
            spec: vec![Complex::default(); BINS],
            y_time: vec![0.0; FFT],
        })
    }

    /// One 10 ms hop in (480 samples, float in [-1, 1]), one hop out. Output lags the input by one hop.
    pub fn process_frame(&mut self, input: &[f32], output: &mut [f32]) -> Result<()> {
        anyhow::ensure!(input.len() == HOP && output.len() == HOP, "expected {HOP} samples");
        // analysis
        self.frame.copy_within(HOP.., 0);
        for (d, s) in self.frame[HOP..].iter_mut().zip(input) {
            *d = *s as f64;
        }
        for i in 0..FFT {
            self.x_time[i] = self.frame[i] * self.window[i];
        }
        self.fwd.process_with_scratch(&mut self.x_time, &mut self.spec, &mut self.scratch_f)?;
        let inv_n = 1.0 / FFT as f64;
        for c in self.spec.iter_mut() {
            *c *= inv_n; // libdf analysis scaling, as in the reference
        }
        // features
        let a = self.alpha;
        let mut feat_erb = vec![0f32; N_ERB];
        for b in 0..N_ERB {
            let (lo, hi) = (self.band_idx[b], self.band_idx[b + 1]);
            let mean = self.spec[lo..hi].iter().map(|c| c.norm_sqr()).sum::<f64>() / (hi - lo) as f64;
            let e = 10.0 * (mean + 1e-10).log10();
            self.erb_state[b] = e * (1.0 - a) + self.erb_state[b] * a;
            feat_erb[b] = ((e - self.erb_state[b]) / 40.0) as f32;
        }
        let mut feat_spec = vec![0f32; 2 * N_DF];
        for k in 0..N_DF {
            self.unit_state[k] = self.spec[k].norm() * (1.0 - a) + self.unit_state[k] * a;
            let s = self.spec[k] / self.unit_state[k].sqrt();
            feat_spec[k] = s.re as f32;
            feat_spec[N_DF + k] = s.im as f32;
        }
        // model
        let (gain, coefs) = {
            let outputs = self.session.run(ort::inputs![
                "feat_erb" => Tensor::from_array(([1usize, 1, 1, N_ERB], feat_erb))?,
                "feat_spec" => Tensor::from_array(([1usize, 2, 1, N_DF], feat_spec))?,
                "erb_hist" => Tensor::from_array(([1usize, 1, 2, N_ERB], std::mem::take(&mut self.erb_hist)))?,
                "spec_hist" => Tensor::from_array(([1usize, 2, 2, N_DF], std::mem::take(&mut self.spec_hist)))?,
                "c0_hist" => Tensor::from_array(([1usize, 64, 4, N_DF], std::mem::take(&mut self.c0_hist)))?,
                "h_enc" => Tensor::from_array(([1usize, 1, 512], std::mem::take(&mut self.h_enc)))?,
                "h_erb" => Tensor::from_array(([2usize, 1, 512], std::mem::take(&mut self.h_erb)))?,
                "h_df" => Tensor::from_array(([2usize, 1, 512], std::mem::take(&mut self.h_df)))?,
            ])?;
            let take = |name: &str| -> Result<Vec<f32>> {
                let (_, data) = outputs[name].try_extract_tensor::<f32>()?;
                Ok(data.to_vec())
            };
            self.erb_hist = take("erb_hist_out")?;
            self.spec_hist = take("spec_hist_out")?;
            self.c0_hist = take("c0_hist_out")?;
            self.h_enc = take("h_enc_out")?;
            self.h_erb = take("h_erb_out")?;
            self.h_df = take("h_df_out")?;
            (take("gain")?, take("coefs")?)
        };
        // mask + deep filter on the masked spectrum
        let s = (self.strength / 100.0).clamp(0.0, 1.0) as f64;
        for k in 0..BINS {
            let g = gain[k] as f64 * s + (1.0 - s);
            self.spec[k] *= g;
        }
        self.hist.copy_within(1.., 0);
        self.hist[DF_ORDER - 1].copy_from_slice(&self.spec[..N_DF]);
        for k in 0..N_DF {
            let mut acc = Complex::default();
            for t in 0..DF_ORDER {
                let c = Complex::new(coefs[(t * N_DF + k) * 2] as f64, coefs[(t * N_DF + k) * 2 + 1] as f64);
                acc += c * self.hist[t][k];
            }
            self.spec[k] = acc;
        }
        // synthesis: numpy irfft(X * N) == unnormalised inverse of X; DC/Nyquist imaginary parts are ignored
        self.spec[0].im = 0.0;
        self.spec[BINS - 1].im = 0.0;
        self.inv.process_with_scratch(&mut self.spec, &mut self.y_time, &mut self.scratch_i)?;
        self.oa.copy_within(HOP.., 0);
        for v in self.oa[HOP..].iter_mut() {
            *v = 0.0;
        }
        for i in 0..FFT {
            self.oa[i] += self.y_time[i] * self.window[i];
        }
        let mut energy = 0f64;
        for i in 0..HOP {
            output[i] = self.oa[i] as f32;
            energy += self.oa[i] * self.oa[i];
        }
        let rms_db = 20.0 * ((energy / HOP as f64).sqrt() + 1e-12).log10();
        if rms_db < self.gate_open_db as f64 {
            let g = ((rms_db - self.gate_close_db as f64) / (self.gate_open_db - self.gate_close_db) as f64).max(0.0) as f32;
            for v in output.iter_mut() {
                *v *= g;
            }
        }
        Ok(())
    }

    /// Whole-signal helper, output aligned with the input (the one-hop delay removed), like the reference `process_file`.
    pub fn process_signal(&mut self, audio: &[f32]) -> Result<Vec<f32>> {
        let n_hops = (audio.len() + HOP - 1) / HOP + 1;
        let mut work = vec![0f32; n_hops * HOP];
        work[..audio.len()].copy_from_slice(audio);
        let mut out = vec![0f32; n_hops * HOP];
        for i in 0..n_hops {
            let (a, b) = (i * HOP, (i + 1) * HOP);
            let inp = work[a..b].to_vec();
            self.process_frame(&inp, &mut out[a..b])?;
        }
        Ok(out[HOP..HOP + audio.len()].to_vec())
    }
}
