//! Regenerates the RMS Replay scenario RRD with the current workspace SDK.

use std::path::PathBuf;

use sha2::{Digest as _, Sha256};

const FRAME_RATE: i64 = 2;
const LAST_TICK: i64 = 100;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/assets/rrd/rms/rms_replay_50s_v0_36_1.rrd"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let rec = rerun::RecordingStreamBuilder::new("rms_replay_fixture")
        .recording_id("rms-replay-fixture-v2")
        .save(&path)?;
    rec.set_log_tick_enabled(false);
    rec.set_log_time_enabled(false);

    for tick in 0..=LAST_TICK {
        let elapsed_seconds = tick as f64 / FRAME_RATE as f64;
        let angle = elapsed_seconds * 0.28;
        let position = [
            (angle.cos() * 5.0) as f32,
            (angle.sin() * 5.0) as f32,
            (1.0 + (angle * 0.5).sin() * 0.25) as f32,
        ];

        rec.set_time_sequence("tick", tick);
        rec.log(
            "world/robot/position",
            &rerun::Points3D::new([position])
                .with_colors([rerun::Color::from_rgb(46, 204, 113)])
                .with_radii([0.22]),
        )?;
        rec.log(
            "telemetry/speed",
            &rerun::Scalars::single(1.1 + (angle * 0.75).sin() * 0.2),
        )?;
    }

    rec.finalize_deferred_sinks();
    let bytes = std::fs::read(&path)?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    println!(
        "Generated ticks 0..={LAST_TICK} at {FRAME_RATE} Hz: {} ({} bytes, sha256:{sha256})",
        path.display(),
        bytes.len(),
    );
    Ok(())
}
