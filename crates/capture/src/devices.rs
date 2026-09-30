//! Which audio endpoint each device-backed track uses, and when to reopen it
//! (S40).
//!
//! Every track starts on the OS default ("System default"). The user can pin
//! a track to one endpoint from the mixer, live, and put it back on the
//! default the same way. A track on the default follows it: when Windows
//! changes the default console endpoint mid-share, the track reopens on the
//! new one without the share restarting. Nothing here changes any OS
//! setting; the default is only ever read.
//!
//! The capture and render threads poll [`should_reopen`] once per buffer —
//! two relaxed atomic loads, so the hot path pays nothing measurable.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A track's device choice: `None` = System default, `Some(id)` = that
/// endpoint. Bumps `generation` on every change so a thread that opened an
/// endpoint can tell cheaply that it should reopen.
#[derive(Debug, Default)]
pub struct DeviceSlot {
    choice: Mutex<Option<String>>,
    generation: AtomicU64,
}

impl DeviceSlot {
    pub fn shared(initial: Option<String>) -> Arc<Self> {
        Arc::new(Self { choice: Mutex::new(normalise(initial)), generation: AtomicU64::new(0) })
    }

    /// Set the choice. Returns false (and bumps nothing) when it is the same
    /// as what is already chosen, so a repeated command does not reopen.
    pub fn set(&self, device: Option<String>) -> bool {
        let device = normalise(device);
        let mut c = self.choice.lock().unwrap();
        if *c == device {
            return false;
        }
        *c = device;
        self.generation.fetch_add(1, Ordering::AcqRel);
        true
    }

    /// The current choice and the generation it belongs to.
    pub fn get(&self) -> (Option<String>, u64) {
        let c = self.choice.lock().unwrap();
        (c.clone(), self.generation.load(Ordering::Acquire))
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// An empty id means the default, the same as no id.
fn normalise(device: Option<String>) -> Option<String> {
    device.filter(|d| !d.trim().is_empty())
}

/// The device slots a share engine has: the sender's microphone, and the
/// output that received audio (or, on the sender, the call return) plays on.
#[derive(Debug)]
pub struct DeviceSlots {
    pub mic: Arc<DeviceSlot>,
    pub output: Arc<DeviceSlot>,
}

impl DeviceSlots {
    pub fn new(mic: Option<String>, output: Option<String>) -> Self {
        Self { mic: DeviceSlot::shared(mic), output: DeviceSlot::shared(output) }
    }

    /// Apply a `device` command. Returns whether anything changed.
    pub fn apply(&self, track: crate::command::DeviceTrack, device: Option<String>) -> bool {
        match track {
            crate::command::DeviceTrack::Mic => self.mic.set(device),
            crate::command::DeviceTrack::Output => self.output.set(device),
        }
    }
}

/// Direction of an endpoint, for the default-change counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Render,
    Capture,
}

static RENDER_DEFAULT_GEN: AtomicU64 = AtomicU64::new(0);
static CAPTURE_DEFAULT_GEN: AtomicU64 = AtomicU64::new(0);

fn counter(flow: Flow) -> &'static AtomicU64 {
    match flow {
        Flow::Render => &RENDER_DEFAULT_GEN,
        Flow::Capture => &CAPTURE_DEFAULT_GEN,
    }
}

/// How many times the OS default for `flow` has changed since the watcher
/// started.
pub fn default_generation(flow: Flow) -> u64 {
    counter(flow).load(Ordering::Acquire)
}

/// Record that the OS default for `flow` changed. Called by the watcher.
pub fn note_default_changed(flow: Flow) {
    counter(flow).fetch_add(1, Ordering::AcqRel);
}

/// What an open endpoint was opened against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opened {
    /// The slot generation at open time.
    pub slot_gen: u64,
    /// Whether it was opened as "System default".
    pub follows_default: bool,
    /// The default-change generation at open time.
    pub default_gen: u64,
}

/// Reopen when the user picked something else, or when the track follows
/// the default and the default moved. A pinned device ignores default
/// changes: the user chose that endpoint on purpose.
pub fn should_reopen(opened: &Opened, slot_gen: u64, default_gen: u64) -> bool {
    slot_gen != opened.slot_gen || (opened.follows_default && default_gen != opened.default_gen)
}

/// Start watching for OS default-endpoint changes, once per process. Cheap:
/// one parked thread holding an `IMMNotificationClient` registration.
pub fn watch_defaults() {
    #[cfg(windows)]
    {
        static STARTED: std::sync::Once = std::sync::Once::new();
        STARTED.call_once(win::start);
    }
}

#[cfg(windows)]
mod win {
    use tracing::{info, warn};
    use windows::core::{implement, PCWSTR};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
        IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    use super::{note_default_changed, Flow};

    #[implement(IMMNotificationClient)]
    struct DefaultWatcher;

    impl IMMNotificationClient_Impl for DefaultWatcher_Impl {
        fn OnDeviceStateChanged(
            &self,
            _id: &PCWSTR,
            _state: DEVICE_STATE,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }
        fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
            Ok(())
        }
        fn OnDefaultDeviceChanged(
            &self,
            flow: EDataFlow,
            role: ERole,
            id: &PCWSTR,
        ) -> windows::core::Result<()> {
            // Console is the role every track opens with; the multimedia and
            // communications notifications for the same change are ignored so
            // one change is one reopen.
            if role != eConsole {
                return Ok(());
            }
            let which = if flow == eRender {
                Flow::Render
            } else if flow == eCapture {
                Flow::Capture
            } else {
                return Ok(());
            };
            // SAFETY: the OS hands us a valid (or null) wide string for the
            // duration of the call.
            let id = if id.is_null() {
                String::new()
            } else {
                unsafe { id.to_string().unwrap_or_default() }
            };
            info!(flow = ?which, endpoint = %id, "OS default audio endpoint changed");
            note_default_changed(which);
            Ok(())
        }
        fn OnPropertyValueChanged(
            &self,
            _id: &PCWSTR,
            _key: &PROPERTYKEY,
        ) -> windows::core::Result<()> {
            Ok(())
        }
    }

    pub(super) fn start() {
        let spawned = std::thread::Builder::new().name("relay-audio-defaults".into()).spawn(|| {
            // SAFETY: COM for this thread; the registration and enumerator
            // live as long as the thread, which lives as long as the process.
            unsafe {
                if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                    warn!("default-endpoint watcher: COM init failed");
                    return;
                }
                let en: IMMDeviceEnumerator =
                    match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                        Ok(e) => e,
                        Err(e) => {
                            warn!(error = %e, "default-endpoint watcher: no enumerator");
                            return;
                        }
                    };
                let client: IMMNotificationClient = DefaultWatcher.into();
                if let Err(e) = en.RegisterEndpointNotificationCallback(&client) {
                    warn!(error = %e, "default-endpoint watcher: registration failed");
                    return;
                }
                loop {
                    std::thread::park();
                }
            }
        });
        if let Err(e) = spawned {
            warn!(error = %e, "could not start the default-endpoint watcher");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::DeviceTrack;

    #[test]
    fn a_slot_bumps_only_on_a_real_change() {
        let s = DeviceSlot::shared(None);
        assert_eq!(s.get(), (None, 0));
        assert!(!s.set(None), "default to default is no change");
        assert!(!s.set(Some("  ".into())), "an empty id is the default");
        assert!(s.set(Some("{a}".into())));
        assert_eq!(s.get(), (Some("{a}".into()), 1));
        assert!(!s.set(Some("{a}".into())), "the same device twice does not reopen");
        assert!(s.set(None));
        assert_eq!(s.generation(), 2);
    }

    #[test]
    fn slots_route_by_track() {
        let slots = DeviceSlots::new(Some("{mic}".into()), None);
        assert_eq!(slots.mic.get().0.as_deref(), Some("{mic}"));
        assert!(slots.apply(DeviceTrack::Output, Some("{spk}".into())));
        assert_eq!(slots.output.get().0.as_deref(), Some("{spk}"));
        assert_eq!(slots.mic.generation(), 0, "the other track is untouched");
    }

    #[test]
    fn reopen_rules() {
        let on_default = Opened { slot_gen: 3, follows_default: true, default_gen: 7 };
        assert!(!should_reopen(&on_default, 3, 7));
        assert!(should_reopen(&on_default, 3, 8), "the default moved");
        assert!(should_reopen(&on_default, 4, 7), "the user picked a device");

        let pinned = Opened { slot_gen: 3, follows_default: false, default_gen: 7 };
        assert!(!should_reopen(&pinned, 3, 8), "a pinned device ignores the default");
        assert!(should_reopen(&pinned, 4, 8), "back to default");
    }

    #[test]
    fn default_counters_are_per_flow() {
        let (r, c) = (default_generation(Flow::Render), default_generation(Flow::Capture));
        note_default_changed(Flow::Capture);
        assert_eq!(default_generation(Flow::Render), r);
        assert!(default_generation(Flow::Capture) > c);
    }
}
