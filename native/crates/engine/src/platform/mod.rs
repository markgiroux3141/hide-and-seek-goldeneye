//! Platform layer — the OS-facing primitives a game's event loop builds on.
//!
//! [`input`] is the held-keys/mouse-delta state the app feeds and the camera/
//! authoring loop read; [`frame`] is the fixed-timestep + frame-pacing clock a
//! render loop drives from. The winit `ApplicationHandler` itself lives in the
//! `game` crate (it maps input to game actions), consuming these.

pub mod frame;
pub mod input;
