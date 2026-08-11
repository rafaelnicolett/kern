//! kern-cli — the final binary: `project create`, `serve`, `status`.
//!
//! Single runtime: the same binary watches, converts, extracts, indexes, and
//! serves. See the `ProcessState` state machine below.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use kern_ingest::{MarkdownChunker, StructuralMarkdownChunker};
use kern_mcp::KernServer;
use kern_model::{EmbeddingProvider, ExtractionProvider, LlamaCppRuntime, OllamaClient};
use kern_ontology::{
    AmbiguousZoneConfig, OntologyEngine, SqliteFrontmatterProfileRepository,
    SqliteInstanceRepository, SqliteTypeRepository, TypeRepository,
};
use kern_vector::{ChunkRecord, LanceVectorStore, VectorStore};
use tracing_subscriber::EnvFilter;

mod embedded;

/// Process states — strictly sequential transitions, no going back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessState {
    Starting,
    CatchUpScan,
    Ready,
    Draining,
    Stopped,
}

#[derive(Parser)]
#[command(name = "kern", version, about = "local RAG + incremental ontology")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manages isolated projects (folder + own state).
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Starts the process: CatchUpScan followed by the MCP stdio server.
    Serve {
        #[arg(long)]
        project: String,
    },
    /// Reports project health — works even without a `serve` running, by
    /// reading persisted state (v0 is single-client, with no multi-process
    /// coordination — in-memory metrics from a currently running `serve`
    /// session are not visible here, only what has already been persisted).
    Status {
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Creates an isolated project — name must be unique on the local machine.
    Create {
        name: String,
        #[arg(long)]
        path: PathBuf,
    },
}

fn registry_path() -> anyhow::Result<PathBuf> {
    // KERN_HOME overrides the home directory — used by integration tests
    // to isolate the global registry across parallel runs.
    if let Ok(override_home) = std::env::var("KERN_HOME") {
        return Ok(PathBuf::from(override_home).join("projects.json"));
    }
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve the home directory"))?;
    Ok(home.join(".kern").join("projects.json"))
}

fn load_registry() -> anyhow::Result<HashMap<String, PathBuf>> {
    let path = registry_path()?;
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&content).unwrap_or_default())
}

fn save_registry(registry: &HashMap<String, PathBuf>) -> anyhow::Result<()> {
    let path = registry_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(registry)?)?;
    Ok(())
}

async fn cmd_project_create(name: String, path: PathBuf) -> anyhow::Result<()> {
    let mut registry = load_registry()?;
    // Name must be unique within the local machine scope.
    if registry.contains_key(&name) {
        anyhow::bail!("AGENT_SURFACE.PROJECT_ALREADY_EXISTS: '{name}' already exists");
    }

    std::fs::create_dir_all(path.join(".kern"))?;
    let db_path = path.join(".kern").join("registry.db");

    // Each project gets its own TypeRegistry + InstanceGraph — never shared
    // across projects.
    let types = SqliteTypeRepository::open(&db_path)?;
    types.seed_canonical_vocabulary().await?;
    let _instances = SqliteInstanceRepository::open(&db_path)?;
    let _frontmatter = SqliteFrontmatterProfileRepository::open(&db_path)?;
    let _vectors = LanceVectorStore::open(&path.join(".kern").join("vectors")).await?;

    registry.insert(name.clone(), path.clone());
    save_registry(&registry)?;

    println!("project '{name}' created at {}", path.display());
    Ok(())
}

fn resolve_project(name: &str) -> anyhow::Result<PathBuf> {
    let registry = load_registry()?;
    registry
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("AGENT_SURFACE.PROJECT_NOT_FOUND: '{name}'"))
}

/// CatchUpScan — reuses the same hash-diff as the watcher to recover from
/// lag. Chunk+embed+index always happens. Ontology enrichment only happens when `ontology_engine` is
/// `Some` — it is `None` when Ollama is not available (the embedded
/// backend only covers embedding, see `spawn_embedded_embedder`). An
/// isolated enrichment failure on one file/chunk never aborts the vector
/// indexing of the rest — it's an enhancement, not a requirement for a
/// chunk to become searchable.
///
/// Two enrichment paths, not one:
/// - **Frontmatter** (`OntologyEngine::process_frontmatter`) runs once per
///   file, before the per-chunk loop, using the whole file's content and
///   the first chunk's id as evidence (`kern-ingest`'s chunker never
///   splits before the first heading, so a `---...---` block always ends
///   up in chunk 0). Deterministic: no distance, no `judge()` for this
///   file's own entity/relations.
/// - **Prose** (`OntologyEngine::process_chunk`) runs per chunk, via
///   `extract()` + distance + merge/new-type/judge. When a file has
///   frontmatter, chunk 0 is skipped for prose extraction — it's the raw
///   YAML block, not prose, and feeding it to an LLM produces noise (real
///   finding: candidates literally named "component", "string" — YAML
///   syntax words, not real entities). Every other chunk of the file
///   (the real body content) still goes through prose extraction as
///   normal.
async fn catch_up_scan(
    root: &Path,
    vector_store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    ontology_engine: Option<&OntologyEngine>,
) -> anyhow::Result<usize> {
    let chunker = StructuralMarkdownChunker;
    let mut indexed = 0usize;

    for entry in walk_markdown_files(root)? {
        let content = std::fs::read_to_string(&entry)?;
        let chunks = chunker.chunk(&entry, &content);
        let entry_path = entry.to_string_lossy().to_string();

        let mut skip_prose_on_first_chunk = false;
        if let Some(engine) = ontology_engine {
            if let Some(first_chunk) = chunks.first() {
                match engine
                    .process_frontmatter(&entry_path, &content, first_chunk.id)
                    .await
                {
                    Ok(Some(_outcome)) => skip_prose_on_first_chunk = true,
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            file = %entry_path,
                            "failed to process frontmatter for this file — vector indexing and prose enrichment are not affected"
                        );
                    }
                }
            }
        }

        for (i, chunk) in chunks.into_iter().enumerate() {
            let embedding = embedder.embed(&chunk.content).await?;
            let file_path = chunk.file_path.to_string_lossy().to_string();

            let is_frontmatter_chunk = i == 0 && skip_prose_on_first_chunk;
            if !is_frontmatter_chunk {
                if let Some(engine) = ontology_engine {
                    if let Err(e) = engine.process_chunk(&chunk.content, &file_path).await {
                        tracing::warn!(
                            error = %e,
                            file = %file_path,
                            "failed to enrich ontology for this chunk — vector indexing is not affected"
                        );
                    }
                }
            }

            vector_store
                .upsert(ChunkRecord {
                    id: chunk.id,
                    file_path,
                    content: chunk.content,
                    embedding,
                    content_hash: chunk.content_hash,
                    updated_at: now_timestamp(),
                })
                .await?;
            indexed += 1;
        }
    }
    Ok(indexed)
}

fn now_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Sorted lexicographically before returning — `read_dir`'s order is
/// filesystem-dependent, not alphabetical. Determinism matters beyond
/// reproducibility: frontmatter-driven relations resolve their target by
/// name (`OntologyEngine::resolve_or_create_placeholder_entity`), and a
/// numbered corpus (`TASK-0001`, `TASK-0002`, ...) processed in name order
/// hits far fewer forward references (a relation naming an id not ingested
/// yet) than an arbitrary filesystem order would — real behavior observed
/// while verifying this engine end to end, not a hypothetical.
fn walk_markdown_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()) == Some(".kern") {
                continue; // never reindex kern's own state
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Last resort when Ollama doesn't respond: extracts `llama-server` (only
/// exists in release builds with the `bundled-llama-server` feature) and
/// starts it against a `.gguf` already in the local cache. Never attempts
/// to download anything from the network — fails explicitly and
/// immediately if a piece is missing (see BDD: "First run without
/// internet fails clearly" — no silent fallback).
async fn spawn_embedded_embedder() -> anyhow::Result<LlamaCppRuntime> {
    let binary = embedded::ensure_llama_server_binary().map_err(|e| {
        anyhow::anyhow!("no model backend available: Ollama is not responding on :11434 and {e}")
    })?;
    let model = embedded::resolve_model()?.ok_or_else(|| {
        anyhow::anyhow!(
            "AGENT_SURFACE.MODEL_MISSING_FROM_CACHE: no .gguf found in \
             ~/.cache/kern/models, and no sidecar .gguf next to the running executable — \
             download the kern-<target>-with-embedding-model release tarball for a \
             zero-setup embedded model, or populate ~/.cache/kern/models by hand before \
             running without Ollama"
        )
    })?;
    let port = pick_free_port()?;
    LlamaCppRuntime::spawn(&binary, &model, port)
        .await
        .map_err(|e| anyhow::anyhow!("failed to start embedded backend (llama-server): {e}"))
}

fn pick_free_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

async fn cmd_serve(project: String) -> anyhow::Result<()> {
    let mut state = ProcessState::Starting;
    tracing::info!(?state, project, "starting");

    let root = resolve_project(&project)?;
    let db_path = root.join(".kern").join("registry.db");

    // A single probe decides both: Ollama serves as embedder and as
    // extractor (llama3.2). The embedded backend (LlamaCppRuntime) only
    // implements EmbeddingProvider — without Ollama, vector indexing keeps
    // working, but ontology enrichment is disabled for this session (see
    // catch_up_scan).
    let ollama_available = OllamaClient::new("all-minilm").probe().await;
    let embedder: Arc<dyn EmbeddingProvider> = if ollama_available {
        Arc::new(OllamaClient::new("all-minilm"))
    } else {
        Arc::new(spawn_embedded_embedder().await?)
    };
    let extraction: Option<Arc<dyn ExtractionProvider>> = if ollama_available {
        Some(Arc::new(OllamaClient::new("llama3.2")))
    } else {
        tracing::warn!(
            "Ollama unavailable — ontology enrichment (extract/judge) disabled for this \
             session; vector indexing continues normally"
        );
        None
    };

    let types: Arc<dyn TypeRepository> = Arc::new(SqliteTypeRepository::open(&db_path)?);
    let instances: Arc<dyn kern_ontology::InstanceRepository> =
        Arc::new(SqliteInstanceRepository::open(&db_path)?);
    let frontmatter_profiles: Arc<dyn kern_ontology::FrontmatterProfileRepository> =
        Arc::new(SqliteFrontmatterProfileRepository::open(&db_path)?);
    let vector_store: Arc<dyn VectorStore> =
        Arc::new(LanceVectorStore::open(&root.join(".kern").join("vectors")).await?);

    let ontology_engine = extraction.map(|extraction| {
        OntologyEngine::new(
            types.clone(),
            instances.clone(),
            extraction,
            frontmatter_profiles,
            embedder.clone(),
            AmbiguousZoneConfig::default(),
        )
    });

    state = ProcessState::CatchUpScan;
    tracing::info!(?state, "catch-up scan — indexing corpus");
    let indexed = catch_up_scan(
        &root,
        vector_store.as_ref(),
        embedder.as_ref(),
        ontology_engine.as_ref(),
    )
    .await?;
    tracing::info!(chunks_indexed = indexed, "catch-up complete");

    state = ProcessState::Ready;
    tracing::info!(?state, "ready — serving MCP via stdio");

    let server = KernServer::new(types, instances, vector_store, embedder);
    let service = rmcp::ServiceExt::serve(server, rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("error serving: {e:?}"))?;

    tokio::select! {
        result = service.waiting() => {
            result?;
        }
        _ = tokio::signal::ctrl_c() => {
            state = ProcessState::Draining;
            tracing::info!(?state, "signal received — shutting down");
        }
    }

    state = ProcessState::Stopped;
    tracing::info!(?state, "stopped");
    Ok(())
}

async fn cmd_status(project: Option<String>) -> anyhow::Result<()> {
    let registry = load_registry()?;

    let Some(name) = project else {
        if registry.is_empty() {
            println!("no project created yet — kern project create <name> --path <folder>");
        } else {
            println!("registered projects:");
            for (name, path) in &registry {
                println!("  {name} -> {}", path.display());
            }
        }
        return Ok(());
    };

    let root = resolve_project(&name)?;
    let db_path = root.join(".kern").join("registry.db");
    let types = SqliteTypeRepository::open(&db_path)?;
    let vector_store = LanceVectorStore::open(&root.join(".kern").join("vectors")).await?;

    let entity_types = types.list_entity_types().await?;
    let relation_types = types.list_relation_types().await?;
    let canonical = relation_types
        .iter()
        .filter(|t| t.status == kern_ontology::RelationTypeStatus::Canonical)
        .count();
    let chunk_count = vector_store.count().await?;

    println!("project: {name} ({})", root.display());
    println!("chunks indexed: {chunk_count}");
    println!(
        "entity types: {} | relation types: {} ({canonical} canonical)",
        entity_types.len(),
        relation_types.len()
    );
    println!(
        "note: fallback rate is an in-memory metric of a `serve` session — \
         not visible here across processes (v0 is single-client, with no multi-process coordination)"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr) // stdout is reserved for the MCP protocol
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Project {
            action: ProjectAction::Create { name, path },
        } => cmd_project_create(name, path).await,
        Command::Serve { project } => cmd_serve(project).await,
        Command::Status { project } => cmd_status(project).await,
    }
}
