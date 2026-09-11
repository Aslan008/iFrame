//! All In Frame Common Library: Core Math, High-Frequency Input, Config, and IPC.

pub mod config;
pub mod input;
pub mod ipc;
pub mod math;

pub use config::WarpConfig;
pub use input::{delta_to_angles, qpc_frequency, qpc_now, InputAccumulator};
pub use ipc::{AifSharedHeader, AIF_MAGIC, AIF_VERSION};
pub use math::{
    deg_to_rad, linearize_depth, project_view_point, unproject_uv, Mat3, Vec2, Vec3,
};
