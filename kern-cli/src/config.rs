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
        };

        save(dir.path(), &config).unwrap();
        let loaded = load(dir.path())
            .unwrap()
            .expect("config should exist after save");

        assert_eq!(loaded.embedding.model, "all-minilm");
        assert_eq!(loaded.embedding.dimension, 384);
        assert_eq!(loaded.extraction.unwrap().model, "llama3.2");
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
        };
        save(dir.path(), &config).unwrap();

        let raw = std::fs::read_to_string(config_path(dir.path())).unwrap();
        assert!(!raw.contains("[extraction]"));

        let loaded = load(dir.path()).unwrap().unwrap();
        assert!(loaded.extraction.is_none());
        assert_eq!(loaded.embedding.context_size, Some(512));
    }
}
