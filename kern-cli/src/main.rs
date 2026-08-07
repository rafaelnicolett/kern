//! kern-cli — binario final: `project create`, `serve`, `status`.
//!
//! Unico runtime: o mesmo binario observa, converte, extrai, indexa e serve.
//! Ver docs/domain/superficie-de-agente/aggregates.md (workspace de
//! delivery) para a maquina de estados do processo (KernProcess).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use kern_ingest::{MarkdownChunker, StructuralMarkdownChunker};
use kern_mcp::KernServer;
use kern_model::{EmbeddingProvider, LlamaCppRuntime, OllamaClient};
use kern_ontology::{
    SqliteFrontmatterProfileRepository, SqliteInstanceRepository, SqliteTypeRepository,
    TypeRepository,
};
use kern_vector::{ChunkRecord, LanceVectorStore, VectorStore};
use tracing_subscriber::EnvFilter;

mod embedded;

/// Estados do KernProcess — transicoes estritamente sequenciais, sem
/// retrocesso (docs/domain/superficie-de-agente/aggregates.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessState {
    Starting,
    CatchUpScan,
    Ready,
    Draining,
    Stopped,
}

#[derive(Parser)]
#[command(name = "kern", version, about = "RAG local + ontologia incremental")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Gerencia projetos isolados (pasta + estado próprio).
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Sobe o processo: CatchUpScan seguido de servidor MCP stdio.
    Serve {
        #[arg(long)]
        project: String,
    },
    /// Reporta saúde do projeto — funciona mesmo sem um `serve` rodando,
    /// lendo o estado persistido (v0 é single-client, sem coordenação
    /// multi-processo — métricas em memória de uma sessão `serve` corrente
    /// não são visíveis aqui, só o que já foi persistido).
    Status {
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Cria um projeto isolado — nome único na máquina local.
    Create {
        name: String,
        #[arg(long)]
        path: PathBuf,
    },
}

fn registry_path() -> anyhow::Result<PathBuf> {
    // KERN_HOME sobrepõe o diretório home — usado por testes de integração
    // pra isolar o registry global entre execuções paralelas.
    if let Ok(override_home) = std::env::var("KERN_HOME") {
        return Ok(PathBuf::from(override_home).join("projects.json"));
    }
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("não foi possível resolver o diretório home"))?;
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
    // Nome único no escopo da máquina local (docs/domain/superficie-de-agente/aggregates.md).
    if registry.contains_key(&name) {
        anyhow::bail!("SUPERFICIE_DE_AGENTE.PROJETO_JA_EXISTE: '{name}' já existe");
    }

    std::fs::create_dir_all(path.join(".kern"))?;
    let db_path = path.join(".kern").join("registry.db");

    // Um TypeRegistry + um InstanceGraph próprios — nunca compartilhados
    // entre projetos.
    let types = SqliteTypeRepository::open(&db_path)?;
    types.seed_canonical_vocabulary().await?;
    let _instances = SqliteInstanceRepository::open(&db_path)?;
    let _frontmatter = SqliteFrontmatterProfileRepository::open(&db_path)?;
    let _vectors = LanceVectorStore::open(&path.join(".kern").join("vectors")).await?;

    registry.insert(name.clone(), path.clone());
    save_registry(&registry)?;

    println!("projeto '{name}' criado em {}", path.display());
    Ok(())
}

fn resolve_project(name: &str) -> anyhow::Result<PathBuf> {
    let registry = load_registry()?;
    registry
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("SUPERFICIE_DE_AGENTE.PROJETO_NAO_ENCONTRADO: '{name}'"))
}

/// CatchUpScan — reusa o mesmo hash-diff do watcher pra recuperar atraso
/// (docs/domain/superficie-de-agente/aggregates.md). Só cobre chunk+embed+
/// index (kern-ontology precisa de conteúdo estruturado — extração de
/// entidades/relações fica pra quando o corpus tiver frontmatter real,
/// não é bloqueante pro critério de aceite do v0).
async fn catch_up_scan(
    root: &Path,
    vector_store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
) -> anyhow::Result<usize> {
    let chunker = StructuralMarkdownChunker;
    let mut indexed = 0usize;

    for entry in walk_markdown_files(root)? {
        let content = std::fs::read_to_string(&entry)?;
        let chunks = chunker.chunk(&entry, &content);
        for chunk in chunks {
            let embedding = embedder.embed(&chunk.content).await?;
            vector_store
                .upsert(ChunkRecord {
                    id: chunk.id,
                    file_path: chunk.file_path.to_string_lossy().to_string(),
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

fn walk_markdown_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()) == Some(".kern") {
                continue; // nunca reindexa o próprio estado do kern
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// Último recurso quando Ollama não responde: extrai `llama-server` (só
/// existe em builds de release com a feature `bundled-llama-server`) e sobe
/// contra um `.gguf` já em cache local. Nunca tenta baixar nada da rede —
/// falha explícita e imediata se faltar peça (ver BDD: "Primeira execução
/// sem internet falha de forma clara" — nenhum fallback silencioso).
async fn spawn_embedded_embedder() -> anyhow::Result<LlamaCppRuntime> {
    let binary = embedded::ensure_llama_server_binary().map_err(|e| {
        anyhow::anyhow!("nenhum backend de modelo disponível: Ollama não responde em :11434 e {e}")
    })?;
    let model = embedded::find_cached_model().ok_or_else(|| {
        anyhow::anyhow!(
            "SUPERFICIE_DE_AGENTE.MODELO_AUSENTE_NO_CACHE: nenhum .gguf encontrado em \
             ~/.cache/kern/models — download automático via Hugging Face Hub ainda não \
             implementado nesta build; popule o cache manualmente antes de rodar sem Ollama"
        )
    })?;
    let port = pick_free_port()?;
    LlamaCppRuntime::spawn(&binary, &model, port)
        .await
        .map_err(|e| anyhow::anyhow!("falha ao iniciar backend embarcado (llama-server): {e}"))
}

fn pick_free_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

async fn cmd_serve(project: String) -> anyhow::Result<()> {
    let mut state = ProcessState::Starting;
    tracing::info!(?state, project, "iniciando");

    let root = resolve_project(&project)?;
    let db_path = root.join(".kern").join("registry.db");

    let ollama = OllamaClient::new("all-minilm");
    let embedder: Arc<dyn EmbeddingProvider> = if ollama.probe().await {
        Arc::new(ollama)
    } else {
        Arc::new(spawn_embedded_embedder().await?)
    };

    let types: Arc<dyn TypeRepository> = Arc::new(SqliteTypeRepository::open(&db_path)?);
    let instances: Arc<dyn kern_ontology::InstanceRepository> =
        Arc::new(SqliteInstanceRepository::open(&db_path)?);
    let vector_store: Arc<dyn VectorStore> =
        Arc::new(LanceVectorStore::open(&root.join(".kern").join("vectors")).await?);

    state = ProcessState::CatchUpScan;
    tracing::info!(?state, "varredura de recuperação — indexando corpus");
    let indexed = catch_up_scan(&root, vector_store.as_ref(), embedder.as_ref()).await?;
    tracing::info!(chunks_indexados = indexed, "catch-up concluído");

    state = ProcessState::Ready;
    tracing::info!(?state, "pronto — servindo MCP via stdio");

    let server = KernServer::new(types, instances, vector_store, embedder);
    let service = rmcp::ServiceExt::serve(server, rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("erro ao servir: {e:?}"))?;

    tokio::select! {
        result = service.waiting() => {
            result?;
        }
        _ = tokio::signal::ctrl_c() => {
            state = ProcessState::Draining;
            tracing::info!(?state, "sinal recebido — encerrando");
        }
    }

    state = ProcessState::Stopped;
    tracing::info!(?state, "encerrado");
    Ok(())
}

async fn cmd_status(project: Option<String>) -> anyhow::Result<()> {
    let registry = load_registry()?;

    let Some(name) = project else {
        if registry.is_empty() {
            println!("nenhum projeto criado ainda — kern project create <nome> --path <pasta>");
        } else {
            println!("projetos registrados:");
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

    println!("projeto: {name} ({})", root.display());
    println!("chunks indexados: {chunk_count}");
    println!(
        "tipos de entidade: {} | tipos de relação: {} ({canonical} canônicos)",
        entity_types.len(),
        relation_types.len()
    );
    println!(
        "nota: taxa de fallback é métrica em memória de uma sessão `serve` — \
         não visível aqui entre processos (v0 é single-client, sem coordenação multi-processo)"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr) // stdout é reservado ao protocolo MCP
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
