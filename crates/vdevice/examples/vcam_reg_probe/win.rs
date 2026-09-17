//! The Windows half of `vcam_reg_probe`; see that file for what it answers.

#![allow(unsafe_code)]

use windows::core::{GUID, PCWSTR};
use windows::Win32::Media::MediaFoundation::{
    MFCreateVirtualCamera, MFStartup, MFVirtualCameraAccess_CurrentUser,
    MFVirtualCameraLifetime_Session, MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{
    CoGetClassObject, CoInitializeEx, IClassFactory, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
    COINIT_MULTITHREADED,
};

const MF_VERSION: u32 = 0x0002_0070;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn service_state(name: &str) -> String {
    let out = std::process::Command::new("sc.exe").args(["query", name]).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .find(|l| l.contains("STATE"))
            .map(|l| l.trim().to_owned())
            .unwrap_or_else(|| "unknown".into()),
        Err(e) => format!("sc.exe failed: {e}"),
    }
}

pub fn main() {
    let clsid_str =
        std::env::args().nth(1).unwrap_or_else(|| relay_vdevice::reg::VCAM_CLSID.to_owned());
    // windows-core parses the bare hyphenated form; the registry (and
    // MFCreateVirtualCamera) want the braced one.
    let guid: GUID =
        GUID::try_from(clsid_str.trim_matches(['{', '}'])).expect("argument must be a braced GUID");

    println!("CLSID          {clsid_str}");
    println!("FrameServer    before: {}", service_state("FrameServer"));

    // SAFETY: standard COM/MF init on this thread; all pointers are valid
    // for the duration of each call.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().expect("CoInitializeEx");

        // In-process view: HKCR = HKCU\Software\Classes over
        // HKLM\SOFTWARE\Classes, so a per-user registration resolves here.
        let inproc: windows::core::Result<IClassFactory> =
            CoGetClassObject(&guid, CLSCTX_INPROC_SERVER, None);
        println!(
            "CoGetClassObject(INPROC)   {}",
            match &inproc {
                Ok(_) => "OK — this process can activate the class".to_owned(),
                Err(e) => format!("{:#010x} {}", e.code().0, e.message()),
            }
        );
        // Out-of-process view: closer to how another security context would
        // have to find it.
        let local: windows::core::Result<IClassFactory> =
            CoGetClassObject(&guid, CLSCTX_LOCAL_SERVER, None);
        println!(
            "CoGetClassObject(LOCAL)    {}",
            match &local {
                Ok(_) => "OK".to_owned(),
                Err(e) => format!("{:#010x} {}", e.code().0, e.message()),
            }
        );

        MFStartup(MF_VERSION, 0).expect("MFStartup");
        let name = wide("Relay Probe Camera");
        let clsid_w = wide(&clsid_str);
        let cam = MFCreateVirtualCamera(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_Session,
            MFVirtualCameraAccess_CurrentUser,
            PCWSTR(name.as_ptr()),
            PCWSTR(clsid_w.as_ptr()),
            None,
        );
        match &cam {
            Ok(cam) => {
                println!("MFCreateVirtualCamera      OK");
                let started = cam.Start(None);
                println!("IMFVirtualCamera::Start    {started:?}");
                let _ = cam.Stop();
                let _ = cam.Remove();
                let _ = cam.Shutdown();
            }
            Err(e) => println!("MFCreateVirtualCamera      {:#010x} {}", e.code().0, e.message()),
        }
    }

    println!("FrameServer    after:  {}", service_state("FrameServer"));
}
