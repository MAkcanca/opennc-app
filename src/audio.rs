//! Audio loop: microphone -> engine (10 ms hops) -> output device (the virtual cable). One thread owns both cpal
//! streams and the engine; the GUI talks to it through `Shared` atomics.
//! ponytail: devices are opened at 48 kHz and the OS converts (WASAPI/CoreAudio/PipeWire all do); add a resampler if a
//! device ever refuses.
use crate::{Engine, HOP, SR};
use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Names of input and output devices on the default host.
pub fn list_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let ins = host.input_devices().map(|it| it.filter_map(|d| dev_name(&d)).collect()).unwrap_or_default();
    let outs = host.output_devices().map(|it| it.filter_map(|d| dev_name(&d)).collect()).unwrap_or_default();
    (ins, outs)
}

fn dev_name(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|x| x.name().to_string())
}

/// State shared between the audio thread and the GUI.
pub struct Shared {
    pub running: AtomicBool,
    pub in_level: AtomicU32,  // dBFS, f32 bits
    pub out_level: AtomicU32, // dBFS, f32 bits
    pub strength: AtomicU32,  // 0..=100, f32 bits
    /// Microphone hops dropped because the engine thread was behind (bounded queue full).
    pub in_drops: AtomicU32,
    /// Output callbacks that found the queue short (played silence for the missing part).
    pub out_underruns: AtomicU32,
    pub hops_done: AtomicU32,
    pub status: Mutex<String>,
}

impl Shared {
    pub fn set_f32(a: &AtomicU32, v: f32) {
        a.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn get_f32(a: &AtomicU32) -> f32 {
        f32::from_bits(a.load(Ordering::Relaxed))
    }
}

pub struct Runner {
    pub shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Runner {
    /// Loads the model, opens both devices and starts processing. Returns once audio is flowing or with the error.
    pub fn start(model_dir: PathBuf, mic: String, output: String, strength: f32) -> Result<Runner> {
        let engine = Engine::new(&model_dir).with_context(|| format!("loading model from {}", model_dir.display()))?;
        Self::start_with(engine, mic, output, strength)
    }

    /// Same with an engine that was loaded ahead of time (the window pre-loads one so starting is instant).
    pub fn start_with(engine: Engine, mic: String, output: String, strength: f32) -> Result<Runner> {
        let shared = Arc::new(Shared {
            running: AtomicBool::new(true),
            in_level: AtomicU32::new((-100f32).to_bits()),
            out_level: AtomicU32::new((-100f32).to_bits()),
            strength: AtomicU32::new(strength.to_bits()),
            in_drops: AtomicU32::new(0),
            out_underruns: AtomicU32::new(0),
            hops_done: AtomicU32::new(0),
            status: Mutex::new(String::new()),
        });
        let (tx, rx) = mpsc::channel::<Result<()>>();
        let s2 = shared.clone();
        let handle = std::thread::Builder::new().name("opennc-audio".into()).spawn(move || {
            if let Err(e) = run_audio(engine, &mic, &output, s2.clone(), &tx) {
                *s2.status.lock().unwrap() = format!("error: {e:#}");
                let _ = tx.send(Err(e));
            }
            s2.running.store(false, Ordering::SeqCst);
        })?;
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Runner { shared, handle: Some(handle) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow!("audio thread did not start in time")),
        }
    }

    pub fn stop(&mut self) {
        self.shared.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop();
    }
}

fn find_device(host: &cpal::Host, name: &str, input: bool) -> Result<cpal::Device> {
    let devs = if input { host.input_devices()? } else { host.output_devices()? };
    for d in devs {
        if dev_name(&d).as_deref() == Some(name) {
            return Ok(d);
        }
    }
    bail!("{} device not found: {name}", if input { "input" } else { "output" })
}

fn dbfs(x: &[f32]) -> f32 {
    let e = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    20.0 * (e.sqrt() + 1e-9).log10()
}

fn run_audio(mut engine: Engine, mic: &str, output: &str, shared: Arc<Shared>, started: &mpsc::Sender<Result<()>>) -> Result<()> {
    let host = cpal::default_host();
    let in_dev = find_device(&host, mic, true)?;
    let out_dev = find_device(&host, output, false)?;
    let in_ch = in_dev.default_input_config().context("mic config")?.channels();
    let out_ch = out_dev.default_output_config().context("output config")?.channels();
    let cfg = |ch: u16| cpal::StreamConfig { channels: ch, sample_rate: SR, buffer_size: cpal::BufferSize::Fixed(HOP as u32) };

    // mic -> engine: whole 10 ms hops over a bounded channel; the callback never blocks, a full queue drops the hop.
    let (ftx, frx) = mpsc::sync_channel::<Vec<f32>>(8);
    let mut acc: Vec<f32> = Vec::with_capacity(HOP);
    let sh_in = shared.clone();
    let in_stream = in_dev
        .build_input_stream(
            cfg(in_ch),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                for frame in data.chunks(in_ch as usize) {
                    acc.push(frame[0]);
                    if acc.len() == HOP {
                        let hop = std::mem::replace(&mut acc, Vec::with_capacity(HOP));
                        if ftx.try_send(hop).is_err() {
                            sh_in.in_drops.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            },
            |e| eprintln!("input stream error: {e}"),
            None,
        )
        .context("opening the microphone at 48 kHz")?;

    // engine -> output: a queue pre-filled with three hops of silence to absorb scheduling jitter (adds 30 ms latency;
    // two hops still underran twice while the streams spun up).
    let ring = Arc::new(Mutex::new(VecDeque::<f32>::from(vec![0f32; 3 * HOP])));
    let r2 = ring.clone();
    let sh_out = shared.clone();
    let out_stream = out_dev
        .build_output_stream(
            cfg(out_ch),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut q = r2.lock().unwrap();
                let mut short = false;
                for frame in data.chunks_mut(out_ch as usize) {
                    let v = q.pop_front().unwrap_or_else(|| {
                        short = true;
                        0.0
                    });
                    frame.fill(v);
                }
                if short {
                    sh_out.out_underruns.fetch_add(1, Ordering::Relaxed);
                }
            },
            |e| eprintln!("output stream error: {e}"),
            None,
        )
        .context("opening the output device at 48 kHz")?;
    in_stream.play()?;
    out_stream.play()?;
    *shared.status.lock().unwrap() = format!("running: {mic} -> {output}");
    let _ = started.send(Ok(()));

    let mut out = vec![0f32; HOP];
    while shared.running.load(Ordering::SeqCst) {
        let hop = match frx.recv_timeout(Duration::from_millis(200)) {
            Ok(h) => h,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        engine.strength = Shared::get_f32(&shared.strength);
        engine.process_frame(&hop, &mut out)?;
        Shared::set_f32(&shared.in_level, dbfs(&hop));
        Shared::set_f32(&shared.out_level, dbfs(&out));
        shared.hops_done.fetch_add(1, Ordering::Relaxed);
        let mut q = ring.lock().unwrap();
        if q.len() > 8 * HOP {
            // the output device fell behind: keep latency bounded instead of growing
            let drop = q.len() - 2 * HOP;
            q.drain(..drop);
        }
        q.extend(out.iter().copied());
    }
    Ok(())
}
