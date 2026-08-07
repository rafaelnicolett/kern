//! Real integration: spawns the compiled `kern` binary as a subprocess —
//! does not call internal functions directly. `KERN_HOME` isolates the
//! global project registry across parallel test runs.

use std::path::Path;
use std::process::Command;

fn kern_cmd(kern_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kern"));
    cmd.env("KERN_HOME", kern_home);
    cmd
}

#[test]
fn project_create_and_status_work_end_to_end() {
    let kern_home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::write(project_dir.path().join("doc.md"), "# Title\ntest content\n").unwrap();

    let create = kern_cmd(kern_home.path())
        .args([
            "project",
            "create",
            "acme",
            "--path",
            project_dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(String::from_utf8_lossy(&create.stdout).contains("acme"));

    // .kern/ was created with registry + vectors.
    assert!(project_dir
        .path()
        .join(".kern")
        .join("registry.db")
        .exists());
    assert!(project_dir.path().join(".kern").join("vectors").exists());

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
}

#[test]
fn project_create_with_existing_name_fails() {
    let kern_home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();

    let first = kern_cmd(kern_home.path())
        .args([
            "project",
            "create",
            "dup",
            "--path",
            project_dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(first.status.success());

    let second = kern_cmd(kern_home.path())
        .args([
            "project",
            "create",
            "dup",
            "--path",
            project_dir.path().to_str().unwrap(),
        ])
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
    let kern_home = tempfile::tempdir().unwrap();

    let empty = kern_cmd(kern_home.path()).arg("status").output().unwrap();
    assert!(empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stdout).contains("no project created"));

    let project_dir = tempfile::tempdir().unwrap();
    kern_cmd(kern_home.path())
        .args([
            "project",
            "create",
            "listado",
            "--path",
            project_dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let listing = kern_cmd(kern_home.path()).arg("status").output().unwrap();
    assert!(listing.status.success());
    assert!(String::from_utf8_lossy(&listing.stdout).contains("listado"));
}
