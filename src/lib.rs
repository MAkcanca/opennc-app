//! OpenNC desktop library: the streaming engine (and, later, the audio loop shared by the GUI and the CLI).
pub mod audio;
pub mod autostart;
pub mod engine;
pub mod instance;
pub mod log;
pub mod npy;
pub mod tray;
pub mod update;
pub mod wav;

pub use engine::{Engine, HOP, SR};
