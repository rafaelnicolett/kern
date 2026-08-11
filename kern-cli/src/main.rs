//! kern-cli — the final binary: `project create`, `serve`, `status`.
//!
//! Single runtime: the same binary watches, converts, extracts, indexes, and
//! serves. See the `ProcessState` state machine below.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use kern_ingest::{
    BudgetAwareMarkdownChunker, HeuristicTokenCounter, MarkdownChunker, StructuralMarkdownChunker,
};
use kern_mcp::KernServer;
use kern_model::{EmbeddingProvider, ExtractionProvider};
use kern_ontology::{
    AmbiguousZoneConfig, OntologyEngine, SqliteFrontmatterProfileRepository,
    SqliteInstanceRepository, SqliteTypeRepository, TypeRepository,
};
use kern_vector::{ChunkRecord, LanceVectorStore, VectorStore};
use tracing_subscriber::EnvFilter;

mod config;
mod embedded;
mod setup;

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
    /// Reads or changes a project's model provider configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Creates an isolated project — name must be unique on the local
    /// machine — and resolves its model provider configuration in the
    /// same step. Interactively (a real terminal, no provider flags
    /// given): a guided setup wizard. Non-interactively (flags given, or
    /// no TTY): `--embedding-provider`/`--embedding-model` are required.
    Create {
        name: String,
        #[arg(long)]
        path: PathBuf,
        /// "ollama" or "llama_cpp_embedded" — skips the interactive
        /// embedding step when given together with `--embedding-model`.
        #[arg(long)]
        embedding_provider: Option<String>,
        /// Required alongside `--embedding-provider` for a uniform CLI
        /// shape, but for "llama_cpp_embedded" the value isn't used to
        /// pick among multiple cached models today — the resolver always
        /// takes the first `.gguf` found (alphabetically) in
        /// `~/.cache/kern/models`/next to the running executable. Matters
        /// for "ollama", where it's the real model tag.
        #[arg(long)]
        embedding_model: Option<String>,
        /// Only "ollama" is real today. Omit both this and
        /// `--extraction-model` to skip extraction/judging for this
        /// project non-interactively (it's optional).
        #[arg(long)]
        extraction_provider: Option<String>,
        #[arg(long)]
        extraction_model: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Prints the project's persisted provider configuration.
    Show {
        #[arg(long)]
        project: String,
    },
    /// Reconfigures an existing project's embedding provider. If the new
    /// provider reports a different dimension than what the project was
    /// already indexed with, this fails clearly rather than corrupting
    /// the index — delete `.kern/vectors` first to actually switch.
    SetEmbedding {
        #[arg(long)]
        project: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
    },
    /// Reconfigures (or removes, with no flags) an existing project's
    /// extraction/judging provider.
    SetExtraction {
        #[arg(long)]
        project: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
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

#[allow(clippy::too_many_arguments)]
async fn cmd_project_create(
    name: String,
    path: PathBuf,
    embedding_provider: Option<String>,
    embedding_model: Option<String>,
    extraction_provider: Option<String>,
    extraction_model: Option<String>,
) -> anyhow::Result<()> {
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

    let embedding_choice = match (embedding_provider, embedding_model) {
        (Some(provider), Some(model)) => Some(parse_embedding_choice(&provider, model)?),
        (None, None) => None,
        _ => anyhow::bail!(
            "AGENT_SURFACE.INCOMPLETE_PROVIDER_FLAGS: pass both --embedding-provider and \
             --embedding-model together, or neither for the guided setup"
        ),
    };
    let embedding = setup::resolve_embedding_config(embedding_choice).await?;

    let extraction_choice = match (extraction_provider, extraction_model) {
        (Some(provider), Some(model)) => Some(parse_extraction_choice(&provider, model)?),
        (None, None) if !std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
            Some(setup::ExtractionChoice::Skip)
        }
        (None, None) => None, // interactive — the wizard asks
        _ => anyhow::bail!(
            "AGENT_SURFACE.INCOMPLETE_PROVIDER_FLAGS: pass both --extraction-provider and \
             --extraction-model together, or neither to skip extraction"
        ),
    };
    let extraction = setup::resolve_extraction_config(extraction_choice).await?;

    let mut vectors = LanceVectorStore::open(&path.join(".kern").join("vectors")).await?;
    vectors.ensure_table(embedding.dimension).await?;

    config::save(
        &path,
        &config::ProjectConfig {
            schema_version: 1,
            embedding,
            extraction,
        },
    )?;

    registry.insert(name.clone(), path.clone());
    save_registry(&registry)?;

    println!("project '{name}' created at {}", path.display());
    Ok(())
}

fn parse_embedding_choice(provider: &str, model: String) -> anyhow::Result<setup::EmbeddingChoice> {
    match provider {
        "ollama" => Ok(setup::EmbeddingChoice::Ollama { model }),
        "llama_cpp_embedded" => Ok(setup::EmbeddingChoice::LlamaCppEmbedded),
        other => anyhow::bail!(
            "AGENT_SURFACE.UNKNOWN_PROVIDER: '{other}' — expected \"ollama\" or \
             \"llama_cpp_embedded\""
        ),
    }
}

fn parse_extraction_choice(
    provider: &str,
    model: String,
) -> anyhow::Result<setup::ExtractionChoice> {
    match provider {
        "ollama" => Ok(setup::ExtractionChoice::Ollama { model }),
        other => {
            anyhow::bail!("AGENT_SURFACE.UNKNOWN_PROVIDER: '{other}' — expected \"ollama\"")
        }
    }
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
/// `Some` — it is `None` when no extraction provider is configured for
/// this project. An isolated enrichment failure on one file/chunk never
/// aborts the vector indexing of the rest — it's an enhancement, not a
/// requirement for a chunk to become searchable.
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
///
/// `chunker` is budget-aware, sourced from the active embedding provider's
/// real `capabilities()` at the call site — never a hardcoded assumption
/// about how much any given backend can accept in one call.
async fn catch_up_scan(
    root: &Path,
    vector_store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    ontology_engine: Option<&OntologyEngine>,
    chunker: &dyn MarkdownChunker,
) -> anyhow::Result<usize> {
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

pub(crate) fn pick_free_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

async fn cmd_serve(project: String) -> anyhow::Result<()> {
    let mut state = ProcessState::Starting;
    tracing::info!(?state, project, "starting");

    let root = resolve_project(&project)?;
    let db_path = root.join(".kern").join("registry.db");

    let project_config = config::load(&root)?.ok_or_else(|| {
        anyhow::anyhow!(
            "AGENT_SURFACE.PROJECT_NOT_CONFIGURED: no .kern/config.toml for '{project}' — run \
             `kern config set-embedding --project {project} --provider <ollama|llama_cpp_embedded> \
             --model <name>` (this project predates provider configuration)"
        )
    })?;
    // No silent runtime fallback if the configured provider is
    // unreachable — a deliberate change from the old hardcoded
    // probe-then-fallback behavior. Once pinned (at `project create` or
    // `kern config set-embedding` time), an unreachable provider is a
    // hard error, never a silent swap to a differently-dimensioned
    // backend (see kern-vector's dimension pinning).
    let (embedder, extraction): (
        Arc<dyn EmbeddingProvider>,
        Option<Arc<dyn ExtractionProvider>>,
    ) = setup::build_providers_from_config(&project_config).await?;
    if extraction.is_none() {
        tracing::info!(
            "no extraction provider configured for this project — ontology enrichment \
             disabled for this session; vector indexing continues normally"
        );
    }

    let caps = embedder.capabilities().await?;
    let budget = caps.max_input_tokens.unwrap_or(256);
    let chunker = BudgetAwareMarkdownChunker::new(
        StructuralMarkdownChunker,
        Box::new(HeuristicTokenCounter),
        budget,
    );

    let types: Arc<dyn TypeRepository> = Arc::new(SqliteTypeRepository::open(&db_path)?);
    let instances: Arc<dyn kern_ontology::InstanceRepository> =
        Arc::new(SqliteInstanceRepository::open(&db_path)?);
    let frontmatter_profiles: Arc<dyn kern_ontology::FrontmatterProfileRepository> =
        Arc::new(SqliteFrontmatterProfileRepository::open(&db_path)?);
    let mut lance_store = LanceVectorStore::open(&root.join(".kern").join("vectors")).await?;
    lance_store
        .ensure_table(project_config.embedding.dimension)
        .await?;
    let vector_store: Arc<dyn VectorStore> = Arc::new(lance_store);

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
        &chunker,
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
    // Read-only: `open` already populates the table if it exists on disk —
    // no `ensure_table` call needed (that's only for creating one), and no
    // dimension has to be guessed just to report status.
    let vector_store = LanceVectorStore::open(&root.join(".kern").join("vectors")).await?;

    let entity_types = types.list_entity_types().await?;
    let relation_types = types.list_relation_types().await?;
    let canonical = relation_types
        .iter()
        .filter(|t| t.status == kern_ontology::RelationTypeStatus::Canonical)
        .count();
    let chunk_count = match vector_store.existing_dimension().await? {
        Some(_) => vector_store.count().await?,
        None => 0,
    };

    println!("project: {name} ({})", root.display());
    match config::load(&root)? {
        Some(cfg) => {
            println!(
                "embedding: {} via {} ({}-dim)",
                cfg.embedding.model, cfg.embedding.provider, cfg.embedding.dimension
            );
            match &cfg.extraction {
                Some(ext) => println!("extraction: {} via {}", ext.model, ext.provider),
                None => println!("extraction: not configured"),
            }
        }
        None => println!(
            "provider: not configured — run `kern config set-embedding --project {name} \
             --provider <ollama|llama_cpp_embedded> --model <name>`"
        ),
    }
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

fn cmd_config_show(project: String) -> anyhow::Result<()> {
    let root = resolve_project(&project)?;
    match config::load(&root)? {
        Some(cfg) => {
            println!("embedding.provider = {}", cfg.embedding.provider);
            println!("embedding.model = {}", cfg.embedding.model);
            println!("embedding.dimension = {}", cfg.embedding.dimension);
            if let Some(ctx) = cfg.embedding.context_size {
                println!("embedding.context_size = {ctx}");
            }
            match &cfg.extraction {
                Some(ext) => {
                    println!("extraction.provider = {}", ext.provider);
                    println!("extraction.model = {}", ext.model);
                }
                None => println!("extraction = not configured"),
            }
        }
        None => println!(
            "'{project}' has no .kern/config.toml yet — run `kern config set-embedding \
             --project {project} --provider <ollama|llama_cpp_embedded> --model <name>`"
        ),
    }
    Ok(())
}

/// Reconfiguring an existing project: the new provider's real dimension is
/// checked against whatever's already indexed via the same
/// `ensure_table`/`DimensionMismatch` path `cmd_serve` uses — switching to
/// a differently-dimensioned model fails clearly here too, it does not
/// silently corrupt the existing index.
async fn cmd_config_set_embedding(
    project: String,
    provider: String,
    model: String,
) -> anyhow::Result<()> {
    let root = resolve_project(&project)?;
    let choice = parse_embedding_choice(&provider, model)?;
    let embedding = setup::resolve_embedding_config(Some(choice)).await?;

    let mut vectors = LanceVectorStore::open(&root.join(".kern").join("vectors")).await?;
    vectors.ensure_table(embedding.dimension).await?;

    let mut cfg = config::load(&root)?.unwrap_or(config::ProjectConfig {
        schema_version: 1,
        embedding: embedding.clone(),
        extraction: None,
    });
    cfg.embedding = embedding;
    config::save(&root, &cfg)?;

    println!("'{project}' embedding provider updated");
    Ok(())
}

async fn cmd_config_set_extraction(
    project: String,
    provider: Option<String>,
    model: Option<String>,
) -> anyhow::Result<()> {
    let root = resolve_project(&project)?;
    let choice = match (provider, model) {
        (Some(provider), Some(model)) => Some(parse_extraction_choice(&provider, model)?),
        (None, None) => Some(setup::ExtractionChoice::Skip),
        _ => anyhow::bail!(
            "AGENT_SURFACE.INCOMPLETE_PROVIDER_FLAGS: pass both --provider and --model together, \
             or neither to remove extraction"
        ),
    };
    let extraction = setup::resolve_extraction_config(choice).await?;

    let mut cfg = config::load(&root)?.ok_or_else(|| {
        anyhow::anyhow!(
            "AGENT_SURFACE.PROJECT_NOT_CONFIGURED: '{project}' has no embedding provider \
             configured yet — run `kern config set-embedding` first"
        )
    })?;
    cfg.extraction = extraction;
    config::save(&root, &cfg)?;

    println!("'{project}' extraction provider updated");
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
            action:
                ProjectAction::Create {
                    name,
                    path,
                    embedding_provider,
                    embedding_model,
                    extraction_provider,
                    extraction_model,
                },
        } => {
            cmd_project_create(
                name,
                path,
                embedding_provider,
                embedding_model,
                extraction_provider,
                extraction_model,
            )
            .await
        }
        Command::Serve { project } => cmd_serve(project).await,
        Command::Status { project } => cmd_status(project).await,
        Command::Config {
            action: ConfigAction::Show { project },
        } => cmd_config_show(project),
        Command::Config {
            action:
                ConfigAction::SetEmbedding {
                    project,
                    provider,
                    model,
                },
        } => cmd_config_set_embedding(project, provider, model).await,
        Command::Config {
            action:
                ConfigAction::SetExtraction {
                    project,
                    provider,
                    model,
                },
        } => cmd_config_set_extraction(project, provider, model).await,
    }
}
