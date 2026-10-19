//! Command-line checks of the engine and the audio loop.
//!   opennc-cli <model_dir> <in.wav> <out.wav>            offline (48 kHz wav in, 16-bit 48 kHz wav out)
//!   opennc-cli devices                                   list input and output device names
//!   opennc-cli live <model_dir> <mic> <output> <secs>    run the real-time loop for a while, report levels and drops
use anyhow::{bail, Result};
use opennc::audio::{self, Runner, Shared};
use opennc::{wav, Engine, HOP, SR};
use std::path::Path;
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("devices") => {
            let (ins, outs) = audio::list_devices();
            println!("inputs:");
            ins.iter().for_each(|n| println!("  {n}"));
            println!("outputs:");
            outs.iter().for_each(|n| println!("  {n}"));
            Ok(())
        }
        Some("live") if args.len() >= 6 => {
            let secs: f64 = args[5].parse()?;
            let runner = Runner::start(Path::new(&args[2]).to_path_buf(), args[3].clone(), args[4].clone(), 100.0)?;
            println!("{}", runner.shared.status.lock().unwrap());
            use std::sync::atomic::Ordering::Relaxed;
            let t0 = Instant::now();
            let (mut peak_in, mut peak_out) = (-100f32, -100f32);
            let mut after_start = None; // counters one second in, so start-up glitches are reported separately
            while t0.elapsed().as_secs_f64() < secs {
                std::thread::sleep(Duration::from_millis(50));
                peak_in = peak_in.max(Shared::get_f32(&runner.shared.in_level));
                peak_out = peak_out.max(Shared::get_f32(&runner.shared.out_level));
                if after_start.is_none() && t0.elapsed() > Duration::from_secs(1) {
                    after_start = Some((runner.shared.in_drops.load(Relaxed), runner.shared.out_underruns.load(Relaxed), runner.shared.hops_done.load(Relaxed)));
                }
                if !runner.shared.running.load(std::sync::atomic::Ordering::SeqCst) {
                    bail!("audio thread stopped: {}", runner.shared.status.lock().unwrap());
                }
            }
            let (d0, u0, h0) = after_start.unwrap_or((0, 0, 0));
            let (d1, u1, h1) = (runner.shared.in_drops.load(Relaxed), runner.shared.out_underruns.load(Relaxed), runner.shared.hops_done.load(Relaxed));
            println!("{secs:.0} s: peak mic {peak_in:.1} dBFS, peak clean {peak_out:.1} dBFS");
            println!("first second: mic hops dropped {d0}, output underruns {u0}, hops processed {h0}");
            println!("after that:   mic hops dropped {}, output underruns {}, hops processed {} (expected ~{})", d1 - d0, u1 - u0, h1 - h0, ((secs - 1.0) * 100.0) as u32);
            Ok(())
        }
        Some(model_dir) if args.len() >= 4 => {
            let audio = wav::read_mono_48k(Path::new(&args[2]))?;
            let mut engine = Engine::new(Path::new(model_dir))?;
            let t0 = Instant::now();
            let out = engine.process_signal(&audio)?;
            let dt = t0.elapsed().as_secs_f64();
            wav::write_mono_48k_i16(Path::new(&args[3]), &out)?;
            let secs = out.len() as f64 / SR as f64;
            println!("wrote {} ({secs:.2} s)  {:.3} ms per 10 ms frame, real-time factor {:.3}", args[3], dt / (out.len() as f64 / HOP as f64) * 1000.0, dt / secs);
            Ok(())
        }
        _ => bail!("usage: opennc-cli <model_dir> <in.wav> <out.wav> | devices | live <model_dir> <mic> <output> <secs>"),
    }
}
