//! End-to-end integration tests for repository-scoped CLI behavior.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use quorum_core::{branch_name, Database, RepositoryRoot};

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
    std::fs::write(path.join("README.md"), "# Test\n").unwrap();
    for args in [
        vec!["config", "user.name", "Quorum Test"],
        vec!["config", "user.email", "quorum@example.com"],
        vec!["add", "README.md"],
        vec!["commit", "--quiet", "-m", "initial"],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git setup failed: {output:?}");
    }
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

fn only_worktree(home: &Path) -> PathBuf {
    let entries = std::fs::read_dir(home.join(".quorum/state"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    entries[0].path().join("implementation")
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git command failed: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
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

    let work_item = repo.join("my-work-item.md");
    std::fs::write(&work_item, "# My Work Item\n").unwrap();
    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", work_item.to_str().unwrap()],
    );
    assert!(out.status.success(), "run failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PlanReview"), "unexpected output: {stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Intake Planner:planner-a started"),
        "missing live progress: {stderr}"
    );
    assert!(
        stderr.contains("completed"),
        "missing completion progress: {stderr}"
    );
    assert!(
        stdout.contains("copilot --resume quorum/my-work-item/PlanReview"),
        "missing resume command: {stdout}"
    );

    assert!(home.join(".quorum/quorum.db").exists());
    let worktree = only_worktree(home);
    let state_dir = worktree.parent().unwrap();
    assert_ne!(
        state_dir.file_name().unwrap().to_string_lossy(),
        "my-work-item",
        "filesystem state must use the stable internal id"
    );
    assert!(worktree.join(".git").is_file());

    let root = RepositoryRoot::discover(&repo).unwrap();
    let db = Database::open(&home.join(".quorum/quorum.db")).unwrap();
    let registered = db.registered_repository(&root).unwrap().unwrap();
    let internal_id = db
        .work_item_id(&registered.id, "my-work-item")
        .unwrap()
        .unwrap();
    let mut store = db.into_store(internal_id.clone()).unwrap();
    let long_plan = "0123456789".repeat(25);
    store.set_plan(&long_plan, "iteration=0").unwrap();
    let activity_count = store.activities().unwrap().len();
    drop(store);

    let out = quorum(home, &repo, &["status", "my-work-item"]);
    assert!(out.status.success(), "status failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PlanReview"), "unexpected output: {stdout}");
    assert!(stdout.contains("planning:"), "missing planning: {stdout}");
    assert!(
        stdout.contains("transitions:"),
        "missing transitions: {stdout}"
    );
    assert!(stdout.contains("activity"), "missing activity: {stdout}");
    assert!(
        !stdout.contains(&long_plan),
        "default status should abbreviate the plan"
    );

    let out = quorum(home, &repo, &["status", "my-work-item", "--verbose"]);
    assert!(out.status.success(), "verbose status failed: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&long_plan),
        "verbose status omitted the full plan"
    );

    let out = quorum(home, &repo, &["status", "my-work-item", "--json"]);
    assert!(out.status.success(), "JSON status failed: {out:?}");
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["version"], 4);
    assert_eq!(value["identity"]["slug"], "my-work-item");
    assert_eq!(value["state"]["current"], "plan_review");
    assert_eq!(value["planning"]["iterations"], 1);
    assert_eq!(value["planning"]["plan"], long_plan);
    let roles = value["activities"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|activity| activity["role"].as_str())
        .collect::<Vec<_>>();
    assert!(roles.iter().any(|role| role.starts_with("Planner:")));
    assert!(roles.contains(&"Coordinator:merge"));
    assert!(roles.iter().all(|role| {
        !["PL", "CO", "IM", "RV"]
            .iter()
            .any(|legacy| role.starts_with(legacy))
    }));

    let activity_count_after = Database::open(&home.join(".quorum/quorum.db"))
        .unwrap()
        .into_store(internal_id)
        .unwrap()
        .activities()
        .unwrap()
        .len();
    assert_eq!(activity_count_after, activity_count);
}

#[test]
fn quiet_suppresses_live_progress_but_not_final_report() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);
    assert!(register(home, &repo).status.success());
    let work_item = repo.join("quiet.md");
    std::fs::write(&work_item, "# Quiet\n").unwrap();

    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", "--quiet", work_item.to_str().unwrap()],
    );
    assert!(out.status.success(), "quiet run failed: {out:?}");
    assert!(
        out.stderr.is_empty(),
        "quiet run wrote stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("PlanReview"));
}

#[test]
fn approve_gates_drive_work_item_to_done() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);
    assert!(register(home, &repo).status.success());

    let work_item = repo.join("done-work-item.md");
    std::fs::write(&work_item, "# Work Item\ndo it\n").unwrap();

    let out = quorum(
        home,
        &repo,
        &["run", work_item.to_str().unwrap(), "--dry-run"],
    );
    assert!(out.status.success(), "run failed: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("PlanReview"));

    let out = quorum(home, &repo, &["approve", "done-work-item", "--dry-run"]);
    assert!(out.status.success(), "approve failed: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("WorkReview"));

    let out = quorum(home, &repo, &["approve", "done-work-item", "--dry-run"]);
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

    let work_item = registered.join("context-work-item.md");
    std::fs::write(&work_item, "# Context Work Item\n").unwrap();
    let out = quorum(
        home,
        &other,
        &[
            "--context",
            registered.to_str().unwrap(),
            "run",
            "--dry-run",
            work_item.to_str().unwrap(),
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
fn unregister_blocks_work_item_commands_and_reregister_keeps_identity() {
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

    let work_item = repo.join("my-work-item.md");
    std::fs::write(&work_item, "# Work Item\n").unwrap();
    assert!(quorum(
        home,
        &repo,
        &["run", "--dry-run", work_item.to_str().unwrap()]
    )
    .status
    .success());

    let out = quorum(home, &repo, &["repo", "unregister"]);
    assert!(out.status.success(), "unregister failed: {out:?}");
    let out = quorum(home, &repo, &["status", "my-work-item"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not registered"));

    let reregistered = register(home, &repo);
    assert!(reregistered.status.success());
    assert!(
        String::from_utf8_lossy(&reregistered.stdout).contains(&repository_id),
        "repository identity changed after re-registration"
    );
    assert!(quorum(home, &repo, &["status", "my-work-item"])
        .status
        .success());
}

#[test]
fn identical_work_item_slugs_are_scoped_by_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let one = home.join("one");
    let two = home.join("two");
    init_repo(&one);
    init_repo(&two);
    assert!(register(home, &one).status.success());
    assert!(register(home, &two).status.success());

    for repo in [&one, &two] {
        let work_item = repo.join("same.md");
        std::fs::write(&work_item, format!("# {}\n", repo.display())).unwrap();
        let out = quorum(
            home,
            repo,
            &["run", "--dry-run", work_item.to_str().unwrap()],
        );
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

#[test]
fn worktree_pins_committed_head_and_resumes_without_user_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);
    assert!(register(home, &repo).status.success());

    let base = git_stdout(&repo, &["rev-parse", "HEAD"]);
    let work_item = repo.join("pinned.md");
    std::fs::write(&work_item, "# Pinned\n").unwrap();
    std::fs::write(repo.join("local-only.txt"), "uncommitted\n").unwrap();
    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", work_item.to_str().unwrap()],
    );
    assert!(out.status.success(), "run failed: {out:?}");

    let worktree = only_worktree(home);
    assert_eq!(git_stdout(&worktree, &["rev-parse", "HEAD"]), base);
    assert!(!worktree.join("local-only.txt").exists());
    assert!(!worktree.join("pinned.md").exists());
    assert!(repo.join("local-only.txt").exists());

    std::fs::write(repo.join("later.txt"), "later\n").unwrap();
    for args in [
        &["add", "later.txt"][..],
        &["commit", "--quiet", "-m", "later"][..],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git commit failed: {output:?}");
    }
    assert_ne!(git_stdout(&repo, &["rev-parse", "HEAD"]), base);

    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", work_item.to_str().unwrap()],
    );
    assert!(out.status.success(), "resume failed: {out:?}");
    assert_eq!(git_stdout(&worktree, &["rev-parse", "HEAD"]), base);
    let listed = git_stdout(&repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        listed
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        2
    );
}

#[test]
fn plain_directory_and_branch_collisions_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);
    assert!(register(home, &repo).status.success());

    let root = RepositoryRoot::discover(&repo).unwrap();
    let mut database = Database::open(&home.join(".quorum/quorum.db")).unwrap();
    let registered = database.registered_repository(&root).unwrap().unwrap();

    let plain_id = database
        .get_or_create_work_item(&registered.id, "plain")
        .unwrap();
    std::fs::create_dir_all(
        home.join(".quorum/state")
            .join(plain_id.as_str())
            .join("implementation"),
    )
    .unwrap();
    let plain_wi = repo.join("plain.md");
    std::fs::write(&plain_wi, "# Plain\n").unwrap();
    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", plain_wi.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("path already exists"));

    let branch_id = database
        .get_or_create_work_item(&registered.id, "branch")
        .unwrap();
    let branch = branch_name("branch", &branch_id);
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", &branch, "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success(), "branch setup failed: {output:?}");
    let branch_wi = repo.join("branch.md");
    std::fs::write(&branch_wi, "# Branch\n").unwrap();
    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", branch_wi.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already exists"));
}

#[test]
fn run_requires_a_committed_head() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let output = Command::new("git")
        .args(["init", "--quiet"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(register(home, &repo).status.success());

    let work_item = repo.join("unborn.md");
    std::fs::write(&work_item, "# Unborn\n").unwrap();
    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", work_item.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("has no committed HEAD"));
}

#[test]
fn creating_record_resumes_worktree_setup_after_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);
    assert!(register(home, &repo).status.success());

    let root = RepositoryRoot::discover(&repo).unwrap();
    let mut database = Database::open(&home.join(".quorum/quorum.db")).unwrap();
    let registered = database.registered_repository(&root).unwrap().unwrap();
    let work_item = database
        .get_or_create_work_item(&registered.id, "crash")
        .unwrap();
    let branch = branch_name("crash", &work_item);
    let path = std::fs::canonicalize(home)
        .unwrap()
        .join(".quorum/state")
        .join(work_item.as_str())
        .join("implementation");
    let base = git_stdout(&repo, &["rev-parse", "HEAD"]);
    database
        .reserve_worktree(&work_item, &base, &branch, &path)
        .unwrap();
    drop(database);

    let work_item = repo.join("crash.md");
    std::fs::write(&work_item, "# Crash recovery\n").unwrap();
    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", work_item.to_str().unwrap()],
    );
    assert!(out.status.success(), "recovery failed: {out:?}");
    assert!(path.join(".git").is_file());
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), base);
}

#[test]
fn creating_record_rejects_a_branch_that_moved_off_base() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);
    assert!(register(home, &repo).status.success());

    let root = RepositoryRoot::discover(&repo).unwrap();
    let mut database = Database::open(&home.join(".quorum/quorum.db")).unwrap();
    let registered = database.registered_repository(&root).unwrap().unwrap();
    let work_item = database
        .get_or_create_work_item(&registered.id, "moved")
        .unwrap();
    let branch = branch_name("moved", &work_item);
    let path = std::fs::canonicalize(home)
        .unwrap()
        .join(".quorum/state")
        .join(work_item.as_str())
        .join("implementation");
    let base = git_stdout(&repo, &["rev-parse", "HEAD"]);
    database
        .reserve_worktree(&work_item, &base, &branch, &path)
        .unwrap();
    drop(database);

    std::fs::write(repo.join("later.txt"), "later\n").unwrap();
    for args in [
        &["add", "later.txt"][..],
        &["commit", "--quiet", "-m", "later"][..],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    let moved = git_stdout(&repo, &["rev-parse", "HEAD"]);
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", &branch, "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let work_item = repo.join("moved.md");
    std::fs::write(&work_item, "# Moved\n").unwrap();
    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", work_item.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("expected base"));
    assert_eq!(
        git_stdout(&repo, &["rev-parse", &format!("{branch}^{{commit}}")]),
        moved
    );
    assert!(!path.exists());
}

#[test]
fn reconcile_rejects_a_worktree_switched_to_another_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    init_repo(&repo);
    assert!(register(home, &repo).status.success());
    let work_item = repo.join("switched.md");
    std::fs::write(&work_item, "# Switched\n").unwrap();
    assert!(quorum(
        home,
        &repo,
        &["run", "--dry-run", work_item.to_str().unwrap()]
    )
    .status
    .success());

    let worktree = only_worktree(home);
    let output = Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["switch", "--quiet", "-c", "unrelated"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let out = quorum(
        home,
        &repo,
        &["run", "--dry-run", work_item.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("uses branch"));
    assert_eq!(
        git_stdout(&worktree, &["symbolic-ref", "--short", "HEAD"]),
        "unrelated"
    );
}
