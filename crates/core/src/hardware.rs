//! What is plugged in right now. Profiles are selected against this.

use serde::{Deserialize, Serialize};

use crate::types::{HeadsetId, MonitorId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ConnectedHardware {
    /// The default render endpoint's known headset, if it is in the library.
    pub headset: Option<HeadsetId>,
    /// All attached monitors, primary first.
    pub monitors: Vec<MonitorId>,
}

impl ConnectedHardware {
    pub fn has_monitor(&self, id: &MonitorId) -> bool {
        self.monitors.iter().any(|m| m == id)
    }
}

/// Source of truth for connected hardware. The real implementation will read
/// the default WASAPI render endpoint and enumerate monitors via EDID; the
/// no-op version reports nothing connected so only "Any" profiles match.
pub trait HardwareProbe: Send + Sync {
    fn probe(&self) -> ConnectedHardware;
}

#[derive(Debug, Default)]
pub struct NoopHardwareProbe;

impl HardwareProbe for NoopHardwareProbe {
    fn probe(&self) -> ConnectedHardware {
        ConnectedHardware::default()
    }
}
