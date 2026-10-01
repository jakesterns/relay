//! Relay's own WebView2 never raises a device or permission prompt (S43b).
//!
//! A `getUserMedia` in the webview once put an unanswered "tauri.localhost
//! wants to use your cameras" dialog on screen. Relay's UI has no business
//! with the camera, microphone, location, notifications or any other
//! permission-gated web API: the engines do capture natively. So every
//! `PermissionRequested` is answered here, silently, with Deny, and WebView2
//! shows no prompt.
//!
//! If Relay ever needs one kind, add exactly that kind to [`ALLOWED`] with a
//! comment saying why; everything else stays denied.
//!
//! Tauri 2 has no API for this, so it goes through the raw WebView2 COM
//! interface that wry exposes via `with_webview`.
//!
//! Manual check (no automated way to raise a real prompt in jsdom): in a
//! debug build, open devtools and run
//! `navigator.mediaDevices.getUserMedia({video:true})` -- it must reject with
//! NotAllowedError and no dialog may appear.

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_STATE,
    COREWEBVIEW2_PERMISSION_STATE_ALLOW, COREWEBVIEW2_PERMISSION_STATE_DENY,
};

/// Permission kinds Relay's UI is allowed to use. Empty: none.
const ALLOWED: &[COREWEBVIEW2_PERMISSION_KIND] = &[];

/// The answer for one request kind: Deny unless allow-listed.
pub(crate) fn decide(kind: COREWEBVIEW2_PERMISSION_KIND) -> COREWEBVIEW2_PERMISSION_STATE {
    if ALLOWED.contains(&kind) {
        COREWEBVIEW2_PERMISSION_STATE_ALLOW
    } else {
        COREWEBVIEW2_PERMISSION_STATE_DENY
    }
}

/// Register the handler on one webview window. Best effort: a failure is
/// logged and the app carries on (WebView2 would then prompt, not grant).
pub(crate) fn deny_all(window: &tauri::WebviewWindow) {
    use webview2_com::PermissionRequestedEventHandler;
    let r = window.with_webview(|wv| {
        // SAFETY: WebView2 COM calls on the live controller, on the UI thread
        // that `with_webview` runs us on.
        unsafe {
            let Ok(core) = wv.controller().CoreWebView2() else {
                tracing::warn!("no CoreWebView2; permission handler not set");
                return;
            };
            let handler = PermissionRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    args.PermissionKind(&mut kind)?;
                    args.SetState(decide(kind))?;
                    // Proof for a live check: a stored "Block" answers
                    // without ever reaching this handler.
                    tracing::info!(kind = kind.0, "webview permission request denied");
                }
                Ok(())
            }));
            let mut token = 0i64;
            match core.add_PermissionRequested(&handler, &mut token) {
                Ok(()) => tracing::info!("webview permission requests are denied"),
                Err(e) => tracing::warn!("could not set the permission handler: {e}"),
            }
        }
    });
    if let Err(e) = r {
        tracing::warn!("with_webview failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND_CAMERA, COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ,
        COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
        COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS,
        COREWEBVIEW2_PERMISSION_KIND_UNKNOWN_PERMISSION,
    };

    #[test]
    fn media_location_notifications_and_the_rest_are_denied() {
        for kind in [
            COREWEBVIEW2_PERMISSION_KIND_CAMERA,
            COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
            COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION,
            COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS,
            COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ,
            COREWEBVIEW2_PERMISSION_KIND_UNKNOWN_PERMISSION,
        ] {
            assert_eq!(decide(kind), COREWEBVIEW2_PERMISSION_STATE_DENY, "{kind:?}");
        }
        assert!(ALLOWED.is_empty(), "an allowed kind needs a reason next to it");
    }
}
