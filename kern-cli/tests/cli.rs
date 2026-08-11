//! Real integration: spawns the compiled `kern` binary as a subprocess —
//! does not call internal functions directly. `KERN_HOME` isolates the
//! global project registry across parallel test runs.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn kern_cmd(kern_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kern"));
    cmd.env("KERN_HOME", kern_home);
    cmd
}

/// `project create` now resolves a real embedding provider as part of
/// creation (see kern-cli's setup module) — these tests need a real
/// backend to prove anything genuine, same "skip, don't fail, if
/// unavailable" convention already used throughout kern-model's and
/// kern-vector's real-integration tests. A raw TCP connect is enough here
/// (no need to pull in an async HTTP client just for a test gate).
fn ollama_available() -> bool {
    std::net::TcpStream::connect_timeout(
        &"127.0.0.1:11434".parse().unwrap(),
        Duration::from_millis(500),
    )
    .is_ok()
}

fn create_args<'a>(name: &'a str, path: &'a str) -> Vec<&'a str> {
    vec![
        "project",
        "create",
        name,
        "--path",
        path,
        "--embedding-provider",
        "ollama",
        "--embedding-model",
        "all-minilm",
    ]
}

#[test]
fn project_create_and_status_work_end_to_end() {
    if !ollama_available() {
        eprintln!("Ollama is not running on :11434 — skipping integration test");
        return;
    }

    let kern_home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::write(project_dir.path().join("doc.md"), "# Title\ntest content\n").unwrap();

    let create = kern_cmd(kern_home.path())
        .args(create_args("acme", project_dir.path().to_str().unwrap()))
        .output()
        .unwrap();
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(String::from_utf8_lossy(&create.stdout).contains("acme"));

    // .kern/ was created with registry + vectors + config.
    assert!(project_dir
        .path()
        .join(".kern")
        .join("registry.db")
        .exists());
    assert!(project_dir.path().join(".kern").join("vectors").exists());
    assert!(project_dir
        .path()
        .join(".kern")
        .join("config.toml")
        .exists());

    let status = kern_cmd(kern_home.path())
        .args(["status", "--project", "acme"])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(status_out.contains("acme"));
    // 8 canonical types seeded on creation, 0 chunks (no `serve` yet).
    assert!(status_out.contains("8 canonical"));
    assert!(status_out.contains("chunks indexed: 0"));
    assert!(status_out.contains("all-minilm"));
    assert!(status_out.contains("ollama"));
}

#[test]
fn project_create_with_existing_name_fails() {
    if !ollama_available() {
        eprintln!("Ollama is not running on :11434 — skipping integration test");
        return;
    }

    let kern_home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();

    let first = kern_cmd(kern_home.path())
        .args(create_args("dup", project_dir.path().to_str().unwrap()))
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let second = kern_cmd(kern_home.path())
        .args(create_args("dup", project_dir.path().to_str().unwrap()))
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("PROJECT_ALREADY_EXISTS"));
}

#[test]
fn status_of_nonexistent_project_fails_with_clear_message() {
    let kern_home = tempfile::tempdir().unwrap();

    let status = kern_cmd(kern_home.path())
        .args(["status", "--project", "nao-existe"])
        .output()
        .unwrap();

    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("PROJECT_NOT_FOUND"));
}

#[test]
fn status_without_project_lists_registered() {
    if !ollama_available() {
        eprintln!("Ollama is not running on :11434 — skipping integration test");
        return;
    }

    let kern_home = tempfile::tempdir().unwrap();

    let empty = kern_cmd(kern_home.path()).arg("status").output().unwrap();
    assert!(empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stdout).contains("no project created"));

    let project_dir = tempfile::tempdir().unwrap();
    let create = kern_cmd(kern_home.path())
        .args(create_args("listado", project_dir.path().to_str().unwrap()))
        .output()
        .unwrap();
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let listing = kern_cmd(kern_home.path()).arg("status").output().unwrap();
    assert!(listing.status.success());
    assert!(String::from_utf8_lossy(&listing.stdout).contains("listado"));
}

/// A genuinely new, unconditional (no Ollama needed) regression guard:
/// `project create` with no provider flags and no TTY (exactly this
/// subprocess's stdin) must fail fast and clearly, never hang waiting for
/// interactive input that can never arrive.
#[test]
fn project_create_without_flags_and_without_a_tty_fails_fast_instead_of_hanging() {
    let kern_home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();

    let create = kern_cmd(kern_home.path())
        .args([
            "project",
            "create",
            "no-tty-test",
            "--path",
            project_dir.path().to_str().unwrap(),
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();

    assert!(!create.status.success());
    assert!(String::from_utf8_lossy(&create.stderr).contains("PROJECT_NOT_CONFIGURED"));
}

/// `serve` against a project that predates provider configuration (or
/// whose config was never set) must fail clearly and immediately, not
/// hang or crash opaquely.
#[test]
fn serve_on_an_unconfigured_project_fails_clearly() {
    let kern_home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();

    // Register the project directly via a config-less `project create`
    // attempt is impossible now (it always resolves a provider) — so
    // instead simulate a legacy/pre-initiative project: a registry entry
    // with no .kern/config.toml at all.
    std::fs::create_dir_all(project_dir.path().join(".kern")).unwrap();
    let registry_path = kern_home.path().join("projects.json");
    std::fs::write(
        &registry_path,
        format!(
            r#"{{"legacy": {:?}}}"#,
            project_dir.path().to_string_lossy()
        ),
    )
    .unwrap();

    let serve = kern_cmd(kern_home.path())
        .args(["serve", "--project", "legacy"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();

    assert!(!serve.status.success());
    assert!(String::from_utf8_lossy(&serve.stderr).contains("PROJECT_NOT_CONFIGURED"));
}
