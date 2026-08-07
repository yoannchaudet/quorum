//! End-to-end integration test for the `quorum` binary.
//!
//! Exercises the engine: `run` on a fresh work item creates the state DB and
//! advances autonomously until it blocks at the first human-review gate;
//! `status` reads the persisted state back.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_quorum"))
}

#[test]
fn run_advances_to_plan_review_and_status_reads_it_back() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let wi = home.join("mywi.md");
    std::fs::write(&wi, "# My WI\n").unwrap();

    // `run` advances the WI until it blocks on the first review gate (PlanReview),
    // and surfaces the HI resume command.
    let out = Command::new(bin())
        .arg("run")
        .arg(&wi)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(out.status.success(), "run failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PlanReview"), "unexpected output: {stdout}");
    assert!(
        stdout.contains("copilot --resume quorum/mywi/PlanReview"),
        "missing resume command: {stdout}"
    );

    // The state DB must now exist.
    let db = home.join(".quorum/state/mywi/quorum.db");
    assert!(db.exists(), "state db was not created at {}", db.display());

    // `status` reads the same persisted state back.
    let out = Command::new(bin())
        .args(["--config", "/dev/null", "status"])
        .arg(&db)
        .output()
        .unwrap();
    assert!(out.status.success(), "status failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PlanReview"), "unexpected output: {stdout}");
}
