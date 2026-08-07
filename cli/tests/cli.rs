//! End-to-end integration test for the `quorum` binary.
//!
//! Exercises the walking skeleton: `run` on a fresh work item creates the state
//! DB and reports the initial state; `status` reads it back.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_quorum"))
}

#[test]
fn run_then_status_reports_intake() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let wi = home.join("mywi.md");
    std::fs::write(&wi, "# My WI\n").unwrap();

    // `run` initializes the WI and reports it is progressing at Intake.
    let out = Command::new(bin())
        .arg("run")
        .arg(&wi)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(out.status.success(), "run failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Intake"), "unexpected output: {stdout}");

    // The state DB must now exist.
    let db = home.join(".quorum/state/mywi/quorum.db");
    assert!(db.exists(), "state db was not created at {}", db.display());

    // `status` reads the same DB back.
    let out = Command::new(bin())
        .args(["--config", "/dev/null", "status"])
        .arg(&db)
        .output()
        .unwrap();
    assert!(out.status.success(), "status failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Intake"), "unexpected output: {stdout}");
}
