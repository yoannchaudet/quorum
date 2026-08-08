//! End-to-end integration test for the `quorum` binary.
//!
//! Exercises the engine: `run` on a fresh work item persists global state and
//! advances autonomously until it blocks at the first human-review gate;
//! `status` reads the work item state back by id.

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

    // `run --dry-run` advances the WI until it blocks on the first review gate
    // (PlanReview), using stub agents so no copilot/network is needed.
    let out = Command::new(bin())
        .arg("run")
        .arg("--dry-run")
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

    assert!(home.join(".quorum/quorum.db").exists());
    let state_entries = std::fs::read_dir(home.join(".quorum/state"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(state_entries.len(), 1);
    assert_ne!(
        state_entries[0].file_name().to_string_lossy(),
        "mywi",
        "filesystem state must use the stable internal id"
    );

    // `status` reads the same persisted state back.
    let out = Command::new(bin())
        .args(["status", "mywi"])
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(out.status.success(), "status failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PlanReview"), "unexpected output: {stdout}");
}

#[test]
fn approve_gates_drive_work_item_to_done() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let wi = home.join("done-wi.md");
    std::fs::write(&wi, "# WI\ndo it\n").unwrap();

    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .arg("--dry-run")
            .env("HOME", home)
            .output()
            .unwrap()
    };

    // Start: blocks at PlanReview.
    let out = run(&["run", wi.to_str().unwrap()]);
    assert!(out.status.success(), "run failed: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("PlanReview"));

    // Approve the plan: proceeds and blocks at WorkReview.
    let out = run(&["approve", "done-wi"]);
    assert!(out.status.success(), "approve failed: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("WorkReview"));

    // Approve the work: reaches Done.
    let out = run(&["approve", "done-wi"]);
    assert!(out.status.success(), "approve failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Done"), "unexpected output: {stdout}");
}
