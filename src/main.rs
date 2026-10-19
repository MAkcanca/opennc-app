//! OpenNC desktop: pick a microphone and the virtual cable, press the power button, select "CABLE Output" as the mic in
//! your apps. UI in ui/app.slint; audio in the library.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use opennc::audio::{self, Runner, Shared};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Duration;

slint::include_modules!();

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Config {
    mic: String,
    output: String,
    strength: f32,
    model_dir: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { mic: String::new(), output: String::new(), strength: 100.0, model_dir: None }
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("OpenNC").join("config.json")
}

fn load_config() -> Config {
    std::fs::read(config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_config(cfg: &Config) {
    let p = config_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, serde_json::to_vec_pretty(cfg).unwrap_or_default());
}

/// config, then OPENNC_MODEL_DIR, then models/opennc-v1 next to the executable, then the working directory.
fn model_dir(cfg: &Config) -> PathBuf {
    if let Some(d) = &cfg.model_dir {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("OPENNC_MODEL_DIR") {
        return PathBuf::from(d);
    }
    let beside_exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("models").join("opennc-v1")));
    match beside_exe {
        Some(p) if p.join("dfn_stream.onnx").exists() => p,
        _ => PathBuf::from("models/opennc-v1"),
    }
}

struct State {
    cfg: Config,
    runner: Option<Runner>,
}

fn strings(v: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(v.iter().map(SharedString::from).collect::<Vec<_>>()))
}

fn is_cable(name: &str) -> bool {
    name.to_lowercase().contains("cable input")
}

fn populate_devices(ui: &AppWindow, cfg: &mut Config) {
    let (inputs, outputs) = audio::list_devices();
    if cfg.mic.is_empty() || !inputs.contains(&cfg.mic) {
        cfg.mic = inputs.first().cloned().unwrap_or_default();
    }
    if cfg.output.is_empty() || !outputs.contains(&cfg.output) {
        cfg.output = outputs.iter().find(|n| is_cable(n)).or(outputs.first()).cloned().unwrap_or_default();
    }
    ui.set_inputs(strings(&inputs));
    ui.set_outputs(strings(&outputs));
    ui.set_mic(cfg.mic.as_str().into());
    ui.set_output(cfg.output.as_str().into());
}

fn idle_message(cfg: &Config) -> String {
    if is_cable(&cfg.output) {
        "Press to start. Then pick \"CABLE Output\" as the microphone in your apps.".into()
    } else {
        "Press to start. Clean audio goes to the selected output.".into()
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let mut cfg = load_config();
    populate_devices(&ui, &mut cfg);
    ui.set_strength(cfg.strength);
    ui.set_message(idle_message(&cfg).into());
    let state = Rc::new(RefCell::new(State { cfg, runner: None }));

    ui.on_toggle({
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            let ui = ui.unwrap();
            let mut st = state.borrow_mut();
            if st.runner.is_some() {
                st.runner = None; // Drop stops the audio thread
                ui.set_active(false);
                ui.set_error(false);
                ui.set_message(idle_message(&st.cfg).into());
                ui.set_mic_level(0.0);
                ui.set_clean_level(0.0);
                ui.set_mic_db("".into());
                ui.set_clean_db("".into());
                return;
            }
            save_config(&st.cfg);
            match Runner::start(model_dir(&st.cfg), st.cfg.mic.clone(), st.cfg.output.clone(), st.cfg.strength) {
                Ok(r) => {
                    st.runner = Some(r);
                    ui.set_active(true);
                    ui.set_error(false);
                    ui.set_message(if is_cable(&st.cfg.output) { "Live. Your apps should use \"CABLE Output\" as their microphone.".into() } else { "Live.".into() });
                }
                Err(e) => {
                    ui.set_active(false);
                    ui.set_error(true);
                    ui.set_message(format!("{e:#}").into());
                }
            }
        }
    });
    ui.on_mic_changed({
        let state = state.clone();
        move |v| {
            let mut st = state.borrow_mut();
            st.cfg.mic = v.to_string();
            save_config(&st.cfg);
        }
    });
    ui.on_output_changed({
        let ui = ui.as_weak();
        let state = state.clone();
        move |v| {
            let mut st = state.borrow_mut();
            st.cfg.output = v.to_string();
            save_config(&st.cfg);
            if st.runner.is_none() {
                ui.unwrap().set_message(idle_message(&st.cfg).into());
            }
        }
    });
    ui.on_strength_changed({
        let state = state.clone();
        move |v| {
            let mut st = state.borrow_mut();
            st.cfg.strength = v;
            if let Some(r) = &st.runner {
                Shared::set_f32(&r.shared.strength, v);
            }
            save_config(&st.cfg);
        }
    });
    ui.on_refresh_devices({
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            let mut st = state.borrow_mut();
            populate_devices(&ui.unwrap(), &mut st.cfg);
        }
    });

    // meters and liveness, 20 times a second while running
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), {
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            let ui = ui.unwrap();
            let mut st = state.borrow_mut();
            let Some(r) = &st.runner else { return };
            if !r.shared.running.load(Ordering::SeqCst) {
                let why = r.shared.status.lock().unwrap().clone();
                st.runner = None;
                ui.set_active(false);
                ui.set_error(true);
                ui.set_message(if why.is_empty() { "The audio device stopped.".into() } else { why.into() });
                return;
            }
            let (mic, clean) = (Shared::get_f32(&r.shared.in_level).max(-90.0), Shared::get_f32(&r.shared.out_level).max(-90.0));
            let frac = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            let text = |db: f32| if db <= -90.0 { "silent".to_string() } else { format!("{db:.0} dB") };
            ui.set_mic_level(frac(mic));
            ui.set_clean_level(frac(clean));
            ui.set_mic_db(text(mic).into());
            ui.set_clean_db(text(clean).into());
            let drops = r.shared.in_drops.load(Ordering::Relaxed) + r.shared.out_underruns.load(Ordering::Relaxed);
            if drops > 2 && !ui.get_error() {
                ui.set_message(format!("Live. {drops} audio buffers dropped so far.").into());
            }
        }
    });

    ui.run()
}
