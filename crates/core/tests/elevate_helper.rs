//! The elevated helper's refusals, exercised against the real binary.
//!
//! These run *unelevated*, which is the point: the helper's first job is to
//! refuse safely, and every refusal here has to happen without touching HKLM.
//! The one thing no test may do is take the happy path — registering the APO
//! or the camera needs an administrator token and is the user's decision, so
//! the live pass is a runbook (`docs/dev/elevation-live.md`), not a test.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

use relay_core::elevate::{OpOutcome, Request, Response, REQUEST_VERSION};

const HELPER: &str = env!("CARGO_BIN_EXE_relay-elevate");

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("relay-elev-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data").join("elevate")).unwrap();
    dir
}

fn elevate_dir(root: &Path) -> PathBuf {
    root.join("data").join("elevate")
}

/// Write a request as raw JSON so a test can craft one no core would send.
fn write_raw(root: &Path, nonce: &str, json: &str) -> PathBuf {
    let path = elevate_dir(root).join(format!("{nonce}.request.json"));
    std::fs::write(&path, json).unwrap();
    path
}

fn run_helper(request: &Path) -> std::process::Output {
    Command::new(HELPER).arg("--request").arg(request).output().expect("running relay-elevate")
}

fn result_of(root: &Path, nonce: &str) -> Option<Response> {
    let path = elevate_dir(root).join(format!("{nonce}.result.json"));
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn request_json(root: &Path, nonce: &str, ops: &str, created_at: u64, version: u32) -> String {
    let dir = serde_json::to_string(root).unwrap();
    format!(
        r#"{{"version":{version},"nonce":"{nonce}","created_at":{created_at},
             "data_dir":{dir},"ops":{ops}}}"#
    )
}

fn now() -> u64 {
    relay_core::elevate::now_secs()
}

/// The whole point: run without an administrator token and every op is
/// refused, with a result file that says so in one sentence.
#[test]
fn an_unelevated_helper_refuses_everything_and_changes_nothing() {
    if relay_core::processes::is_elevated() {
        eprintln!("skipped: this test process is elevated");
        return;
    }
    let root = temp_root("unelev");
    let nonce = "unelev-1";
    let req = write_raw(
        &root,
        nonce,
        &request_json(
            &root,
            nonce,
            r#"[{"install_apo":{}},"install_camera"]"#,
            now(),
            REQUEST_VERSION,
        ),
    );

    let out = run_helper(&req);
    assert!(!out.status.success(), "a refusal must not report success");

    let response = result_of(&root, nonce).expect("the helper always writes a result");
    assert!(!response.elevated);
    assert!(!response.ok());
    assert_eq!(response.results.len(), 2);
    for r in &response.results {
        match &r.outcome {
            OpOutcome::Refused { reason } => assert!(reason.contains("elevated"), "{reason}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    // Nothing was recorded, so nothing would later be "uninstalled".
    assert!(!root.join("installed.json").exists());
    assert!(!root.join("apo-backup").exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// A request naming something outside the closed op set never parses, so the
/// helper cannot even be asked to do it.
#[test]
fn an_op_outside_the_allow_list_is_not_a_request_at_all() {
    let root = temp_root("badop");
    let nonce = "badop-1";
    let req = write_raw(
        &root,
        nonce,
        &request_json(&root, nonce, r#"["delete_system32"]"#, now(), REQUEST_VERSION),
    );

    let out = run_helper(&req);
    assert!(!out.status.success());
    assert!(result_of(&root, nonce).is_none(), "an unparseable request produces no result");
    let _ = std::fs::remove_dir_all(&root);
}

/// The request has to be the file the core wrote, in the elevate directory of
/// the data root it names. A copy dropped elsewhere is refused.
#[test]
fn a_request_outside_the_elevate_directory_is_refused() {
    if relay_core::processes::is_elevated() {
        eprintln!("skipped: this test process is elevated");
        return;
    }
    let root = temp_root("stray");
    let nonce = "stray-1";
    let json = request_json(&root, nonce, r#"["install_camera"]"#, now(), REQUEST_VERSION);
    let stray = root.join("somewhere-else.json");
    std::fs::write(&stray, &json).unwrap();

    let out = run_helper(&stray);
    assert!(!out.status.success());
    let response = result_of(&root, nonce).expect("result written next to the data root");
    let reason = match &response.results[0].outcome {
        OpOutcome::Refused { reason } => reason.clone(),
        other => panic!("expected a refusal, got {other:?}"),
    };
    // Location is checked before elevation, so this is the reason given.
    assert!(reason.contains("request is not at"), "{reason}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A request file left behind by a crash cannot be replayed later.
#[test]
fn a_stale_request_is_refused() {
    let root = temp_root("stale");
    let nonce = "stale-1";
    let old = now() - relay_core::elevate::MAX_REQUEST_AGE_SECS - 60;
    let req = write_raw(
        &root,
        nonce,
        &request_json(&root, nonce, r#"["uninstall_camera"]"#, old, REQUEST_VERSION),
    );

    let out = run_helper(&req);
    assert!(!out.status.success());
    let response = result_of(&root, nonce).expect("result");
    let reason = match &response.results[0].outcome {
        OpOutcome::Refused { reason } => reason.clone(),
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(reason.contains("stale"), "{reason}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The helper run with no arguments does nothing and says so.
#[test]
fn the_helper_is_not_a_user_command() {
    let out = Command::new(HELPER).output().expect("running relay-elevate");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing was done"));
}

/// The camera's recorded keys are the uninstall's entire authority, so a
/// tampered `installed.json` must not turn one UAC prompt into a
/// delete-anything primitive. Vetting is pure, so this is checked directly
/// rather than by running the helper against a doctored file.
#[test]
fn recorded_keys_outside_our_clsid_would_be_refused() {
    use relay_core::elevate::vet_com_keys;
    let ours = relay_vdevice::reg::VCAM_CLSID;
    let recorded = vec![
        format!(r"SOFTWARE\Classes\CLSID\{ours}\InprocServer32"),
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run".to_string(),
    ];
    assert!(vet_com_keys(&recorded, ours).is_err());
    assert!(vet_com_keys(&recorded[..1], ours).is_ok());
}

/// Requests and results live under the data root the request names, which is
/// what keeps a `--data-dir` run out of the real install's folder.
#[test]
fn a_data_dir_run_stays_in_its_own_root() {
    let root = temp_root("scoped");
    let nonce = "scoped-1";
    let req = write_raw(
        &root,
        nonce,
        &request_json(&root, nonce, r#"[{"uninstall_apo":{}}]"#, now(), REQUEST_VERSION),
    );
    let _ = run_helper(&req);
    assert!(elevate_dir(&root).join(format!("{nonce}.result.json")).exists());

    // And the request round-trips through the same struct the core writes.
    let parsed: Request = serde_json::from_slice(&std::fs::read(&req).unwrap()).unwrap();
    assert_eq!(parsed.data_dir, root);
    let _ = std::fs::remove_dir_all(&root);
}
