//! Guided (or non-interactive) provider setup — shared by `kern project
//! create` and `kern config set-embedding`/`set-extraction`. Never
//! persists a config that hasn't been proven to work: every path here
//! ends with a real round-trip against the chosen backend before
//! returning.

use std::io::IsTerminal;
use std::sync::Arc;

use kern_model::{
    build_embedding_provider, EmbeddingProvider, EmbeddingProviderSelection, ExtractionProvider,
    ExtractionProviderKind, OllamaClient,
};

use crate::config::{EmbeddingConfig, ExtractionConfig};
use crate::embedded;

/// Bundled `llama-server` is spawned with this context size — the
/// verified real architectural max of the embedding model kern actually
/// bundles (`all-MiniLM-L6-v2`, F16 GGUF: `bert.context_length` 512, see
/// NOTICE-THIRD-PARTY). If a different model is ever bundled, this needs
/// re-verifying the same way, not just bumping.
const BUNDLED_MODEL_CONTEXT_SIZE: u32 = 512;

pub enum EmbeddingChoice {
    Ollama { model: String },
    LlamaCppEmbedded,
}

pub enum ExtractionChoice {
    Ollama { model: String },
    Skip,
}

/// `choice` is `None` to trigger the interactive wizard — only actually
/// interactive if stdin is a real terminal; otherwise a clear error
/// rather than hanging on input that will never arrive.
pub async fn resolve_embedding_config(
    choice: Option<EmbeddingChoice>,
) -> anyhow::Result<EmbeddingConfig> {
    let choice = match choice {
        Some(c) => c,
        None if std::io::stdin().is_terminal() => prompt_for_embedding_choice().await?,
        None => anyhow::bail!(
            "AGENT_SURFACE.PROJECT_NOT_CONFIGURED: no --embedding-provider/--embedding-model \
             given and stdin isn't a terminal — pass both flags explicitly, or run this \
             interactively for the guided setup"
        ),
    };

    match choice {
        EmbeddingChoice::Ollama { model } => {
            let provider = OllamaClient::new(model.clone());
            let caps = provider.capabilities().await.map_err(|e| {
                anyhow::anyhow!(
                    "AGENT_SURFACE.PROVIDER_UNAVAILABLE: could not use ollama:{model} for \
                     embeddings — {e}"
                )
            })?;
            Ok(EmbeddingConfig {
                provider: "ollama".to_string(),
                model,
                dimension: caps.embedding_dim as i32,
                context_size: None,
            })
        }
        EmbeddingChoice::LlamaCppEmbedded => {
            let (model_path, caps) = spawn_embedded_and_probe().await?;
            let model_name = model_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| model_path.display().to_string());
            Ok(EmbeddingConfig {
                provider: "llama_cpp_embedded".to_string(),
                model: model_name,
                dimension: caps.embedding_dim as i32,
                context_size: Some(BUNDLED_MODEL_CONTEXT_SIZE),
            })
        }
    }
}

/// `Ok(None)` is a normal, valid outcome — extraction is optional, and
/// skipping it (explicitly, or by having no TTY and no flags) just means
/// ontology enrichment stays disabled for this project, same graceful
/// degradation `cmd_serve` already has today when Ollama is unavailable.
pub async fn resolve_extraction_config(
    choice: Option<ExtractionChoice>,
) -> anyhow::Result<Option<ExtractionConfig>> {
    let choice = match choice {
        Some(c) => c,
        None if std::io::stdin().is_terminal() => prompt_for_extraction_choice().await?,
        None => return Ok(None),
    };

    match choice {
        ExtractionChoice::Skip => Ok(None),
        ExtractionChoice::Ollama { model } => {
            let provider = kern_model::build_extraction_provider(
                ExtractionProviderKind::Ollama,
                model.clone(),
                None,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "AGENT_SURFACE.PROVIDER_UNAVAILABLE: could not use ollama:{model} for \
                     extraction — {e}"
                )
            })?;
            // Real proof it works: a tiny, cheap real call through the
            // actual trait method the ontology engine uses, not a
            // separate synthetic "ping".
            provider
                .interpret_frontmatter_schema(&["id".to_string()])
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "AGENT_SURFACE.PROVIDER_UNAVAILABLE: ollama:{model} did not respond to \
                         a real extraction call — {e}"
                    )
                })?;
            Ok(Some(ExtractionConfig {
                provider: "ollama".to_string(),
                model,
            }))
        }
    }
}

/// Resolves the persisted config back into real, ready-to-use providers —
/// used by `cmd_serve`. Deliberately no fallback: an unreachable
/// configured provider is a hard error, never a silent swap to a
/// differently-dimensioned backend (see kern-vector's dimension pinning).
pub async fn build_providers_from_config(
    config: &crate::config::ProjectConfig,
) -> anyhow::Result<(
    Arc<dyn EmbeddingProvider>,
    Option<Arc<dyn ExtractionProvider>>,
)> {
    let embedding_selection = match config.embedding.provider.as_str() {
        "ollama" => EmbeddingProviderSelection::Ollama {
            model: config.embedding.model.clone(),
            base_url: None,
        },
        "llama_cpp_embedded" => {
            let binary = embedded::ensure_llama_server_binary().map_err(|e| {
                anyhow::anyhow!(
                    "AGENT_SURFACE.PROVIDER_UNAVAILABLE: project is configured for the bundled \
                     embedded backend, but {e}"
                )
            })?;
            let model_path = embedded::resolve_model()?.ok_or_else(|| {
                anyhow::anyhow!(
                    "AGENT_SURFACE.PROVIDER_UNAVAILABLE: project is configured for the bundled \
                     embedded backend, but no .gguf found in ~/.cache/kern/models and no \
                     sidecar next to the running executable"
                )
            })?;
            let port = crate::pick_free_port()?;
            EmbeddingProviderSelection::LlamaCppEmbedded {
                binary_path: binary,
                model_path,
                port,
                context_size: config
                    .embedding
                    .context_size
                    .unwrap_or(BUNDLED_MODEL_CONTEXT_SIZE),
                // Deliberately NOT config.indexing.chunk_concurrency — the
                // obvious-looking choice (kern owns both ends of this
                // subprocess, so why not keep them equal?) was tried and
                // measured against the real bundled model: parallel_slots=4
                // was consistently ~8-9% SLOWER than 1, not faster, at both
                // small and large concurrent-call counts. This tiny
                // quantized model's individual requests are fast enough
                // that -np's scheduling overhead outweighs any real
                // parallelism gained — unlike Ollama's much larger
                // generative model, where more real slots measurably
                // helped. Default stays 1 (matches pre-fix behavior, zero
                // regression risk) until a model/workload is found where
                // raising it actually measures faster — see the README's
                // "Indexing throughput" section.
                parallel_slots: 1,
            }
        }
        other => anyhow::bail!(
            "AGENT_SURFACE.UNKNOWN_PROVIDER: '{other}' in .kern/config.toml is not a known \
             embedding provider"
        ),
    };
    let embedder = build_embedding_provider(embedding_selection)
        .await
        .map_err(|e| anyhow::anyhow!("AGENT_SURFACE.PROVIDER_UNAVAILABLE: {e}"))?;

    let extraction = match &config.extraction {
        Some(cfg) if cfg.provider == "ollama" => {
            let provider = kern_model::build_extraction_provider(
                ExtractionProviderKind::Ollama,
                cfg.model.clone(),
                None,
            )
            .await
            .map_err(|e| anyhow::anyhow!("AGENT_SURFACE.PROVIDER_UNAVAILABLE: {e}"))?;
            Some(provider)
        }
        Some(cfg) => anyhow::bail!(
            "AGENT_SURFACE.UNKNOWN_PROVIDER: '{}' in .kern/config.toml is not a known \
             extraction provider",
            cfg.provider
        ),
        None => None,
    };

    Ok((embedder, extraction))
}

async fn spawn_embedded_and_probe(
) -> anyhow::Result<(std::path::PathBuf, kern_model::EmbeddingCapabilities)> {
    let binary = embedded::ensure_llama_server_binary()
        .map_err(|e| anyhow::anyhow!("AGENT_SURFACE.PROVIDER_UNAVAILABLE: {e}"))?;
    let model_path = embedded::resolve_model()?.ok_or_else(|| {
        anyhow::anyhow!(
            "AGENT_SURFACE.MODEL_MISSING_FROM_CACHE: no .gguf found in ~/.cache/kern/models, \
             and no sidecar .gguf next to the running executable"
        )
    })?;
    let port = crate::pick_free_port()?;
    let selection = EmbeddingProviderSelection::LlamaCppEmbedded {
        binary_path: binary,
        model_path: model_path.clone(),
        port,
        context_size: BUNDLED_MODEL_CONTEXT_SIZE,
        // A one-off canary call during setup/capability-probing, before
        // any project config (and its chunk_concurrency) exists yet —
        // concurrency doesn't matter for a single request.
        parallel_slots: 1,
    };
    let provider = build_embedding_provider(selection)
        .await
        .map_err(|e| anyhow::anyhow!("AGENT_SURFACE.PROVIDER_UNAVAILABLE: {e}"))?;
    let caps = provider.capabilities().await?;
    Ok((model_path, caps))
}

async fn prompt_for_embedding_choice() -> anyhow::Result<EmbeddingChoice> {
    println!("Detecting local model backends...");

    let mut options: Vec<(String, EmbeddingChoice)> = Vec::new();

    let ollama = OllamaClient::new(String::new());
    if ollama.probe().await {
        if let Ok(models) = ollama.list_models().await {
            for m in models.iter().filter(|m| m.supports_embedding()) {
                let dim = m
                    .details
                    .embedding_length
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let ctx = m
                    .details
                    .context_length
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".to_string());
                options.push((
                    format!("ollama: {} (embedding, {dim}-dim, ctx {ctx})", m.name),
                    EmbeddingChoice::Ollama {
                        model: m.name.clone(),
                    },
                ));
            }
        }
    }

    let embedded_available =
        embedded::ensure_llama_server_binary().is_ok() && embedded::resolve_model()?.is_some();
    if embedded_available {
        options.push((
            "bundled llama-server (embedding only)".to_string(),
            EmbeddingChoice::LlamaCppEmbedded,
        ));
    }

    if options.is_empty() {
        anyhow::bail!(
            "AGENT_SURFACE.PROVIDER_UNAVAILABLE: no local embedding backend found — run \
             `ollama pull all-minilm`, or download the kern-<target>-with-embedding-model \
             release tarball for the bundled path"
        );
    }

    prompt_pick(&options, "embedding provider")
}

async fn prompt_for_extraction_choice() -> anyhow::Result<ExtractionChoice> {
    let ollama = OllamaClient::new(String::new());
    if !ollama.probe().await {
        println!(
            "Ollama not detected — skipping ontology extraction setup (embeddings still work)."
        );
        return Ok(ExtractionChoice::Skip);
    }
    let models = ollama.list_models().await.unwrap_or_default();
    let mut options: Vec<(String, ExtractionChoice)> = models
        .iter()
        .filter(|m| m.supports_completion())
        .map(|m| {
            (
                format!("ollama: {} (extraction/judging)", m.name),
                ExtractionChoice::Ollama {
                    model: m.name.clone(),
                },
            )
        })
        .collect();

    if options.is_empty() {
        println!(
            "No generative Ollama model found — skipping ontology extraction setup (embeddings \
             still work). Pull one (e.g. `ollama pull llama3.2`) and re-run `kern config \
             set-extraction` later if you want it."
        );
        return Ok(ExtractionChoice::Skip);
    }
    options.push((
        "skip extraction for this project".to_string(),
        ExtractionChoice::Skip,
    ));

    prompt_pick(&options, "extraction/judging model")
}

fn prompt_pick<T>(options: &[(String, T)], what: &str) -> anyhow::Result<T>
where
    T: Clone,
{
    for (i, (label, _)) in options.iter().enumerate() {
        println!("  [{}] {label}", i + 1);
    }
    print!("Pick a {what} [1]: ");
    std::io::Write::flush(&mut std::io::stdout()).ok();

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    let idx = if trimmed.is_empty() {
        0
    } else {
        trimmed
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("invalid choice: '{trimmed}'"))?
            .checked_sub(1)
            .ok_or_else(|| anyhow::anyhow!("choice must be 1 or higher"))?
    };
    options
        .get(idx)
        .map(|(_, choice)| choice.clone())
        .ok_or_else(|| anyhow::anyhow!("choice out of range"))
}

impl Clone for EmbeddingChoice {
    fn clone(&self) -> Self {
        match self {
            EmbeddingChoice::Ollama { model } => EmbeddingChoice::Ollama {
                model: model.clone(),
            },
            EmbeddingChoice::LlamaCppEmbedded => EmbeddingChoice::LlamaCppEmbedded,
        }
    }
}

impl Clone for ExtractionChoice {
    fn clone(&self) -> Self {
        match self {
            ExtractionChoice::Ollama { model } => ExtractionChoice::Ollama {
                model: model.clone(),
            },
            ExtractionChoice::Skip => ExtractionChoice::Skip,
        }
    }
}
