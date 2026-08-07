//! Embedded backend — extracts `llama-server` (+ shared libs) from kern's
//! own binary into the local cache, on first use. Only actually exists
//! when the build was made with `--features bundled-llama-server`
//! (CI release builds, see `.github/workflows/release.yml`); development
//! builds don't embed anything and depend only on the Ollama probe (see
//! `cmd_serve` in `main.rs`).
//!
//! The `.gguf` model itself is **not** embedded in the binary (weights are
//! too large for `include_bytes!`) — it's resolved from the filesystem, in
//! order: (1) `~/.cache/kern/models/`, the persistent cache
//! (`find_cached_model`); (2) a `.gguf` shipped as a *sidecar* file next to
//! the running executable — the shape of the `kern-<target>-with-embedding-model`
//! release tarball — adopted into the cache on first use (`resolve_model`).
//! Neither path performs a network call: there is still no automatic
//! download from Hugging Face Hub (or anywhere else) at runtime; the
//! with-embedding-model tarball is a build-time bundling decision, not
//! download-on-demand.

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

/// Shared home for the persistent model cache — `~/.cache/kern/models/`.
fn models_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cache").join("kern").join("models"))
}

/// First `.gguf` in `dir`, alphabetical order — no "official" default,
/// first wins. Shared by the persistent-cache lookup and the sidecar
/// lookup below.
fn first_gguf_in(dir: &std::path::Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("gguf"))
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

/// First `.gguf` found in `~/.cache/kern/models/`, alphabetical order —
/// v0 doesn't pick an "official" default model nor download anything
/// automatically (see module doc). Fails explicitly if the directory
/// doesn't exist or is empty; the caller decides the final error message.
pub fn find_cached_model() -> Option<PathBuf> {
    first_gguf_in(&models_dir()?)
}

/// A `.gguf` shipped as a *sidecar* file next to the running executable —
/// the shape of the `kern-<target>-with-embedding-model` release tarball
/// (binary + model file, extracted side by side). Uses `current_exe()`,
/// not a compile-time path — the binary is byte-identical between tarball
/// variants, only packaging differs. `Ok(None)` is a normal state (slim
/// tarball, dev build), not an error.
fn sidecar_model() -> anyhow::Result<Option<PathBuf>> {
    let exe = std::env::current_exe()?;
    Ok(exe.parent().and_then(first_gguf_in))
}

/// Copies `source` into `models_dir` if it isn't already there — idempotent,
/// mirrors `ensure_extracted_at`'s pattern for the llama-server binary.
/// Only ever called once a real sidecar file has already been found on
/// local disk: no network call, nothing fabricated.
fn adopt_into_model_cache(
    source: &std::path::Path,
    models_dir: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(models_dir)?;
    let file_name = source.file_name().ok_or_else(|| {
        anyhow::anyhow!("sidecar model path has no file name: {}", source.display())
    })?;
    let dest = models_dir.join(file_name);
    if !dest.exists() {
        std::fs::copy(source, &dest)?;
        tracing::info!(
            source = %source.display(),
            dest = %dest.display(),
            "adopted sidecar embedding model into ~/.cache/kern/models"
        );
    }
    Ok(dest)
}

/// Full model resolution order used by `spawn_embedded_embedder` (main.rs):
/// (1) `~/.cache/kern/models/` — unchanged, works today; (2) a sidecar
/// `.gguf` next to the running executable — the with-embedding-model
/// tarball's shape — adopted into the same cache so it behaves identically
/// to a manually-placed file from this point on, including on a later run
/// after the binary has moved away from its sidecar. `Ok(None)` only when
/// neither source has a model; the caller decides the final error message.
/// Real filesystem errors (e.g. an unwritable cache dir) surface as `Err`,
/// never silently collapsed into `None` — that would misreport a
/// permissions failure as "no model available".
pub fn resolve_model() -> anyhow::Result<Option<PathBuf>> {
    if let Some(cached) = find_cached_model() {
        return Ok(Some(cached));
    }
    match sidecar_model()? {
        Some(sidecar) => {
            let dir = models_dir()
                .ok_or_else(|| anyhow::anyhow!("could not resolve the home directory"))?;
            Ok(Some(adopt_into_model_cache(&sidecar, &dir)?))
        }
        None => Ok(None),
    }
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

// Not feature-gated — plain filesystem I/O, no dependency on the
// `include_bytes!`'d llama-server archive, so this runs under the default
// `cargo test --workspace`.
#[cfg(test)]
mod sidecar_model_tests {
    use super::*;

    #[test]
    fn first_gguf_in_picks_the_alphabetically_first_gguf_and_ignores_other_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("readme.txt"), b"not a model").unwrap();
        std::fs::write(tmp.path().join("zeta.gguf"), b"zeta").unwrap();
        std::fs::write(tmp.path().join("alpha.gguf"), b"alpha").unwrap();

        let found = first_gguf_in(tmp.path()).expect("should find a .gguf");
        assert_eq!(found.file_name().unwrap(), "alpha.gguf");
    }

    #[test]
    fn first_gguf_in_returns_none_for_an_empty_or_missing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(first_gguf_in(tmp.path()).is_none());
        assert!(first_gguf_in(&tmp.path().join("does-not-exist")).is_none());
    }

    #[test]
    fn adopt_into_model_cache_copies_the_file_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("sidecar");
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("model.gguf");
        std::fs::write(&source, b"fake model bytes").unwrap();

        let first = adopt_into_model_cache(&source, &cache_dir).expect("first copy should work");
        assert!(first.exists());
        assert_eq!(std::fs::read(&first).unwrap(), b"fake model bytes");

        // Idempotency: mutate the cached copy, then adopt again — a real
        // copy would clobber our mutation, so surviving it proves the
        // second call short-circuited on `dest.exists()`.
        std::fs::write(&first, b"mutated after first adopt").unwrap();
        let second =
            adopt_into_model_cache(&source, &cache_dir).expect("second call should be idempotent");
        assert_eq!(first, second);
        assert_eq!(
            std::fs::read(&second).unwrap(),
            b"mutated after first adopt"
        );
    }
}
