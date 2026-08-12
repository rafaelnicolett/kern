//! Per-project configuration — `<project>/.kern/config.toml`, written once
//! by the guided setup flow (or non-interactive flags) at `project create`
//! time, read by every subsequent `serve`/`status`. Pins the embedding
//! provider, model, and its real dimension for the project's lifetime —
//! switching providers on an existing project needs an explicit
//! reconfigure + re-index (see `kern_vector::VectorStoreError::DimensionMismatch`),
//! never silent.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub schema_version: u32,
    pub embedding: EmbeddingConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extraction: Option<ExtractionConfig>,
    /// `#[serde(default)]` so a project configured before this field
    /// existed loads fine and just gets the default — a missing
    /// `[indexing]` section is not the kind of ambiguity that deserves a
    /// hard error the way a missing embedding provider does, this is a
    /// pure performance knob with a safe default.
    #[serde(default)]
    pub indexing: IndexingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexingConfig {
    /// How many chunks `catch_up_scan` embeds/enriches concurrently.
    /// Bounded by what the configured model backend can actually sustain
    /// in parallel, not by kern itself — see the README's "Indexing
    /// throughput" section before raising this. Real local backends
    /// process the requests they can and queue the rest, so this is a
    /// "how many in flight at once" knob, not a hard guarantee of that
    /// much real parallelism.
    #[serde(default = "default_chunk_concurrency")]
    pub chunk_concurrency: usize,
}

/// Deliberately higher than the model backend's own real concurrency
/// ceiling (Ollama defaults to `OLLAMA_NUM_PARALLEL=4` real parallel
/// slots) — a client-side queue a bit deeper than the server can execute
/// at once keeps all of the server's slots busy instead of idling between
/// requests. This is NOT the knob that controls real backend parallelism;
/// see the README's "Indexing throughput" section for that one
/// (`OLLAMA_NUM_PARALLEL`, external to kern). Raising this without also
/// raising the backend's own concurrency mostly just deepens the queue.
fn default_chunk_concurrency() -> usize {
    8
}

impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            chunk_concurrency: default_chunk_concurrency(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// `"ollama"` or `"llama_cpp_embedded"`.
    pub provider: String,
    /// Ollama tag, or the bundled `.gguf` file name.
    pub model: String,
    /// Written once from a real `capabilities()` call — never hand-edited.
    pub dimension: i32,
    /// `llama_cpp_embedded` only — the runtime needs this to spawn
    /// `llama-server`; ignored for `"ollama"`, where Ollama owns its own
    /// serving parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionConfig {
    /// Only `"ollama"` is real today (see `kern_model::ExtractionProviderKind`).
    pub provider: String,
    pub model: String,
}

fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".kern").join("config.toml")
}

/// `Ok(None)` when no config exists yet — a project created before this
/// mechanism existed, or one whose setup was skipped. The caller decides
/// what to do (point at `kern config set-embedding`, fail clearly).
pub fn load(project_root: &Path) -> anyhow::Result<Option<ProjectConfig>> {
    let path = config_path(project_root);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(Some(toml::from_str(&content)?))
}

pub fn save(project_root: &Path, config: &ProjectConfig) -> anyhow::Result<()> {
    let path = config_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_none_when_no_config_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let config = ProjectConfig {
            schema_version: 1,
            embedding: EmbeddingConfig {
                provider: "ollama".to_string(),
                model: "all-minilm".to_string(),
                dimension: 384,
                context_size: None,
            },
            extraction: Some(ExtractionConfig {
                provider: "ollama".to_string(),
                model: "llama3.2".to_string(),
            }),
            indexing: IndexingConfig {
                chunk_concurrency: 8,
            },
        };

        save(dir.path(), &config).unwrap();
        let loaded = load(dir.path())
            .unwrap()
            .expect("config should exist after save");

        assert_eq!(loaded.embedding.model, "all-minilm");
        assert_eq!(loaded.embedding.dimension, 384);
        assert_eq!(loaded.extraction.unwrap().model, "llama3.2");
        assert_eq!(loaded.indexing.chunk_concurrency, 8);
    }

    #[test]
    fn indexing_defaults_when_the_config_file_predates_the_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A config.toml written before `[indexing]` existed — no such
        // section at all.
        std::fs::write(
            &path,
            "schema_version = 1\n\n[embedding]\nprovider = \"ollama\"\nmodel = \"all-minilm\"\ndimension = 384\n",
        )
        .unwrap();

        let loaded = load(dir.path()).unwrap().unwrap();
        assert_eq!(
            loaded.indexing.chunk_concurrency,
            default_chunk_concurrency(),
            "a pre-existing config without [indexing] should default, not fail to load"
        );
    }

    #[test]
    fn extraction_is_omitted_from_the_file_when_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let config = ProjectConfig {
            schema_version: 1,
            embedding: EmbeddingConfig {
                provider: "llama_cpp_embedded".to_string(),
                model: "all-MiniLM-L6-v2-ggml-model-f16.gguf".to_string(),
                dimension: 384,
                context_size: Some(512),
            },
            extraction: None,
            indexing: IndexingConfig::default(),
        };
        save(dir.path(), &config).unwrap();

        let raw = std::fs::read_to_string(config_path(dir.path())).unwrap();
        assert!(!raw.contains("[extraction]"));

        let loaded = load(dir.path()).unwrap().unwrap();
        assert!(loaded.extraction.is_none());
        assert_eq!(loaded.embedding.context_size, Some(512));
    }
}
