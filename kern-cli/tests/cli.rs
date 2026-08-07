//! Integração real: spawna o binário `kern` compilado como subprocesso —
//! não chama funções internas. `KERN_HOME` isola o registry global de
//! projetos entre execuções de teste paralelas.

use std::path::Path;
use std::process::Command;

fn kern_cmd(kern_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kern"));
    cmd.env("KERN_HOME", kern_home);
    cmd
}

#[test]
fn project_create_e_status_funcionam_fim_a_fim() {
    let kern_home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        project_dir.path().join("doc.md"),
        "# Título\nconteúdo de teste\n",
    )
    .unwrap();

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

    // .kern/ foi criado com registro + vetores.
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
    // 8 tipos canônicos semeados na criação, 0 chunks (ainda sem `serve`).
    assert!(status_out.contains("8 canônicos"));
    assert!(status_out.contains("chunks indexados: 0"));
}

#[test]
fn project_create_com_nome_ja_existente_falha() {
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
    assert!(String::from_utf8_lossy(&second.stderr).contains("PROJETO_JA_EXISTE"));
}

#[test]
fn status_de_projeto_inexistente_falha_com_mensagem_clara() {
    let kern_home = tempfile::tempdir().unwrap();

    let status = kern_cmd(kern_home.path())
        .args(["status", "--project", "nao-existe"])
        .output()
        .unwrap();

    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("PROJETO_NAO_ENCONTRADO"));
}

#[test]
fn status_sem_projeto_lista_registrados() {
    let kern_home = tempfile::tempdir().unwrap();

    let empty = kern_cmd(kern_home.path()).arg("status").output().unwrap();
    assert!(empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stdout).contains("nenhum projeto"));

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
