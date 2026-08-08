//! End-to-end integration tests for repository-scoped CLI behavior.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_quorum"))
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    let output = Command::new("git")
        .args(["init", "--quiet"])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "git init failed: {output:?}");
}

fn init_bare_repo(path: &Path) {
    let output = Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git init --bare failed: {output:?}"
    );
}

fn quorum(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .env("HOME", home)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn register(home: &Path, repo: &Path) -> Output {
    quorum(home, repo, &["repo", "register"])
}

#[test]
fn run_advances_to_plan_review_and_status_reads_it_back() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);

    let registration = register(home, &repo);
    assert!(
        registration.status.success(),
        "register failed: {registration:?}"
    );

    let wi = repo.join("mywi.md");
    std::fs::write(&wi, "# My WI\n").unwrap();
    let out = quorum(home, &repo, &["run", "--dry-run", wi.to_str().unwrap()]);
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

    let out = quorum(home, &repo, &["status", "mywi"]);
    assert!(out.status.success(), "status failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PlanReview"), "unexpected output: {stdout}");
}

#[test]
fn approve_gates_drive_work_item_to_done() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);
    assert!(register(home, &repo).status.success());

    let wi = repo.join("done-wi.md");
    std::fs::write(&wi, "# WI\ndo it\n").unwrap();

    let out = quorum(home, &repo, &["run", wi.to_str().unwrap(), "--dry-run"]);
    assert!(out.status.success(), "run failed: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("PlanReview"));

    let out = quorum(home, &repo, &["approve", "done-wi", "--dry-run"]);
    assert!(out.status.success(), "approve failed: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("WorkReview"));

    let out = quorum(home, &repo, &["approve", "done-wi", "--dry-run"]);
    assert!(out.status.success(), "approve failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Done"), "unexpected output: {stdout}");
}

#[test]
fn context_overrides_cwd_and_nested_paths_resolve_to_root() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let registered = home.join("registered");
    let other = home.join("other");
    let nested = registered.join("a/b");
    init_repo(&registered);
    init_repo(&other);
    std::fs::create_dir_all(&nested).unwrap();

    let out = quorum(
        home,
        &other,
        &["--context", nested.to_str().unwrap(), "repo", "register"],
    );
    assert!(out.status.success(), "register failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let canonical_registered = std::fs::canonicalize(&registered).unwrap();
    let canonical_other = std::fs::canonicalize(&other).unwrap();
    assert!(stdout.contains(canonical_registered.to_str().unwrap()));
    assert!(!stdout.contains(canonical_other.to_str().unwrap()));

    let wi = registered.join("context-wi.md");
    std::fs::write(&wi, "# Context WI\n").unwrap();
    let out = quorum(
        home,
        &other,
        &[
            "--context",
            registered.to_str().unwrap(),
            "run",
            "--dry-run",
            wi.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "context run failed: {out:?}");
}

#[test]
fn explicit_repo_path_overrides_context_and_bare_repositories_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let explicit = home.join("explicit");
    let context = home.join("context");
    let bare = home.join("bare.git");
    init_repo(&explicit);
    init_repo(&context);
    init_bare_repo(&bare);

    let out = quorum(
        home,
        &context,
        &[
            "--context",
            context.to_str().unwrap(),
            "repo",
            "register",
            explicit.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "explicit registration failed: {out:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(std::fs::canonicalize(&explicit).unwrap().to_str().unwrap()));
    assert!(!stdout.contains(std::fs::canonicalize(&context).unwrap().to_str().unwrap()));

    let out = quorum(
        home,
        &context,
        &["repo", "register", bare.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not inside a Git working tree"));
}

#[test]
fn unregister_blocks_wi_commands_and_reregister_keeps_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);

    let registered = register(home, &repo);
    assert!(registered.status.success());
    let registered_stdout = String::from_utf8_lossy(&registered.stdout);
    let repository_id = registered_stdout
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string();

    let wi = repo.join("mywi.md");
    std::fs::write(&wi, "# WI\n").unwrap();
    assert!(
        quorum(home, &repo, &["run", "--dry-run", wi.to_str().unwrap()])
            .status
            .success()
    );

    let out = quorum(home, &repo, &["repo", "unregister"]);
    assert!(out.status.success(), "unregister failed: {out:?}");
    let out = quorum(home, &repo, &["status", "mywi"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not registered"));

    let reregistered = register(home, &repo);
    assert!(reregistered.status.success());
    assert!(
        String::from_utf8_lossy(&reregistered.stdout).contains(&repository_id),
        "repository identity changed after re-registration"
    );
    assert!(quorum(home, &repo, &["status", "mywi"]).status.success());
}

#[test]
fn identical_wi_slugs_are_scoped_by_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let one = home.join("one");
    let two = home.join("two");
    init_repo(&one);
    init_repo(&two);
    assert!(register(home, &one).status.success());
    assert!(register(home, &two).status.success());

    for repo in [&one, &two] {
        let wi = repo.join("same.md");
        std::fs::write(&wi, format!("# {}\n", repo.display())).unwrap();
        let out = quorum(home, repo, &["run", "--dry-run", wi.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "run failed in {}: {out:?}",
            repo.display()
        );
        assert!(quorum(home, repo, &["status", "same"]).status.success());
    }

    let listed = quorum(home, home, &["repo", "list"]);
    assert!(listed.status.success());
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains(one.to_str().unwrap()));
    assert!(stdout.contains(two.to_str().unwrap()));
}
