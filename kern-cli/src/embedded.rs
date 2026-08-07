//! Embedded backend — extracts `llama-server` (+ shared libs) from kern's
//! own binary into the local cache, on first use. Only actually exists
//! when the build was made with `--features bundled-llama-server`
//! (CI release builds, see `.github/workflows/release.yml`); development
//! builds don't embed anything and depend only on the Ollama probe (see
//! `cmd_serve` in `main.rs`).
//!
//! The `.gguf` model itself is **not** embedded (model weights are too
//! large for the binary) — it's resolved separately from the local cache
//! (`~/.cache/kern/models/`). Automatic download via Hugging Face Hub is
//! not yet implemented (see sprint-status.md).

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
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve the home directory"))?;
    Ok(home
        .join(".cache")
        .join("kern")
        .join("bin")
        .join("llama-server"))
}

/// Extracts the embedded bundle to `cache_dir` if it's not there yet —
/// idempotent, repeated calls don't re-extract. Returns the path to the
/// `llama-server` executable, already made executable (unix).
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
            "extracting the embedded bundle did not produce {} — corrupted bundle or \
             unexpected layout",
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
        "this build does not embed llama-server (bundled-llama-server feature disabled \
         — only CI release builds enable it)"
    )
}

/// First `.gguf` found in `~/.cache/kern/models/`, alphabetical order —
/// v0 doesn't pick an "official" default model nor download anything
/// automatically (see module doc). Fails explicitly if the directory
/// doesn't exist or is empty; the caller decides the final error message.
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

    /// Actually extracts the embedded bundle (the test binary only exists
    /// when compiled with `--features bundled-llama-server` and a real CI
    /// release provided `KERN_LLAMA_SERVER_ARCHIVE` — does not run in the
    /// default development `cargo test --workspace`).
    #[test]
    fn ensure_extracted_at_produces_executable_binary_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("llama-server");

        let first = ensure_extracted_at(&cache_dir).expect("first extraction should work");
        assert!(first.exists());

        let modified_before = std::fs::metadata(&first).unwrap().modified().unwrap();
        let second = ensure_extracted_at(&cache_dir).expect("second call should be idempotent");
        let modified_after = std::fs::metadata(&second).unwrap().modified().unwrap();
        assert_eq!(modified_before, modified_after, "should not re-extract");
    }
}
