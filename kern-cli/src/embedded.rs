//! Backend embarcado — extrai `llama-server` (+ libs compartilhadas) do
//! próprio binário do kern pro cache local, no primeiro uso. Só existe de
//! verdade quando a build foi feita com `--features bundled-llama-server`
//! (builds de release da CI, ver `.github/workflows/release.yml`); builds de
//! desenvolvimento não embutem nada e dependem só do probe de Ollama (ver
//! `cmd_serve` em `main.rs`).
//!
//! O modelo `.gguf` em si **não** é embutido (pesos de modelo são grandes
//! demais pro binário) — é resolvido separadamente a partir do cache local
//! (`~/.cache/kern/models/`). Download automático via Hugging Face Hub
//! ainda não está implementado (ver sprint-status.md, Sprint S5).

use std::path::PathBuf;

#[cfg(feature = "bundled-llama-server")]
static LLAMA_SERVER_BUNDLE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/llama-server-bundle.tar.gz"));

#[cfg(feature = "bundled-llama-server")]
fn binary_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

#[cfg(feature = "bundled-llama-server")]
fn default_cache_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("não foi possível resolver o diretório home"))?;
    Ok(home
        .join(".cache")
        .join("kern")
        .join("bin")
        .join("llama-server"))
}

/// Extrai o pacote embarcado pra `cache_dir` se ainda não estiver lá —
/// idempotente, chamadas repetidas não reextraem. Retorna o path do
/// executável `llama-server` já com permissão de execução (unix).
#[cfg(feature = "bundled-llama-server")]
fn ensure_extracted_at(cache_dir: &std::path::Path) -> anyhow::Result<PathBuf> {
    let binary_path = cache_dir.join(binary_name());

    if !binary_path.exists() {
        std::fs::create_dir_all(cache_dir)?;
        let gzip = flate2::read::GzDecoder::new(LLAMA_SERVER_BUNDLE);
        let mut archive = tar::Archive::new(gzip);
        archive.unpack(cache_dir)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms)?;
        }
    }

    if !binary_path.exists() {
        anyhow::bail!(
            "extração do pacote embarcado não produziu {} — pacote corrompido ou \
             layout inesperado",
            binary_path.display()
        );
    }
    Ok(binary_path)
}

#[cfg(feature = "bundled-llama-server")]
pub fn ensure_llama_server_binary() -> anyhow::Result<PathBuf> {
    ensure_extracted_at(&default_cache_dir()?)
}

#[cfg(not(feature = "bundled-llama-server"))]
pub fn ensure_llama_server_binary() -> anyhow::Result<PathBuf> {
    anyhow::bail!(
        "esta build não embute o llama-server (feature bundled-llama-server desativada \
         — só builds de release da CI a ativam)"
    )
}

/// Primeiro `.gguf` encontrado em `~/.cache/kern/models/`, ordem
/// alfabética — v0 não escolhe um modelo "oficial" default nem baixa nada
/// automaticamente (ver módulo doc). Falha explícita se o diretório não
/// existir ou estiver vazio; quem chama decide a mensagem de erro final.
pub fn find_cached_model() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let models_dir = home.join(".cache").join("kern").join("models");
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&models_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("gguf"))
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

#[cfg(all(test, feature = "bundled-llama-server"))]
mod tests {
    use super::*;

    /// Extrai o pacote embarcado de verdade (o binário de teste só existe
    /// quando compilado com `--features bundled-llama-server` e uma release
    /// CI real forneceu `KERN_LLAMA_SERVER_ARCHIVE` — não roda no `cargo
    /// test --workspace` padrão de desenvolvimento).
    #[test]
    fn ensure_extracted_at_produz_binario_executavel_e_e_idempotente() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("llama-server");

        let first = ensure_extracted_at(&cache_dir).expect("primeira extração deveria funcionar");
        assert!(first.exists());

        let modified_before = std::fs::metadata(&first).unwrap().modified().unwrap();
        let second =
            ensure_extracted_at(&cache_dir).expect("segunda chamada deveria ser idempotente");
        let modified_after = std::fs::metadata(&second).unwrap().modified().unwrap();
        assert_eq!(modified_before, modified_after, "não deveria reextrair");
    }
}
