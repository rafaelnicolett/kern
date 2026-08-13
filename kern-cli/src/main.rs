//! kern-cli — the final binary: `project create`, `serve`, `status`.
//!
//! Single runtime: the same binary watches, converts, extracts, indexes, and
//! serves. See the `ProcessState` state machine below.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use kern_ingest::{
    is_real_change, BudgetAwareMarkdownChunker, HeuristicTokenCounter, MarkdownChunker,
    StructuralMarkdownChunker,
};
use kern_mcp::KernServer;
use kern_model::{EmbeddingProvider, ExtractionProvider};
use kern_ontology::{
    AmbiguousZoneConfig, IndexedFileRepository, OntologyEngine, SqliteFrontmatterProfileRepository,
    SqliteIndexedFileRepository, SqliteInstanceRepository, SqliteTypeRepository, TypeRepository,
};
use kern_vector::{ChunkRecord, LanceVectorStore, VectorStore};
use tracing_subscriber::EnvFilter;

use futures::stream::{self, TryStreamExt};

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
    let _indexed_files = SqliteIndexedFileRepository::open(&db_path)?;

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
            indexing: config::IndexingConfig::default(),
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
/// One chunk queued for the concurrent phase of `catch_up_scan`, after
/// frontmatter ingestion (Phase 1, serial) has already run for its file.
struct PendingChunk {
    chunk: kern_ingest::Chunk,
    file_path: String,
    is_frontmatter_chunk: bool,
}

/// `indexed`/`skipped` as before (chunks embedded+stored / rejected by the
/// backend and left out). `unchanged_files` is the whole point of this
/// struct existing — on a warm restart with nothing edited, this should
/// equal the corpus's file count and `indexed` should be 0. `changed_files`
/// covers both genuinely new files and edited ones; `deleted_files` is
/// files that were indexed before but are no longer on disk.
struct CatchUpSummary {
    indexed: usize,
    skipped: usize,
    unchanged_files: usize,
    changed_files: usize,
    deleted_files: usize,
}

/// Decrements `file_path`'s remaining-chunk counter; if this was the last
/// one, persists its new hash so a future scan correctly treats it as
/// unchanged. Called from both the normal completion path and the
/// `RequestRejected`-skip path in Phase 2 below — a file with any skipped
/// chunk still needs to reach this, or it would be reprocessed forever.
async fn mark_file_complete_if_done(
    file_path: &str,
    file_remaining: &HashMap<String, Arc<std::sync::atomic::AtomicUsize>>,
    file_new_hash: &HashMap<String, String>,
    indexed_files: &dyn IndexedFileRepository,
) -> anyhow::Result<()> {
    let Some(counter) = file_remaining.get(file_path) else {
        return Ok(());
    };
    // fetch_sub returns the value BEFORE decrementing — 1 means this was
    // the last remaining chunk for this file.
    if counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
        if let Some(hash) = file_new_hash.get(file_path) {
            indexed_files.mark_indexed(file_path, hash).await?;
        }
    }
    Ok(())
}

async fn catch_up_scan(
    root: &Path,
    vector_store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    ontology_engine: Option<&OntologyEngine>,
    chunker: &dyn MarkdownChunker,
    // Persisted "what was already indexed" state — see
    // `kern_ontology::IndexedFileRepository`. Diffed against the corpus on
    // disk below so unchanged files skip chunking/embedding/ontology
    // entirely instead of the whole corpus being reprocessed on every
    // `kern serve` launch (real bug reported against a 3710-file
    // production corpus: a full cold reindex took ~15-20 minutes, every
    // single time the process restarted). Ontology entities/relations
    // from a changed file's OLD content, or from a deleted file, are
    // deliberately NOT cleaned up in this version — see the README's
    // "Indexing throughput" section for why that's a known, explicit gap
    // rather than an oversight.
    indexed_files: &dyn IndexedFileRepository,
    // How many chunks are embedded/enriched concurrently — from
    // `.kern/config.toml`'s `[indexing] chunk_concurrency`
    // (`config::IndexingConfig`), not a hardcoded constant, so a user can
    // tune it for their own hardware/backend without a code change. See
    // the README's "Indexing throughput" section.
    chunk_concurrency: usize,
) -> anyhow::Result<CatchUpSummary> {
    let known_hashes: HashMap<PathBuf, String> = indexed_files
        .all()
        .await?
        .into_iter()
        .map(|(path, hash)| (PathBuf::from(path), hash))
        .collect();
    let current_files = walk_markdown_files(root)?;
    let current_set: HashSet<&PathBuf> = current_files.iter().collect();

    // Phase 0 — deletion detection: a file that was indexed before but no
    // longer exists on disk. Vector cleanup is exact — chunks are
    // exclusively owned by their file, `delete_by_file` already existed
    // for exactly this, just never called anywhere. Ontology cleanup is
    // NOT attempted (same documented v1 gap as changed files below).
    let mut deleted_files = 0usize;
    for path in known_hashes.keys() {
        if !current_set.contains(path) {
            let file_path = path.to_string_lossy().to_string();
            vector_store.delete_by_file(&file_path).await?;
            indexed_files.remove(&file_path).await?;
            deleted_files += 1;
        }
    }

    // Phase 1 — serial, cheap: for every file currently on disk, check
    // whether its content actually changed since the last successful
    // index (a blake3 hash comparison, not a mtime check — `is_real_change`)
    // BEFORE paying for chunking, embedding, or ontology enrichment. This
    // is the entire point of this feature: an unchanged file now costs
    // one read + one hash compare instead of a full reprocess. Kept
    // serial for the same reason as before: `walk_markdown_files`'s
    // sorted order still matters for forward-reference resolution
    // quality when files DO get reprocessed, and hashing text files
    // locally is fast enough (blake3, GB/s) that this phase was never the
    // bottleneck the concurrency in Phase 2 targets.
    let mut pending = Vec::new();
    let mut file_remaining: HashMap<String, Arc<std::sync::atomic::AtomicUsize>> = HashMap::new();
    let mut file_new_hash: HashMap<String, String> = HashMap::new();
    let mut unchanged_files = 0usize;
    let mut changed_files = 0usize;

    for entry in &current_files {
        let content = std::fs::read_to_string(entry)?;
        let entry_path = entry.to_string_lossy().to_string();

        let Some(new_hash) = is_real_change(entry, &content, &known_hashes) else {
            unchanged_files += 1;
            continue;
        };
        changed_files += 1;

        // Already indexed under a different hash — clear its stale chunks
        // before the new ones land, so a reindex never leaves duplicate
        // or orphaned chunk rows behind.
        if known_hashes.contains_key(entry) {
            vector_store.delete_by_file(&entry_path).await?;
        }

        let chunks = chunker.chunk(entry, &content);

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

        if chunks.is_empty() {
            // Nothing to embed for this file — mark it indexed right
            // away, there's nothing to wait for in Phase 2.
            indexed_files.mark_indexed(&entry_path, &new_hash).await?;
            continue;
        }

        file_remaining.insert(
            entry_path.clone(),
            Arc::new(std::sync::atomic::AtomicUsize::new(chunks.len())),
        );
        file_new_hash.insert(entry_path, new_hash);

        for (i, chunk) in chunks.into_iter().enumerate() {
            let file_path = chunk.file_path.to_string_lossy().to_string();
            pending.push(PendingChunk {
                chunk,
                file_path,
                is_frontmatter_chunk: i == 0 && skip_prose_on_first_chunk,
            });
        }
    }

    if ontology_engine.is_some() && (changed_files > 0 || deleted_files > 0) {
        tracing::warn!(
            changed_files,
            deleted_files,
            "ontology entities/relations from changed files' previous content, or from deleted \
             files, are not removed by this scan — additive-only until a future ontology \
             garbage-collection feature; see the README's \"Indexing throughput\" section"
        );
    }

    // Phase 2 — concurrent, expensive: embedding and ontology enrichment
    // are network round-trips to the model backend — the real cost of
    // indexing. `try_for_each_concurrent` overlaps up to `chunk_concurrency`
    // of them at once instead of paying each one's latency back to back,
    // and still fails fast (stops launching new work, propagates the
    // error) on a genuine backend failure, matching the original serial
    // loop's behavior. A `process_chunk` (ontology) failure is still
    // logged and skipped, not fatal — vector indexing for that chunk
    // still completes.
    //
    // Real bug found empirically on a 3710-file real corpus: a code
    // block/table `kern-ingest`'s chunker correctly kept whole (never
    // splitting one is a deliberate invariant — see
    // BudgetAwareMarkdownChunker) can still be too large for the
    // embedding backend's context window, which then rejects it with a
    // non-2xx response. Treating that the same as "the backend is down"
    // (the old, single ModelError::BackendUnavailable bucket) meant ONE
    // oversized chunk out of thousands aborted the entire indexing run,
    // losing all progress — the whole corpus, not just that one chunk.
    // `ModelError::RequestRejected` distinguishes "backend rejected this
    // specific input" from a real connectivity failure — only the latter
    // still aborts the run; the former is logged and skipped so the rest
    // of a real, messy corpus still indexes. A skipped chunk's file still
    // gets marked indexed (see `mark_file_complete_if_done`) — reprocessing
    // identical bytes next launch would just reject it again.
    let indexed = std::sync::atomic::AtomicUsize::new(0);
    let skipped = std::sync::atomic::AtomicUsize::new(0);
    stream::iter(pending.into_iter().map(Ok::<_, anyhow::Error>))
        .try_for_each_concurrent(Some(chunk_concurrency), |item| {
            let indexed = &indexed;
            let skipped = &skipped;
            let file_remaining = &file_remaining;
            let file_new_hash = &file_new_hash;
            async move {
                let embedding = match embedder.embed(&item.chunk.content).await {
                    Ok(embedding) => embedding,
                    Err(kern_model::ModelError::RequestRejected { status, body }) => {
                        skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::warn!(
                            file = %item.file_path,
                            chunk_id = %item.chunk.id,
                            status,
                            body = %body.chars().take(300).collect::<String>(),
                            "embedding backend rejected this chunk — likely a code block or \
                             table too large for its context window, kept whole rather than \
                             split; skipping this chunk, the rest of the corpus still indexes"
                        );
                        mark_file_complete_if_done(
                            &item.file_path,
                            file_remaining,
                            file_new_hash,
                            indexed_files,
                        )
                        .await?;
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                };

                if !item.is_frontmatter_chunk {
                    if let Some(engine) = ontology_engine {
                        if let Err(e) = engine
                            .process_chunk(&item.chunk.content, &item.file_path)
                            .await
                        {
                            tracing::warn!(
                                error = %e,
                                file = %item.file_path,
                                "failed to enrich ontology for this chunk — vector indexing is not affected"
                            );
                        }
                    }
                }

                vector_store
                    .upsert(ChunkRecord {
                        id: item.chunk.id,
                        file_path: item.file_path.clone(),
                        content: item.chunk.content,
                        embedding,
                        content_hash: item.chunk.content_hash,
                        updated_at: now_timestamp(),
                    })
                    .await?;
                indexed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                mark_file_complete_if_done(
                    &item.file_path,
                    file_remaining,
                    file_new_hash,
                    indexed_files,
                )
                .await?;
                Ok(())
            }
        })
        .await?;

    Ok(CatchUpSummary {
        indexed: indexed.load(std::sync::atomic::Ordering::Relaxed),
        skipped: skipped.load(std::sync::atomic::Ordering::Relaxed),
        unchanged_files,
        changed_files,
        deleted_files,
    })
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
    let indexed_files: Arc<dyn IndexedFileRepository> =
        Arc::new(SqliteIndexedFileRepository::open(&db_path)?);
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
    let summary = catch_up_scan(
        &root,
        vector_store.as_ref(),
        embedder.as_ref(),
        ontology_engine.as_ref(),
        &chunker,
        indexed_files.as_ref(),
        project_config.indexing.chunk_concurrency,
    )
    .await?;
    if summary.skipped > 0 {
        tracing::warn!(
            chunks_skipped = summary.skipped,
            "some chunks were rejected by the embedding backend and left out of the index — \
             see the individual warnings above for which files/chunks and why"
        );
    }
    tracing::info!(
        chunks_indexed = summary.indexed,
        chunks_skipped = summary.skipped,
        unchanged_files = summary.unchanged_files,
        changed_files = summary.changed_files,
        deleted_files = summary.deleted_files,
        "catch-up complete"
    );

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
            println!(
                "indexing.chunk_concurrency = {} (edit .kern/config.toml to change — see \
                 README's \"Indexing throughput\" section)",
                cfg.indexing.chunk_concurrency
            );
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
        indexing: config::IndexingConfig::default(),
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

#[cfg(test)]
mod catch_up_scan_tests {
    use super::*;

    /// Fails on exactly the input strings registered via `fail_for`, with
    /// whichever `ModelError` variant is configured — otherwise returns a
    /// fixed embedding. Real, in-process, no network — this is what lets
    /// these tests assert on exact embed() call counts.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FailureMode {
        RequestRejected,
        BackendUnavailable,
    }

    struct CountingEmbedder {
        calls: std::sync::atomic::AtomicUsize,
        dim: usize,
        fail_for: std::sync::Mutex<HashMap<String, FailureMode>>,
    }

    impl CountingEmbedder {
        fn new(dim: usize) -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
                dim,
                fail_for: std::sync::Mutex::new(HashMap::new()),
            }
        }

        fn fail(&self, content: &str, mode: FailureMode) {
            self.fail_for
                .lock()
                .unwrap()
                .insert(content.to_string(), mode);
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for CountingEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, kern_model::ModelError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(mode) = self.fail_for.lock().unwrap().get(text) {
                return match mode {
                    FailureMode::RequestRejected => Err(kern_model::ModelError::RequestRejected {
                        status: 500,
                        body: "input too large to process".to_string(),
                    }),
                    FailureMode::BackendUnavailable => {
                        Err(kern_model::ModelError::BackendUnavailable(
                            "connection refused".to_string(),
                        ))
                    }
                };
            }
            Ok(vec![0.5; self.dim])
        }

        async fn capabilities(
            &self,
        ) -> Result<kern_model::EmbeddingCapabilities, kern_model::ModelError> {
            Ok(kern_model::EmbeddingCapabilities {
                model_id: "fake".to_string(),
                embedding_dim: self.dim,
                max_input_tokens: None,
            })
        }
    }

    const TEST_DIM: usize = 8;

    async fn open_store(dir: &Path) -> LanceVectorStore {
        let mut store = LanceVectorStore::open(dir).await.unwrap();
        store.ensure_table(TEST_DIM as i32).await.unwrap();
        store
    }

    #[tokio::test]
    async fn second_scan_of_an_unchanged_corpus_embeds_nothing() {
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.md"), "# A\n\nContent A.\n").unwrap();
        std::fs::write(corpus.path().join("b.md"), "# B\n\nContent B.\n").unwrap();

        let embedder = CountingEmbedder::new(TEST_DIM);
        let vector_store = open_store(&state.path().join("vectors")).await;
        let indexed_files =
            SqliteIndexedFileRepository::open(&state.path().join("registry.db")).unwrap();
        let chunker = StructuralMarkdownChunker;

        let first = catch_up_scan(
            corpus.path(),
            &vector_store,
            &embedder,
            None,
            &chunker,
            &indexed_files,
            4,
        )
        .await
        .unwrap();
        assert_eq!(first.indexed, 2);
        assert_eq!(first.changed_files, 2);
        assert_eq!(first.unchanged_files, 0);
        let calls_after_first = embedder.calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(calls_after_first > 0);

        let second = catch_up_scan(
            corpus.path(),
            &vector_store,
            &embedder,
            None,
            &chunker,
            &indexed_files,
            4,
        )
        .await
        .unwrap();
        assert_eq!(second.indexed, 0, "nothing changed, nothing to embed");
        assert_eq!(second.unchanged_files, 2);
        assert_eq!(second.changed_files, 0);
        assert_eq!(
            embedder.calls.load(std::sync::atomic::Ordering::SeqCst),
            calls_after_first,
            "a second scan of an unchanged corpus must not call embed() again"
        );
    }

    #[tokio::test]
    async fn changed_file_is_reindexed_and_deleted_file_is_removed_from_the_vector_store() {
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.md"), "# A\n\nOriginal content.\n").unwrap();
        std::fs::write(corpus.path().join("b.md"), "# B\n\nContent B.\n").unwrap();

        let embedder = CountingEmbedder::new(TEST_DIM);
        let vector_store = open_store(&state.path().join("vectors")).await;
        let indexed_files =
            SqliteIndexedFileRepository::open(&state.path().join("registry.db")).unwrap();
        let chunker = StructuralMarkdownChunker;

        catch_up_scan(
            corpus.path(),
            &vector_store,
            &embedder,
            None,
            &chunker,
            &indexed_files,
            4,
        )
        .await
        .unwrap();
        let calls_after_first = embedder.calls.load(std::sync::atomic::Ordering::SeqCst);

        std::fs::write(corpus.path().join("a.md"), "# A\n\nEdited content.\n").unwrap();
        std::fs::remove_file(corpus.path().join("b.md")).unwrap();

        let second = catch_up_scan(
            corpus.path(),
            &vector_store,
            &embedder,
            None,
            &chunker,
            &indexed_files,
            4,
        )
        .await
        .unwrap();
        assert_eq!(second.changed_files, 1, "only a.md was edited");
        assert_eq!(second.deleted_files, 1, "b.md was removed from disk");
        assert!(
            embedder.calls.load(std::sync::atomic::Ordering::SeqCst) > calls_after_first,
            "the edited file's new content should have been embedded"
        );

        let results = vector_store
            .search_hybrid(&[0.5; TEST_DIM], "", 10)
            .await
            .unwrap();
        assert!(
            results.iter().all(|r| !r.chunk.file_path.contains("b.md")),
            "the deleted file's chunks should be gone from the vector store: {:?}",
            results
                .iter()
                .map(|r| &r.chunk.file_path)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_chunk_skipped_via_request_rejected_still_marks_its_file_indexed() {
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let bad_content = "# Bad\n\nToo big for the backend.\n";
        std::fs::write(corpus.path().join("bad.md"), bad_content).unwrap();

        let embedder = CountingEmbedder::new(TEST_DIM);
        embedder.fail(bad_content, FailureMode::RequestRejected);
        let vector_store = open_store(&state.path().join("vectors")).await;
        let indexed_files =
            SqliteIndexedFileRepository::open(&state.path().join("registry.db")).unwrap();
        let chunker = StructuralMarkdownChunker;

        let first = catch_up_scan(
            corpus.path(),
            &vector_store,
            &embedder,
            None,
            &chunker,
            &indexed_files,
            4,
        )
        .await
        .unwrap();
        assert_eq!(first.indexed, 0);
        assert_eq!(first.skipped, 1);
        let calls_after_first = embedder.calls.load(std::sync::atomic::Ordering::SeqCst);

        let second = catch_up_scan(
            corpus.path(),
            &vector_store,
            &embedder,
            None,
            &chunker,
            &indexed_files,
            4,
        )
        .await
        .unwrap();
        assert_eq!(
            second.unchanged_files, 1,
            "the file should be marked indexed despite the skipped chunk, so a rescan treats \
             identical bytes as unchanged instead of retrying forever"
        );
        assert_eq!(
            embedder.calls.load(std::sync::atomic::Ordering::SeqCst),
            calls_after_first,
            "no re-embed attempt against the same rejected content"
        );
    }

    #[tokio::test]
    async fn a_backend_failure_mid_scan_does_not_mark_the_failing_file_indexed() {
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let bad_content = "# Bad\n\nWill fail.\n";
        std::fs::write(corpus.path().join("bad.md"), bad_content).unwrap();

        let embedder = CountingEmbedder::new(TEST_DIM);
        embedder.fail(bad_content, FailureMode::BackendUnavailable);
        let vector_store = open_store(&state.path().join("vectors")).await;
        let indexed_files =
            SqliteIndexedFileRepository::open(&state.path().join("registry.db")).unwrap();
        let chunker = StructuralMarkdownChunker;

        let result = catch_up_scan(
            corpus.path(),
            &vector_store,
            &embedder,
            None,
            &chunker,
            &indexed_files,
            4,
        )
        .await;
        assert!(
            result.is_err(),
            "a genuine backend failure should abort catch_up_scan, not be silently skipped"
        );

        let known = indexed_files.all().await.unwrap();
        assert!(
            known.is_empty(),
            "the failing file must not be marked indexed, so a retry reprocesses it"
        );
    }
}
