//! iframe.exe — control app: injector, game watcher, UI, ETW monitor.
//! M0: placeholder binary proving the workspace builds end-to-end.

use iframe_common::pacer::{PacerConfig, PacerMode, JitPacer};

fn main() {
    println!("iFrame v{} — zero-added-latency frame pacer", env!("CARGO_PKG_VERSION"));
    println!("M0: workspace OK. Pacer smoke: 60 FPS @ 120 Hz = {:.3} ms interval",
        1000.0 / 60.0);
    // Touch the core so dead-code elimination can't hide build errors.
    let pacer = JitPacer::new(PacerConfig { mode: PacerMode::FixedVsync, ..Default::default() }, 10_000_000);
    println!("JitPacer initialised, frames seen: {}", pacer.frames_seen());
}