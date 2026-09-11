//! Audio endpoint change notifications → [`CoreEvent::HardwareChanged`].
//!
//! `IMMNotificationClient` is the WASAPI-sanctioned way to hear about default
//! device switches and endpoint arrival/removal; callbacks arrive on MMDevice
//! worker threads and are forwarded straight into the service's event channel
//! (the channel is the thread boundary — nothing else is shared). Display and
//! USB topology changes are watched separately by the winloop window via
//! `WM_DISPLAYCHANGE` / `WM_DEVICECHANGE`.

use anyhow::{Context, Result};
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;
use windows::core::implement;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

use super::probe_win::ComGuard;
use crate::winloop::CoreEvent;

#[implement(IMMNotificationClient)]
struct Notifier {
    tx: UnboundedSender<CoreEvent>,
}

impl Notifier {
    fn changed(&self, what: &str) {
        debug!(what, "audio endpoint change");
        let _ = self.tx.send(CoreEvent::HardwareChanged);
    }
}

impl IMMNotificationClient_Impl for Notifier_Impl {
    fn OnDeviceStateChanged(
        &self,
        _id: &windows::core::PCWSTR,
        _state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        self.changed("state");
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &windows::core::PCWSTR) -> windows::core::Result<()> {
        self.changed("added");
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &windows::core::PCWSTR) -> windows::core::Result<()> {
        self.changed("removed");
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _id: &windows::core::PCWSTR,
    ) -> windows::core::Result<()> {
        // Only the render/console default drives headset selection; the same
        // physical switch also fires for eMultimedia/eCommunications.
        if flow == eRender && role == eConsole {
            self.changed("default");
        }
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _id: &windows::core::PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        // Property churn is constant (volume, meters); identity is unaffected.
        Ok(())
    }
}

/// Keeps the registration alive. Dropping unregisters.
pub struct AudioWatcher {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
    _com: ComGuard,
}

impl AudioWatcher {
    pub fn start(tx: UnboundedSender<CoreEvent>) -> Result<Self> {
        let com = ComGuard::init();
        // SAFETY: standard enumerator creation + callback registration; the
        // client and enumerator live as long as this struct.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("creating MMDeviceEnumerator")?;
            let client: IMMNotificationClient = Notifier { tx }.into();
            enumerator
                .RegisterEndpointNotificationCallback(&client)
                .context("registering endpoint notifications")?;
            Ok(Self { enumerator, client, _com: com })
        }
    }
}

impl Drop for AudioWatcher {
    fn drop(&mut self) {
        // SAFETY: unregistering exactly what was registered in `start`.
        unsafe {
            let _ = self.enumerator.UnregisterEndpointNotificationCallback(&self.client);
        }
    }
}
