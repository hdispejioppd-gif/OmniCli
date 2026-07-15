use assert_cmd::Command;
use predicates::prelude::*;
use sha2::{Digest, Sha256};
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

fn omni(temp: &TempDir) -> Command {
    let mut command = Command::cargo_bin("omni").expect("binary exists");
    command.args([
        "--data-dir",
        temp.path().join("data").to_str().expect("utf-8 path"),
        "--workspace",
        temp.path().to_str().expect("utf-8 path"),
    ]);
    command
}

fn rust_fixture(temp: &TempDir) {
    std::fs::create_dir(temp.path().join("src")).expect("src directory");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("manifest");
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("source");
}

#[test]
fn help_exposes_core_commands() {
    Command::cargo_bin("omni")
        .expect("binary exists")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("sessions"))
        .stdout(predicate::str::contains("workflow"))
        .stdout(predicate::str::contains("tui"))
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("ask"))
        .stdout(predicate::str::contains("plan"))
        .stdout(predicate::str::contains("review"))
        .stdout(predicate::str::contains("models"))
        .stdout(predicate::str::contains("tools"))
        .stdout(predicate::str::contains("context"));
}

#[test]
fn fake_provider_streams_ndjson_and_persists_session() {
    let temp = TempDir::new().expect("temp dir");
    omni(&temp)
        .args(["--json", "run", "hello agent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"type\":\"run_started\""))
        .stdout(predicate::str::contains("\"type\":\"run_finished\""));

    omni(&temp)
        .args(["sessions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello agent"));
}

#[test]
fn read_tool_stays_inside_workspace() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(temp.path().join("sample.txt"), "tool payload").expect("fixture");

    omni(&temp)
        .args(["run", "read sample.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tool payload"));

    omni(&temp)
        .args(["run", "read ../secret.txt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path must be relative"));
}

#[test]
fn write_and_shell_require_explicit_permissions() {
    let temp = TempDir::new().expect("temp dir");
    omni(&temp)
        .args(["run", "write output.txt::hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--allow-write"));
    assert!(!temp.path().join("output.txt").exists());

    omni(&temp)
        .args(["run", "--allow-write", "write output.txt::hello"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(temp.path().join("output.txt")).unwrap(),
        "hello"
    );

    omni(&temp)
        .args(["run", "--allow-write", "write output.txt::replacement"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path already exists"));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("output.txt")).unwrap(),
        "hello"
    );

    omni(&temp)
        .args(["run", "shell echo forbidden"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--allow-shell"));
}

#[test]
fn patch_requires_hash_and_changes_one_exact_match() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "alpha beta gamma").unwrap();
    let hash = hex::encode(Sha256::digest(b"alpha beta gamma"));
    let prompt = format!("patch sample.txt::{hash}::beta::delta");

    omni(&temp)
        .args(["run", "--allow-write", &prompt])
        .assert()
        .success()
        .stdout(predicate::str::contains("sha256_after"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "alpha delta gamma");
}

#[test]
fn git_status_is_read_only_and_does_not_require_shell_permission() {
    let temp = TempDir::new().expect("temp dir");
    let status = ProcessCommand::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(temp.path())
        .status()
        .expect("git starts");
    assert!(status.success());
    std::fs::write(temp.path().join("untracked.txt"), "content").unwrap();

    omni(&temp)
        .args(["run", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("untracked.txt"));
}

#[test]
fn git_diff_uses_builtin_read_only_diff() {
    let temp = TempDir::new().expect("temp dir");
    let run_git = |args: &[&str]| {
        let status = ProcessCommand::new("git")
            .args(args)
            .current_dir(temp.path())
            .status()
            .expect("git starts");
        assert!(status.success());
    };
    run_git(&["init", "--initial-branch=main"]);
    std::fs::write(temp.path().join("tracked.txt"), "before\n").unwrap();
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Omni Test",
        "-c",
        "user.email=omni@example.invalid",
        "commit",
        "-m",
        "fixture",
    ]);
    std::fs::write(temp.path().join("tracked.txt"), "after\n").unwrap();

    omni(&temp)
        .args(["run", "diff"])
        .assert()
        .success()
        .stdout(predicate::str::contains("-before"))
        .stdout(predicate::str::contains("+after"));
}

#[test]
fn openai_provider_requires_environment_key() {
    let temp = TempDir::new().expect("temp dir");
    omni(&temp)
        .env_remove("OPENAI_API_KEY")
        .args(["--provider", "openai", "run", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("provider API key is not set"));
}

#[test]
fn model_requested_checks_require_verify_permission() {
    let temp = TempDir::new().expect("temp dir");
    rust_fixture(&temp);
    omni(&temp)
        .args(["run", "checks"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("validation requires --verify"));
}

#[test]
fn verify_runs_automatically_after_workspace_change() {
    let temp = TempDir::new().expect("temp dir");
    rust_fixture(&temp);
    omni(&temp)
        .args([
            "--json",
            "run",
            "--allow-write",
            "--verify",
            "write notes.txt::validated",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\":\"run_checks\""))
        .stdout(predicate::str::contains("\"code\":\"checks_passed\""))
        .stdout(predicate::str::contains("\"type\":\"run_finished\""));
}

#[test]
fn verify_rejects_changed_projects_without_detected_checks() {
    let temp = TempDir::new().expect("temp dir");
    omni(&temp)
        .args([
            "run",
            "--allow-write",
            "--verify",
            "write notes.txt::unverified",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no supported project checks were detected",
        ));
}

#[test]
fn mcp_server_mode_emits_only_json_rpc() {
    let temp = TempDir::new().expect("temp dir");
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n"
    );
    let output = omni(&temp)
        .args(["mcp", "serve"])
        .write_stdin(input)
        .output()
        .expect("MCP server runs");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line).expect("stdout line is JSON-RPC");
    }
    assert!(lines[1].contains("run_checks"));
}

#[test]
fn configured_mcp_child_registers_and_executes_namespaced_tool() {
    let temp = TempDir::new().expect("temp dir");
    let status = ProcessCommand::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(temp.path())
        .status()
        .expect("git starts");
    assert!(status.success());
    let executable = assert_cmd::cargo::cargo_bin("omni");
    let quote = |value: &std::path::Path| {
        value
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    };
    let config = temp.path().join("mcp.toml");
    let forwarded_environment = if cfg!(windows) {
        "[\"PATH\", \"SystemRoot\"]"
    } else {
        "[\"PATH\"]"
    };
    std::fs::write(
        &config,
        format!(
            "[mcp.servers.local]\ncommand = \"{}\"\nargs = [\"--workspace\", \"{}\", \"mcp\", \"serve\"]\nenv = {}\n",
            quote(&executable),
            quote(temp.path()),
            forwarded_environment,
        ),
    )
    .unwrap();

    omni(&temp)
        .args([
            "--config",
            config.to_str().unwrap(),
            "run",
            "--allow-mcp-start",
            "--allow-mcp-call",
            "mcp mcp__local__git_status::{}",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("branch.head main"));
}

#[test]
fn workflow_runs_dependency_graph_and_emits_one_json_report() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(temp.path().join("first.txt"), "first").unwrap();
    std::fs::write(temp.path().join("second.txt"), "second").unwrap();
    std::fs::write(
        temp.path().join("workflow.yml"),
        r#"version: 1
steps:
  - id: first
    tool: read_file
    arguments:
      path: first.txt
  - id: second
    tool: read_file
    arguments:
      path: second.txt
  - id: final
    tool: read_file
    arguments:
      path: workflow.yml
    needs: [first, second]
"#,
    )
    .unwrap();

    let output = omni(&temp)
        .args(["--json", "workflow", "run", "workflow.yml"])
        .output()
        .expect("workflow runs");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(report["status"], "succeeded");
    assert_eq!(report["steps"].as_array().unwrap().len(), 3);
}

#[test]
fn workflow_emits_hashed_artifact_metadata() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(temp.path().join("artifact.txt"), "artifact payload").unwrap();
    std::fs::write(
        temp.path().join("workflow.yml"),
        r#"version: 2
steps:
  - id: capture
    tool: read_file
    arguments:
      path: artifact.txt
    artifacts:
      - artifact.txt
"#,
    )
    .unwrap();
    let output = omni(&temp)
        .args(["--json", "workflow", "run", "workflow.yml"])
        .output()
        .expect("workflow runs");
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["steps"][0]["artifacts"][0]["path"], "artifact.txt");
    assert_eq!(report["steps"][0]["artifacts"][0]["size_bytes"], 16);
    assert_eq!(
        report["steps"][0]["artifacts"][0]["sha256"],
        hex::encode(Sha256::digest(b"artifact payload"))
    );
}

#[test]
fn workflow_permission_failure_skips_only_descendants() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(temp.path().join("safe.txt"), "safe").unwrap();
    std::fs::write(
        temp.path().join("workflow.yml"),
        r#"version: 1
steps:
  - id: denied
    tool: shell
    arguments:
      command: echo forbidden
  - id: blocked
    tool: read_file
    arguments:
      path: safe.txt
    needs: [denied]
  - id: independent
    tool: read_file
    arguments:
      path: safe.txt
"#,
    )
    .unwrap();

    let output = omni(&temp)
        .args(["--json", "workflow", "run", "workflow.yml"])
        .output()
        .expect("workflow runs");
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["steps"][0]["status"], "failed");
    assert_eq!(report["steps"][1]["status"], "skipped");
    assert_eq!(report["steps"][2]["status"], "succeeded");
}

#[test]
fn workflow_resume_reuses_successes_and_current_permissions() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(temp.path().join("safe.txt"), "safe").unwrap();
    std::fs::write(
        temp.path().join("workflow.yml"),
        r#"version: 1
steps:
  - id: read
    tool: read_file
    arguments:
      path: safe.txt
  - id: create
    tool: create_file
    arguments:
      path: created.txt
      content: resumed
    needs: [read]
"#,
    )
    .unwrap();

    let first = omni(&temp)
        .args(["--json", "workflow", "run", "workflow.yml"])
        .output()
        .expect("first workflow run");
    assert!(!first.status.success());
    let first_report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let run_id = first_report["run_id"].as_str().unwrap();
    assert_eq!(first_report["steps"][0]["attempts"], 1);
    assert_eq!(first_report["steps"][1]["status"], "failed");

    let resumed = omni(&temp)
        .args(["--json", "workflow", "resume", run_id, "--allow-write"])
        .output()
        .expect("resumed workflow run");
    assert!(resumed.status.success());
    let resumed_report: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed_report["run_id"], run_id);
    assert_eq!(resumed_report["steps"][0]["attempts"], 1);
    assert_eq!(resumed_report["steps"][1]["attempts"], 2);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("created.txt")).unwrap(),
        "resumed"
    );
}

#[test]
fn workflow_resume_rejects_semantic_changes() {
    let temp = TempDir::new().expect("temp dir");
    let workflow_path = temp.path().join("workflow.yml");
    std::fs::write(
        &workflow_path,
        r#"version: 1
steps:
  - id: create
    tool: create_file
    arguments:
      path: original.txt
      content: original
"#,
    )
    .unwrap();
    let first = omni(&temp)
        .args(["--json", "workflow", "run", "workflow.yml"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let run_id = report["run_id"].as_str().unwrap();

    std::fs::write(
        &workflow_path,
        r#"version: 1
steps:
  - id: create
    tool: create_file
    arguments:
      path: changed.txt
      content: changed
"#,
    )
    .unwrap();
    omni(&temp)
        .args(["workflow", "resume", run_id, "--allow-write"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "workflow changed since the run was created",
        ));
    assert!(!temp.path().join("changed.txt").exists());
}

#[test]
fn tui_rejects_json_and_redirected_terminals_without_hanging() {
    let temp = TempDir::new().expect("temp dir");
    omni(&temp)
        .args(["--json", "tui"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--json cannot be used with tui"));
    omni(&temp)
        .arg("tui")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "TUI requires interactive stdin and stdout",
        ));
}

#[test]
fn managed_worktree_cli_creates_selects_and_removes_isolated_checkout() {
    let temp = TempDir::new().expect("temp dir");
    let data = TempDir::new().expect("external data dir");
    let worktree_cli = || {
        let mut command = Command::cargo_bin("omni").expect("binary exists");
        command.args([
            "--data-dir",
            data.path().to_str().unwrap(),
            "--workspace",
            temp.path().to_str().unwrap(),
        ]);
        command
    };
    let run_git = |args: &[&str]| {
        let status = ProcessCommand::new("git")
            .args(args)
            .current_dir(temp.path())
            .status()
            .expect("git starts");
        assert!(status.success());
    };
    run_git(&["init", "--initial-branch=main"]);
    std::fs::write(temp.path().join("tracked.txt"), "isolated payload").unwrap();
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.name=Omni Test",
        "-c",
        "user.email=omni@example.invalid",
        "commit",
        "-m",
        "fixture",
    ]);

    let created = worktree_cli()
        .args(["--json", "worktree", "create", "task-17"])
        .output()
        .expect("worktree create runs");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let info: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(info["state"], "active");
    assert_eq!(info["branch_ref"], "refs/heads/omni/task-17");

    worktree_cli()
        .args(["--worktree", "task-17", "run", "read tracked.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("isolated payload"));

    worktree_cli()
        .args(["worktree", "create", "task-18"])
        .assert()
        .success();
    std::fs::write(
        temp.path().join("tasks.yml"),
        r#"version: 1
tasks:
  - id: first
    worktree: task-17
    prompt: read tracked.txt
    verify: false
  - id: second
    worktree: task-18
    prompt: status
    verify: false
"#,
    )
    .unwrap();
    let supervised = worktree_cli()
        .args([
            "--json",
            "supervisor",
            "run",
            "tasks.yml",
            "--concurrency",
            "2",
        ])
        .output()
        .expect("supervisor runs");
    assert!(
        supervised.status.success(),
        "{}",
        String::from_utf8_lossy(&supervised.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&supervised.stdout).unwrap();
    assert_eq!(report["status"], "succeeded");
    assert_ne!(
        report["tasks"][0]["session_id"],
        report["tasks"][1]["session_id"]
    );

    for name in ["task-17", "task-18"] {
        worktree_cli()
            .args(["worktree", "remove", name])
            .assert()
            .success();
    }
}

#[test]
fn ask_and_plan_are_read_only_and_persist_sessions() {
    let temp = TempDir::new().expect("temp dir");
    omni(&temp)
        .args(["ask", "what language is this project"])
        .assert()
        .success();

    omni(&temp).args(["plan", "add logging"]).assert().success();

    omni(&temp)
        .args(["sessions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("what language is this project"))
        .stdout(predicate::str::contains("add logging"));

    omni(&temp)
        .args(["ask", "write output.txt::hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("permission denied"));
    assert!(!temp.path().join("output.txt").exists());
}

#[test]
fn review_is_read_only_and_can_run_checks_with_verify() {
    let temp = TempDir::new().expect("temp dir");
    rust_fixture(&temp);

    omni(&temp)
        .args(["--json", "review", "--verify"])
        .assert()
        .success()
        .stdout(predicate::str::contains("run_started"))
        .stdout(predicate::str::contains("run_finished"));
}

#[test]
fn models_lists_configured_selectors() {
    let temp = TempDir::new().expect("temp dir");
    omni(&temp)
        .args(["--json", "models"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fake"))
        .stdout(predicate::str::contains("openai/gpt-4.1-mini"))
        .stdout(predicate::str::contains(
            "anthropic/claude-sonnet-4-20250514",
        ));

    omni(&temp)
        .args(["models"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fake\n"))
        .stdout(predicate::str::contains("openai/gpt-4.1-mini\n"))
        .stdout(predicate::str::contains(
            "anthropic/claude-sonnet-4-20250514\n",
        ));
}

#[test]
fn tools_lists_builtin_tools() {
    let temp = TempDir::new().expect("temp dir");
    omni(&temp)
        .args(["--json", "tools"])
        .assert()
        .success()
        .stdout(predicate::str::contains("read_file"))
        .stdout(predicate::str::contains("apply_patch"))
        .stdout(predicate::str::contains("search_files"))
        .stdout(predicate::str::contains("web_fetch"))
        .stdout(predicate::str::contains("web_search"));

    omni(&temp)
        .args(["tools"])
        .assert()
        .success()
        .stdout(predicate::str::contains("read_file\t"))
        .stdout(predicate::str::contains("apply_patch\t"))
        .stdout(predicate::str::contains("search_files\t"))
        .stdout(predicate::str::contains("web_fetch\t"))
        .stdout(predicate::str::contains("web_search\t"));
}

#[test]
fn context_indexes_and_queries_workspace() {
    let temp = TempDir::new().expect("temp dir");
    rust_fixture(&temp);

    omni(&temp)
        .args(["--json", "context", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rust"))
        .stdout(predicate::str::contains("cargo"));

    omni(&temp)
        .args(["--json", "context", "query", "lib", "--limit", "5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("src/lib.rs"));
}

#[test]
fn search_files_tool_finds_relevant_files() {
    let temp = TempDir::new().expect("temp dir");
    rust_fixture(&temp);

    omni(&temp)
        .args(["run", "search lib"])
        .assert()
        .success()
        .stdout(predicate::str::contains("src/lib.rs"));
}

#[test]
fn profile_offline_switches_provider_to_fake() {
    let temp = TempDir::new().expect("temp dir");
    omni(&temp)
        .args(["--profile", "offline", "--json", "doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"provider\":\"fake\""));

    omni(&temp)
        .args(["--profile", "unknown", "doctor"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown profile"));
}

#[test]
fn custom_profile_overrides_defaults() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(
        temp.path().join("omni.toml"),
        r#"
max_turns = 20

[profiles.fast]
max_turns = 2
max_tool_output_bytes = 1024
"#,
    )
    .unwrap();

    let output = omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--profile",
            "fast",
            "--json",
            "doctor",
        ])
        .output()
        .expect("doctor runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"profile\":\"fast\""));
}

#[test]
fn completions_generates_scripts_for_supported_shells() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        Command::cargo_bin("omni")
            .expect("binary exists")
            .args(["completions", shell])
            .assert()
            .success();
    }

    Command::cargo_bin("omni")
        .expect("binary exists")
        .args(["completions", "unknown"])
        .assert()
        .failure();
}

#[test]
fn lm_studio_provider_appears_in_models_and_doctor() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(
        temp.path().join("omni.toml"),
        r#"
provider = "lm-studio"

[lm_studio]
base_url = "http://localhost:1234/v1/"
model = "my-local-model"
timeout_seconds = 60
"#,
    )
    .unwrap();

    let output = omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "models",
        ])
        .output()
        .expect("models runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("lm-studio/my-local-model"));

    let output = omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "doctor",
        ])
        .output()
        .expect("doctor runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"provider\":\"lm-studio\""));
    assert!(stdout.contains("\"model\":\"my-local-model\""));
    assert!(stdout.contains("\"base_url\":\"http://localhost:1234/v1/\""));
    assert!(stdout.contains("\"api_key_present\":true"));
}

#[test]
fn llama_cpp_provider_appears_in_models_and_doctor() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(
        temp.path().join("omni.toml"),
        r#"
provider = "llama-cpp"

[llama_cpp]
base_url = "http://localhost:8080"
model = "my-gguf"
timeout_seconds = 60
temperature = 0.5
n_predict = 512
"#,
    )
    .unwrap();

    let output = omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "models",
        ])
        .output()
        .expect("models runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("llama-cpp/my-gguf"));

    let output = omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "doctor",
        ])
        .output()
        .expect("doctor runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"provider\":\"llama-cpp\""));
    assert!(stdout.contains("\"model\":\"my-gguf\""));
    assert!(stdout.contains("\"base_url\":\"http://localhost:8080\""));
    assert!(stdout.contains("\"api_key_present\":true"));
}

#[test]
fn ollama_provider_appears_in_models_and_doctor() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(
        temp.path().join("omni.toml"),
        r#"
provider = "ollama"

[ollama]
base_url = "http://localhost:11434"
model = "llama3.1-test"
timeout_seconds = 60
"#,
    )
    .unwrap();

    let output = omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "models",
        ])
        .output()
        .expect("models runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ollama/llama3.1-test"));

    let output = omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "doctor",
        ])
        .output()
        .expect("doctor runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"provider\":\"ollama\""));
    assert!(stdout.contains("\"model\":\"llama3.1-test\""));
    assert!(stdout.contains("\"base_url\":\"http://localhost:11434\""));
}

#[test]
fn openai_compatible_provider_appears_in_models_and_requires_key_env() {
    let temp = TempDir::new().expect("temp dir");
    std::fs::write(
        temp.path().join("omni.toml"),
        r#"
provider = "openai-compatible"

[openai_compatible]
base_url = "https://generic.example.invalid/v1/"
model = "custom-model"
api_key_env = "CUSTOM_API_KEY"
timeout_seconds = 30
"#,
    )
    .unwrap();

    let output = omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "models",
        ])
        .output()
        .expect("models runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("openai-compatible/custom-model"));

    let output = omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "doctor",
        ])
        .output()
        .expect("doctor runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"provider\":\"openai-compatible\""));
    assert!(stdout.contains("\"status\":\"degraded\""));
    assert!(stdout.contains("\"api_key_present\":false"));
}

#[test]
fn plugin_tools_require_allow_plugins() {
    let temp = TempDir::new().expect("temp dir");
    let plugin_dir = temp.path().join("plugins/datetime");
    std::fs::create_dir_all(&plugin_dir).expect("plugin dir");

    std::fs::write(
        plugin_dir.join("omni-plugin.toml"),
        r#"
[plugin]
name = "datetime"
version = "0.1.0"
description = "datetime tools"
entrypoint = "datetime.py"
"#,
    )
    .unwrap();

    std::fs::write(
        plugin_dir.join("datetime.py"),
        r#"#!/usr/bin/env python3
import json, sys

def send(v): print(json.dumps(v), flush=True)

def main():
    for line in sys.stdin:
        req = json.loads(line)
        mid = req.get("id")
        method = req.get("method")
        if method == "initialize":
            send({"jsonrpc":"2.0","id":mid,"result":{"name":"datetime","version":"0.1.0"}})
        elif method == "tools/list":
            send({"jsonrpc":"2.0","id":mid,"result":[{"name":"now","description":"now","input_schema":{"type":"object","additionalProperties":False}}]})
        elif method == "tools/call":
            send({"jsonrpc":"2.0","id":mid,"result":{"success":True,"stdout":"2026-07-13","stderr":"","truncated":False,"metadata":{}}})
        else:
            send({"jsonrpc":"2.0","id":mid,"error":{"code":-32601,"message":"unknown"}})

if __name__ == "__main__":
    main()
"#,
    )
    .unwrap();

    let manifest = plugin_dir.join("omni-plugin.toml");
    std::fs::write(
        temp.path().join("omni.toml"),
        format!(
            r#"
[plugins.datetime]
manifest = "{}"
"#,
            manifest.to_str().unwrap().replace('\\', "/")
        ),
    )
    .unwrap();

    omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "plugins",
            "list",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("datetime"))
        .stdout(predicate::str::contains("now"));

    omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "run",
            "plugin plugin__datetime__now::{}",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown tool"));

    omni(&temp)
        .args([
            "--config",
            temp.path().join("omni.toml").to_str().unwrap(),
            "--json",
            "run",
            "--allow-plugins",
            "plugin plugin__datetime__now::{}",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("2026-07-13"));
}
