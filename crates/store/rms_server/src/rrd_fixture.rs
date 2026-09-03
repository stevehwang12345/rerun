use std::sync::LazyLock;

use sha2::{Digest as _, Sha256};

use crate::domain::{ReplayLoop, TimelineDescriptor};

pub(crate) const BYTES: &[u8] =
    include_bytes!("../../../../tests/assets/rrd/rms/rms_replay_50s_v0_36_1.rrd");
pub(crate) const DEFAULT_TIMELINE: &str = "tick";
pub(crate) const DURATION_SECONDS: f64 = 50.0;
pub(crate) const DURATION_LABEL: &str = "00:50";
pub(crate) const RRD_VERSION: &str = "0.36.1";

static CONTENT_SHA256: LazyLock<String> = LazyLock::new(|| format!("{:x}", Sha256::digest(BYTES)));
static ETAG: LazyLock<String> = LazyLock::new(|| format!("\"sha256:{}\"", content_sha256()));

pub(crate) fn timelines() -> Vec<TimelineDescriptor> {
    vec![TimelineDescriptor {
        name: DEFAULT_TIMELINE.to_owned(),
        kind: "sequence".to_owned(),
        start: "0".to_owned(),
        end: "100".to_owned(),
        duration_seconds: Some(DURATION_SECONDS),
        fps: Some(2.0),
    }]
}

pub(crate) fn initial_loop() -> ReplayLoop {
    ReplayLoop {
        mode: "off".to_owned(),
        start: None,
        end: None,
    }
}

pub(crate) fn content_sha256() -> &'static str {
    CONTENT_SHA256.as_str()
}

pub(crate) fn etag() -> &'static str {
    ETAG.as_str()
}
