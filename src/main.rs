//! OpenNC desktop: pick a microphone and the virtual cable, press the power button, select "CABLE Output" as the mic in
//! your apps. UI in ui/app.slint; audio in the library. Start and stop run off the UI thread so the window never freezes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use opennc::audio::{self, Runner, Shared};
use opennc::Engine;
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Off,
    Starting,
    On,
    Stopping,
}

struct State {
    cfg: Config,
    phase: Phase,
    runner: Option<Runner>,
    /// Engine loaded ahead of time so pressing the button opens the devices and nothing else.
    prepared: Option<Engine>,
    shown_mic: f32,
    shown_clean: f32,
}

type Shared_ = Arc<Mutex<State>>;

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

fn live_message(cfg: &Config) -> String {
    if is_cable(&cfg.output) {
        "Live. Your apps should use \"CABLE Output\" as their microphone.".into()
    } else {
        "Live.".into()
    }
}

/// Load the next engine in the background so the next start is instant.
fn prepare_engine(state: &Shared_) {
    let dir = model_dir(&state.lock().unwrap().cfg);
    let state = state.clone();
    std::thread::spawn(move || {
        if let Ok(e) = Engine::new(&dir) {
            let mut st = state.lock().unwrap();
            if st.prepared.is_none() {
                st.prepared = Some(e);
            }
        }
    });
}

fn start(ui: Weak<AppWindow>, state: Shared_) {
    let (engine, mic, output, strength, dir) = {
        let mut st = state.lock().unwrap();
        if st.phase != Phase::Off {
            return;
        }
        st.phase = Phase::Starting;
        save_config(&st.cfg);
        (st.prepared.take(), st.cfg.mic.clone(), st.cfg.output.clone(), st.cfg.strength, model_dir(&st.cfg))
    };
    if let Some(ui) = ui.upgrade() {
        ui.set_busy(true);
        ui.set_error(false);
        ui.set_message("Starting".into());
    }
    std::thread::spawn(move || {
        let engine = match engine {
            Some(e) => Ok(e),
            None => Engine::new(&dir).map_err(|e| anyhow::anyhow!("loading model from {}: {e:#}", dir.display())),
        };
        let result = engine.and_then(|e| Runner::start_with(e, mic, output, strength));
        let state2 = state.clone();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let mut st = state2.lock().unwrap();
            ui.set_busy(false);
            match result {
                Ok(r) => {
                    st.runner = Some(r);
                    st.phase = Phase::On;
                    ui.set_active(true);
                    ui.set_message(live_message(&st.cfg).into());
                }
                Err(e) => {
                    st.phase = Phase::Off;
                    eprintln!("start failed: {e:#}");
                    ui.set_active(false);
                    ui.set_error(true);
                    ui.set_message(format!("{e:#}").into());
                }
            }
        });
    });
}

fn stop(ui: Weak<AppWindow>, state: Shared_) {
    let runner = {
        let mut st = state.lock().unwrap();
        if st.phase != Phase::On {
            return;
        }
        st.phase = Phase::Stopping;
        st.shown_mic = -90.0;
        st.shown_clean = -90.0;
        st.runner.take()
    };
    if let Some(ui) = ui.upgrade() {
        ui.set_busy(true);
        ui.set_active(false);
        ui.set_message("Stopping".into());
        ui.set_mic_level(0.0);
        ui.set_clean_level(0.0);
        ui.set_mic_db("".into());
        ui.set_clean_db("".into());
    }
    std::thread::spawn(move || {
        drop(runner); // joins the audio thread and closes the devices
        let state2 = state.clone();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let mut st = state2.lock().unwrap();
            st.phase = Phase::Off;
            ui.set_busy(false);
            ui.set_error(false);
            ui.set_message(idle_message(&st.cfg).into());
            drop(st);
            prepare_engine(&state2);
        });
    });
}

/// Windows draws a light title bar by default; ask DWM for the dark one so the frame matches the app.
#[cfg(windows)]
fn dark_title_bar(ui: &AppWindow) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let holder = ui.window().window_handle();
    let Ok(handle) = holder.window_handle() else { return };
    if let RawWindowHandle::Win32(w) = handle.as_raw() {
        let on: i32 = 1;
        const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
        unsafe {
            windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(w.hwnd.get() as _, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const i32 as *const _, 4);
        }
    }
}
#[cfg(not(windows))]
fn dark_title_bar(_ui: &AppWindow) {}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let mut cfg = load_config();
    populate_devices(&ui, &mut cfg);
    ui.set_strength(cfg.strength);
    ui.set_message(idle_message(&cfg).into());
    let state: Shared_ = Arc::new(Mutex::new(State { cfg, phase: Phase::Off, runner: None, prepared: None, shown_mic: -90.0, shown_clean: -90.0 }));
    prepare_engine(&state);

    ui.on_toggle({
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            let phase = state.lock().unwrap().phase;
            match phase {
                Phase::Off => start(ui.clone(), state.clone()),
                Phase::On => stop(ui.clone(), state.clone()),
                _ => {}
            }
        }
    });
    ui.on_mic_changed({
        let state = state.clone();
        move |v| {
            let mut st = state.lock().unwrap();
            st.cfg.mic = v.to_string();
            save_config(&st.cfg);
        }
    });
    ui.on_output_changed({
        let ui = ui.as_weak();
        let state = state.clone();
        move |v| {
            let mut st = state.lock().unwrap();
            st.cfg.output = v.to_string();
            save_config(&st.cfg);
            if st.phase == Phase::Off {
                ui.unwrap().set_message(idle_message(&st.cfg).into());
            }
        }
    });
    ui.on_strength_changed({
        let state = state.clone();
        move |v| {
            let mut st = state.lock().unwrap();
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
            let mut st = state.lock().unwrap();
            populate_devices(&ui.unwrap(), &mut st.cfg);
        }
    });

    // meters with fast attack / slow release, and liveness, 25 times a second
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(40), {
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            let ui = ui.unwrap();
            let mut st = state.lock().unwrap();
            if st.phase != Phase::On {
                return;
            }
            let Some(r) = &st.runner else { return };
            if !r.shared.running.load(Ordering::SeqCst) {
                let why = r.shared.status.lock().unwrap().clone();
                st.runner = None;
                st.phase = Phase::Off;
                ui.set_active(false);
                ui.set_error(true);
                ui.set_message(if why.is_empty() { "The audio device stopped.".into() } else { why.into() });
                ui.set_mic_level(0.0);
                ui.set_clean_level(0.0);
                return;
            }
            let mic = Shared::get_f32(&r.shared.in_level).max(-90.0);
            let clean = Shared::get_f32(&r.shared.out_level).max(-90.0);
            let drops = r.shared.in_drops.load(Ordering::Relaxed) + r.shared.out_underruns.load(Ordering::Relaxed);
            st.shown_mic = mic.max(st.shown_mic - 2.5);
            st.shown_clean = clean.max(st.shown_clean - 2.5);
            let frac = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            let text = |db: f32| if db <= -89.0 { "silent".to_string() } else { format!("{db:.0} dB") };
            ui.set_mic_level(frac(st.shown_mic));
            ui.set_clean_level(frac(st.shown_clean));
            ui.set_mic_db(text(st.shown_mic).into());
            ui.set_clean_db(text(st.shown_clean).into());
            if drops > 2 && !ui.get_error() {
                ui.set_message(format!("Live. {drops} audio buffers dropped so far.").into());
            }
        }
    });

    ui.show()?;
    dark_title_bar(&ui);
    slint::run_event_loop()?;
    ui.hide()
}
