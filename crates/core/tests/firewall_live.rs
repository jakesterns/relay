//! The firewall probe against this machine's real rule store.
//!
//! `cargo test -p relay-core --test firewall_live -- --ignored --nocapture`
//!
//! `firewall.rs`'s unit tests cover the parser and the verdict with fixtures
//! captured from a real machine; this covers the part fixtures cannot — that
//! the registry walk actually finds those rules on a live Windows, and that
//! the policy read agrees with what Windows reports.
//!
//! **Read-only.** Nothing here adds, removes or changes a rule, so it is safe
//! to run on a working machine. The write paths refuse without
//! `RELAY_FIREWALL_ALLOW_LIVE_WRITE=1` and an elevated token, and are
//! exercised by the VM cycle (`scripts/vm-cycle.ps1`) instead.

#![cfg(windows)]

use relay_core::firewall::{self, Action, Direction};

#[test]
#[ignore = "reads this machine's live firewall rules"]
fn the_registry_walk_finds_real_rules() {
    let rules = firewall::read_rules().expect("reading the firewall rule store");
    println!("parsed {} rules", rules.len());
    assert!(
        rules.len() > 10,
        "a real Windows install has hundreds of built-in rules; got {}",
        rules.len()
    );

    // Every parsed rule must be self-consistent: the parser returns None
    // rather than guessing, so anything in the list is fully determined.
    for r in &rules {
        assert!(!r.raw.is_empty(), "rule {} kept no verbatim string", r.key);
        assert!(r.profiles != 0, "rule {} parsed to no profile at all", r.key);
    }

    let inbound_allow =
        rules.iter().filter(|r| r.action == Action::Allow && r.direction == Direction::In).count();
    println!("  inbound allow: {inbound_allow}");
    assert!(inbound_allow > 0, "no inbound Allow rules at all — did the walk read the data?");
}

#[test]
#[ignore = "reads this machine's live firewall policy"]
fn the_policy_read_returns_a_live_profile() {
    let p = firewall::read_policy().expect("reading firewall policy");
    println!("{p:?}");
    assert!(
        p.active_profiles != 0,
        "no active network profile — is this machine connected to anything?"
    );
}

/// The end-to-end shape the UI banner depends on: probe, classify, report.
#[test]
#[ignore = "reads this machine's live firewall state"]
fn status_classifies_this_machines_share_binary() {
    let program = firewall::share_program().expect("locating relay-share.exe");
    let s = firewall::status(&program);
    println!("{s:#?}");
    assert!(!s.unknown, "the probe fell back to defaults: {}", s.verdict.summary());
    assert_eq!(s.program, program.to_string_lossy());

    // Whatever the verdict, it has to be internally consistent — a blocked
    // machine must not also claim it can share.
    if s.blocking_rules > 0 {
        assert_eq!(s.verdict, firewall::Verdict::Blocked);
        assert!(!s.verdict.can_share());
    }
    if s.verdict == firewall::Verdict::Allowed {
        assert!(s.rule_present, "verdict Allowed with no Allow rule recorded");
        assert!(s.verdict.can_share());
    }
}

/// The dry run is read-only and must say something about every change it
/// would make, because it is what the user reads before the UAC prompt.
#[test]
#[ignore = "reads this machine's live firewall state"]
fn the_dry_run_lists_the_allow_rule() {
    let program = firewall::share_program().expect("locating relay-share.exe");
    let before = firewall::read_rules().expect("reading rules").len();
    let lines = firewall::install_dry_run(&program);
    for l in &lines {
        println!("{l}");
    }
    assert!(lines.iter().any(|l| l.contains(firewall::SHARE_EXE)), "{lines:?}");
    assert_eq!(
        before,
        firewall::read_rules().expect("reading rules").len(),
        "dry run changed rules"
    );
}

/// The gate is the thing standing between a bug and a machine-wide policy
/// change, so prove it holds in an ordinary unelevated process.
#[test]
fn the_write_paths_refuse_without_the_gate() {
    let dir = std::env::temp_dir().join(format!("relay-fw-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let paths = relay_core::config::Paths::at(&dir);

    // Unset here regardless of how the session was started.
    // SAFETY: single-threaded test, no other thread reads the environment.
    unsafe { std::env::remove_var(firewall::LIVE_WRITE_GATE) };

    let program = firewall::share_program().expect("locating relay-share.exe");
    let err = firewall::install_live(&paths, &program).expect_err("install must refuse");
    assert!(format!("{err:#}").contains(firewall::LIVE_WRITE_GATE), "{err:#}");
    let err = firewall::uninstall_live(&paths).expect_err("uninstall must refuse");
    assert!(format!("{err:#}").contains(firewall::LIVE_WRITE_GATE), "{err:#}");

    // A refusal must not have written the record either.
    assert!(!paths.firewall_file().exists(), "a refused install still wrote firewall.json");
    let _ = std::fs::remove_dir_all(&dir);
}
