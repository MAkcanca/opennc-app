//! OpenNC desktop: pick a microphone and the virtual cable, press the power button, select "CABLE Output" as the mic in
//! your apps. UI in ui/app.slint; audio in the library. Lives in the tray; start and stop run off the UI thread.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use opennc::audio::{self, Runner, Shared};
use opennc::log::log;
use opennc::tray::{Tray, TrayAction};
use opennc::update::{self, Update};
use opennc::{autostart, Engine};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

slint::include_modules!();

const CABLE_URL: &str = "https://vb-audio.com/Cable/";

// The tray icon lives on the UI thread only; background work reaches it through the event loop.
thread_local! { static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) }; }
fn with_tray(f: impl FnOnce(&Tray)) {
    TRAY.with(|t| {
        if let Some(t) = t.borrow().as_ref() {
            f(t)
        }
    });
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Config {
    mic: String,
    output: String,
    strength: f32,
    model_dir: Option<String>,
    /// Turn noise cancellation on as soon as the app starts.
    auto_on: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { mic: String::new(), output: String::new(), strength: 100.0, model_dir: None, auto_on: true }
    }
}

fn config_path() -> PathBuf {
    opennc::log::config_dir().join("config.json")
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Off,
    Starting,
    On,
    Stopping,
    /// A device went away (or was not there at launch); retries until it is back.
    Waiting,
}

struct State {
    cfg: Config,
    phase: Phase,
    runner: Option<Runner>,
    prepared: Option<Engine>,
    shown_mic: f32,
    shown_clean: f32,
    last_retry: Instant,
    update: Option<Update>,
    updating: bool,
}

type St = Arc<Mutex<State>>;

fn strings(v: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(v.iter().map(SharedString::from).collect::<Vec<_>>()))
}

fn is_cable(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("cable input") || n.contains("blackhole")
}

/// Refresh the device lists; returns whether both configured devices are present.
fn populate_devices(ui: &AppWindow, cfg: &mut Config) -> bool {
    let (inputs, outputs) = audio::list_devices();
    if cfg.mic.is_empty() {
        cfg.mic = inputs.first().cloned().unwrap_or_default();
    }
    if cfg.output.is_empty() {
        cfg.output = outputs.iter().find(|n| is_cable(n)).or(outputs.first()).cloned().unwrap_or_default();
    }
    ui.set_cable_missing(!outputs.iter().any(|n| is_cable(n)));
    ui.set_inputs(strings(&inputs));
    ui.set_outputs(strings(&outputs));
    ui.set_mic(cfg.mic.as_str().into());
    ui.set_output(cfg.output.as_str().into());
    inputs.contains(&cfg.mic) && outputs.contains(&cfg.output)
}

fn devices_present(cfg: &Config) -> bool {
    let (inputs, outputs) = audio::list_devices();
    inputs.contains(&cfg.mic) && outputs.contains(&cfg.output)
}

fn output_hint(cfg: &Config) -> String {
    if is_cable(&cfg.output) {
        "Pick \"CABLE Output\" as the microphone in your apps.".into()
    } else {
        "Clean audio goes to this device.".into()
    }
}

/// Load the next engine in the background so the next start is instant.
fn prepare_engine(state: &St) {
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

fn enter_waiting(ui: &AppWindow, st: &mut State, why: &str) {
    log(format!("waiting for devices: {why}"));
    st.phase = Phase::Waiting;
    st.runner = None;
    st.last_retry = Instant::now();
    ui.set_busy(false);
    ui.set_active(false);
    ui.set_waiting(true);
    ui.set_error(false);
    ui.set_notice("Your microphone or output device is not available. OpenNC resumes when it is back.".into());
    ui.set_mic_level(0.0);
    ui.set_clean_level(0.0);
}

fn start(ui: Weak<AppWindow>, state: St) {
    let (engine, mic, output, strength, dir) = {
        let mut st = state.lock().unwrap();
        if st.phase != Phase::Off && st.phase != Phase::Waiting {
            return;
        }
        st.phase = Phase::Starting;
        save_config(&st.cfg);
        (st.prepared.take(), st.cfg.mic.clone(), st.cfg.output.clone(), st.cfg.strength, model_dir(&st.cfg))
    };
    if let Some(ui) = ui.upgrade() {
        ui.set_busy(true);
        ui.set_error(false);
    }
    std::thread::spawn(move || {
        let engine = match engine {
            Some(e) => Ok(e),
            None => Engine::new(&dir).map_err(|e| anyhow::anyhow!("loading model from {}: {e:#}", dir.display())),
        };
        let result = engine.and_then(|e| Runner::start_with(e, mic.clone(), output.clone(), strength));
        let state2 = state.clone();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let mut st = state2.lock().unwrap();
            ui.set_busy(false);
            match result {
                Ok(r) => {
                    log(format!("started: {mic} -> {output}"));
                    st.runner = Some(r);
                    st.phase = Phase::On;
                    ui.set_active(true);
                    ui.set_waiting(false);
                    ui.set_notice("".into());
                    with_tray(|t| t.set_active(true));
                }
                Err(e) => {
                    let text = format!("{e:#}");
                    if text.contains("device not found") {
                        enter_waiting(&ui, &mut st, &text);
                    } else {
                        log(format!("start failed: {text}"));
                        st.phase = Phase::Off;
                        ui.set_active(false);
                        ui.set_error_message(text.into());
                        ui.set_error(true);
                    }
                }
            }
        });
    });
}

fn stop(ui: Weak<AppWindow>, state: St) {
    let runner = {
        let mut st = state.lock().unwrap();
        match st.phase {
            Phase::On => {}
            Phase::Waiting => {
                // cancel waiting
                st.phase = Phase::Off;
                if let Some(ui) = ui.upgrade() {
                    ui.set_waiting(false);
                    ui.set_notice("".into());
                }
                return;
            }
            _ => return,
        }
        st.phase = Phase::Stopping;
        st.shown_mic = -90.0;
        st.shown_clean = -90.0;
        st.runner.take()
    };
    if let Some(ui) = ui.upgrade() {
        ui.set_busy(true);
        ui.set_active(false);
        ui.set_mic_level(0.0);
        ui.set_clean_level(0.0);
        ui.set_mic_db("".into());
        ui.set_clean_db("".into());
    }
    with_tray(|t| t.set_active(false));
    std::thread::spawn(move || {
        drop(runner); // joins the audio thread and closes the devices
        log("stopped");
        let state2 = state.clone();
        let _ = ui.upgrade_in_event_loop(move |ui| {
            let mut st = state2.lock().unwrap();
            st.phase = Phase::Off;
            ui.set_busy(false);
            ui.set_error(false);
            drop(st);
            prepare_engine(&state2);
        });
    });
}

fn check_updates(ui: Weak<AppWindow>, state: St) {
    std::thread::spawn(move || match update::check() {
        Ok(Some(u)) => {
            log(format!("update available: {}", u.version));
            let v = u.version.clone();
            state.lock().unwrap().update = Some(u);
            let _ = ui.upgrade_in_event_loop(move |ui| ui.set_update_version(v.into()));
        }
        Ok(None) => {}
        Err(e) => log(format!("update check failed: {e:#}")),
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let minimized = std::env::args().any(|a| a == "--minimized");
    log(format!("OpenNC {} starting{}", update::CURRENT, if minimized { " (minimized)" } else { "" }));
    let Some(instance) = opennc::instance::acquire(!minimized) else {
        log("another OpenNC is already running; handing over to it");
        return Ok(());
    };
    let ui = AppWindow::new()?;
    let mut cfg = load_config();
    let present = populate_devices(&ui, &mut cfg);
    ui.set_strength(cfg.strength);
    ui.set_output_hint(output_hint(&cfg).into());
    ui.set_auto_on(cfg.auto_on);
    ui.set_start_with_windows(autostart::is_enabled());
    ui.set_version(format!("OpenNC {}  ·  48 kHz  ·  10 ms frames", update::CURRENT).into());
    let auto_on = cfg.auto_on;
    let state: St = Arc::new(Mutex::new(State {
        cfg,
        phase: Phase::Off,
        runner: None,
        prepared: None,
        shown_mic: -90.0,
        shown_clean: -90.0,
        last_retry: Instant::now(),
        update: None,
        updating: false,
    }));
    prepare_engine(&state);
    TRAY.with(|t| {
        *t.borrow_mut() = match Tray::new(false, autostart::is_enabled()) {
            Ok(t) => Some(t),
            Err(e) => {
                log(format!("tray unavailable: {e:#}"));
                None
            }
        }
    });

    ui.on_toggle({
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            let phase = state.lock().unwrap().phase;
            match phase {
                Phase::Off => start(ui.clone(), state.clone()),
                Phase::On | Phase::Waiting => stop(ui.clone(), state.clone()),
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
                let ui = ui.unwrap();
                ui.set_output_hint(output_hint(&st.cfg).into());
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
    ui.on_get_cable(|| {
        let _ = open::that(CABLE_URL);
    });
    ui.on_set_start_with_windows(|on| {
        if let Err(e) = autostart::set_enabled(on) {
            log(format!("start with Windows: {e}"));
        }
        with_tray(|t| t.set_autostart(on));
    });
    ui.on_set_auto_on({
        let state = state.clone();
        move |on| {
            let mut st = state.lock().unwrap();
            st.cfg.auto_on = on;
            save_config(&st.cfg);
        }
    });
    ui.on_install_update({
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            let u = {
                let mut st = state.lock().unwrap();
                if st.updating {
                    return;
                }
                st.updating = true;
                st.update.clone()
            };
            let Some(u) = u else { return };
            ui.unwrap().set_updating(true);
            let ui = ui.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let result = update::install(&u);
                let _ = ui.upgrade_in_event_loop(move |ui| match result {
                    Ok(p) => {
                        log(format!("installer launched: {}", p.display()));
                        state.lock().unwrap().runner = None;
                        let _ = slint::quit_event_loop();
                    }
                    Err(e) => {
                        log(format!("update failed: {e:#}"));
                        state.lock().unwrap().updating = false;
                        ui.set_updating(false);
                        ui.set_error_message(format!("Update failed: {e:#}").into());
                        ui.set_error(true);
                    }
                });
            });
        }
    });
    // closing the window keeps OpenNC running in the tray
    ui.window().on_close_requested({
        let ui = ui.as_weak();
        move || {
            let _ = ui.unwrap().hide();
            slint::CloseRequestResponse::HideWindow
        }
    });

    // meters, liveness, device recovery and tray events, 25 times a second
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(40), {
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            let ui = ui.unwrap();
            if instance.show_requested() {
                let _ = ui.show();
                bring_to_front(&ui);
            }
            let actions = TRAY.with(|t| t.borrow().as_ref().map(|t| t.poll()).unwrap_or_default());
            for a in actions {
                match a {
                    TrayAction::Toggle => ui.invoke_toggle(),
                    TrayAction::Show => {
                        let _ = ui.show();
                        bring_to_front(&ui);
                    }
                    TrayAction::SetAutostart(on) => {
                        ui.set_start_with_windows(on);
                        ui.invoke_set_start_with_windows(on);
                    }
                    TrayAction::Quit => {
                        state.lock().unwrap().runner = None;
                        let _ = slint::quit_event_loop();
                    }
                }
            }
            let mut st = state.lock().unwrap();
            match st.phase {
                Phase::Waiting => {
                    if st.last_retry.elapsed() > Duration::from_secs(2) {
                        st.last_retry = Instant::now();
                        if devices_present(&st.cfg) {
                            populate_devices(&ui, &mut st.cfg);
                            drop(st);
                            start(ui.as_weak(), state.clone());
                        }
                    }
                }
                Phase::On => {
                    let Some(r) = &st.runner else { return };
                    if !r.shared.running.load(Ordering::SeqCst) {
                        let why = r.shared.status.lock().unwrap().clone();
                        with_tray(|t| t.set_active(false));
                        enter_waiting(&ui, &mut st, &why);
                        return;
                    }
                    let mic = Shared::get_f32(&r.shared.in_level).max(-90.0);
                    let clean = Shared::get_f32(&r.shared.out_level).max(-90.0);
                    st.shown_mic = mic.max(st.shown_mic - 2.5);
                    st.shown_clean = clean.max(st.shown_clean - 2.5);
                    let frac = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                    let text = |db: f32| if db <= -89.0 { "silent".to_string() } else { format!("{db:.0} dB") };
                    ui.set_mic_level(frac(st.shown_mic));
                    ui.set_clean_level(frac(st.shown_clean));
                    ui.set_mic_db(text(st.shown_mic).into());
                    ui.set_clean_db(text(st.shown_clean).into());
                }
                _ => {}
            }
        }
    });

    // update check shortly after launch, then every 6 hours
    let update_timer = slint::Timer::default();
    update_timer.start(slint::TimerMode::Repeated, Duration::from_secs(6 * 3600), {
        let ui = ui.as_weak();
        let state = state.clone();
        move || check_updates(ui.clone(), state.clone())
    });
    let first_check = slint::Timer::default();
    first_check.start(slint::TimerMode::SingleShot, Duration::from_secs(3), {
        let ui = ui.as_weak();
        let state = state.clone();
        move || check_updates(ui.clone(), state.clone())
    });

    if !minimized {
        ui.show()?;
        dark_title_bar(&ui);
    }
    if auto_on {
        if present {
            start(ui.as_weak(), state.clone());
        } else {
            enter_waiting(&ui, &mut state.lock().unwrap(), "devices missing at launch");
        }
    }
    slint::run_event_loop_until_quit()?;
    state.lock().unwrap().runner = None;
    log("exit");
    Ok(())
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

/// Raise the window after a second launch or a tray click. Windows may refuse to take focus from the app the user
/// is in; then it flashes the taskbar button instead, which is the right signal anyway.
#[cfg(windows)]
fn bring_to_front(ui: &AppWindow) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let holder = ui.window().window_handle();
    let Ok(handle) = holder.window_handle() else { return };
    if let RawWindowHandle::Win32(w) = handle.as_raw() {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow(w.hwnd.get() as _);
        }
    }
}
#[cfg(not(windows))]
fn bring_to_front(_ui: &AppWindow) {}
