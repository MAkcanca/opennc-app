//! System tray icon with a small menu. Events are polled from the UI timer (`poll`), so no extra thread.
use anyhow::{Context, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub enum TrayAction {
    Toggle,
    Show,
    SetAutostart(bool),
    Quit,
}

pub struct Tray {
    _icon: TrayIcon,
    toggle: CheckMenuItem,
    autostart: CheckMenuItem,
    show: MenuItem,
    quit: MenuItem,
}

fn icon_rgba() -> Result<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!("../ui/icon64.png").as_slice()));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    anyhow::ensure!(info.color_type == png::ColorType::Rgba, "icon64.png must be RGBA");
    buf.truncate(info.buffer_size());
    Ok((buf, info.width, info.height))
}

impl Tray {
    pub fn new(active: bool, autostart_on: bool) -> Result<Self> {
        let (rgba, w, h) = icon_rgba()?;
        let menu = Menu::new();
        let toggle = CheckMenuItem::new("Noise cancellation", true, active, None);
        let show = MenuItem::new("Open OpenNC", true, None);
        let autostart = CheckMenuItem::new("Start with Windows", true, autostart_on, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append_items(&[&toggle, &PredefinedMenuItem::separator(), &show, &autostart, &PredefinedMenuItem::separator(), &quit])?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("OpenNC")
            .with_icon(Icon::from_rgba(rgba, w, h)?)
            .build()
            .context("creating the tray icon")?;
        Ok(Self { _icon: icon, toggle, autostart, show, quit })
    }

    pub fn set_active(&self, on: bool) {
        self.toggle.set_checked(on);
        let _ = self._icon.set_tooltip(Some(if on { "OpenNC: noise cancellation on" } else { "OpenNC: off" }));
    }

    pub fn set_autostart(&self, on: bool) {
        self.autostart.set_checked(on);
    }

    /// Drain pending tray and menu events.
    pub fn poll(&self) -> Vec<TrayAction> {
        let mut out = Vec::new();
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == *self.toggle.id() {
                out.push(TrayAction::Toggle);
            } else if ev.id == *self.show.id() {
                out.push(TrayAction::Show);
            } else if ev.id == *self.autostart.id() {
                out.push(TrayAction::SetAutostart(self.autostart.is_checked()));
            } else if ev.id == *self.quit.id() {
                out.push(TrayAction::Quit);
            }
        }
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                out.push(TrayAction::Show);
            }
        }
        out
    }
}
