//! OpenNC desktop library: the streaming engine (and, later, the audio loop shared by the GUI and the CLI).
pub mod audio;
pub mod engine;
pub mod npy;
pub mod wav;

pub use engine::{Engine, HOP, SR};
