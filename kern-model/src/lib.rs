//! kern-model — model provider abstraction (embedding + extraction/judge).
//!
//! Ports (traits) consumed by kern-ingest (embed) and kern-ontology
//! (extract/judge). The real backend (embedded via a llama-server subprocess,
//! or opportunistic Ollama) is an implementation detail behind these traits
//! (hexagonal architecture, see docs/adr/0001).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("inference subprocess unavailable: {0}")]
    BackendUnavailable(String),
    #[error("malformed inference response: {0}")]
    MalformedResponse(String),
    #[error("HTTP request to backend failed: {0}")]
    Http(#[from] reqwest::Error),
    // TODO: timeout variants, model not loaded, etc.
}

/// Ground-truth self-description of an `EmbeddingProvider` — never a static
/// claim about "the" model kern assumes is running, always derived from a
/// real round-trip against the actual backend. `max_input_tokens` is `None`
/// when the backend genuinely can't report it; callers (kern-ingest's
/// chunker) fall back to their own conservative default in that case.
#[derive(Debug, Clone)]
pub struct EmbeddingCapabilities {
    pub model_id: String,
    pub embedding_dim: usize,
    pub max_input_tokens: Option<usize>,
}

/// Port consumed by kern-ingest to vectorize chunks.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, ModelError>;
    // TODO(kern-ingest): embed_batch to reduce round-trips to the subprocess.

    /// Queried once per `serve` session at the composition root, not per
    /// chunk — implementors may do a real round-trip to answer this.
    async fn capabilities(&self) -> Result<EmbeddingCapabilities, ModelError>;
}

/// Entity/relation candidate extracted from a chunk — pre-classification.
/// TODO: replace with the real type, shared with kern-ontology (for now it's
/// only here to shape the trait signature).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateEntity {
    pub raw_name: String,
    pub raw_type_hint: Option<String>,
}

/// Relation vocabulary — the 8 seeded canonical types plus promoted
/// candidates. TODO: the real type comes from kern-ontology; this is just a
/// placeholder to close out `extract`'s signature.
#[derive(Debug, Clone, Default)]
pub struct RelationVocabulary {
    pub canonical_types: Vec<String>,
}

/// An already-existing entity type, candidate for a match — used by `judge`.
/// TODO: replace with the real type from kern-ontology::TypeRegistry.
#[derive(Debug, Clone)]
pub struct EntityType {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JudgeDecision {
    Merge,
    NewType,
    Reject,
}

/// Port consumed by kern-ontology — only called in the ambiguous zone.
/// Loaded lazily, unloaded after idling.
#[async_trait]
pub trait ExtractionProvider: Send + Sync {
    /// Sync, zero I/O — known at construction. For display (`kern status`,
    /// `kern config show`), not for any budget/capability decision.
    fn model_id(&self) -> &str;

    async fn extract(
        &self,
        chunk: &str,
        vocab: &RelationVocabulary,
    ) -> Result<Vec<CandidateEntity>, ModelError>;

    async fn judge(
        &self,
        candidate: &CandidateEntity,
        nearest_existing: &[EntityType],
    ) -> Result<JudgeDecision, ModelError>;

    /// Interprets the frontmatter field -> canonical concept mapping, once
    /// per new shape. `keys` is the set of frontmatter keys, never the
    /// values. Returns, per key, the matching canonical concept (`id`,
    /// `kind`, `status`, `depends_on`, `implements`, ...) or `None` if the
    /// key doesn't map to anything known.
    async fn interpret_frontmatter_schema(
        &self,
        keys: &[String],
    ) -> Result<std::collections::HashMap<String, Option<String>>, ModelError>;
}

/// Embedded backend — spawns `llama-server` (llama.cpp) as a local
/// subprocess and talks to it over HTTP (`/v1/embeddings`, OpenAI-compatible).
/// `ExtractionProvider` is not implemented here: extraction/judge remain
/// restricted to the opportunistic adapter (`OllamaClient`) until an embedded
/// generative model comes into scope.
pub struct LlamaCppRuntime {
    base_url: String,
    http: reqwest::Client,
    model_id: String,
    context_size: u32,
    // Keeps the subprocess alive for the runtime's lifetime — dropping this
    // terminates the `llama-server` (kill_on_drop, see `spawn`).
    _child: tokio::process::Child,
}

#[derive(Serialize)]
struct OpenAiEmbedRequest<'a> {
    input: &'a str,
}

#[derive(Deserialize)]
struct OpenAiEmbedResponse {
    data: Vec<OpenAiEmbedDatum>,
}

#[derive(Deserialize)]
struct OpenAiEmbedDatum {
    embedding: Vec<f32>,
}

impl LlamaCppRuntime {
    /// Starts `llama-server` pointing at the `.gguf` in `model_path`, on
    /// port `port` (localhost only), and waits for `/health` to respond OK
    /// before returning — never returns a runtime that isn't yet ready to
    /// serve embeddings. `context_size` is passed explicitly as both `-c`
    /// and `-b`/`-ub` — the caller decides it (kern has to know its own
    /// backend's real limit, not guess), rather than silently inheriting
    /// whatever llama-server's build default happens to be.
    pub async fn spawn(
        binary_path: &std::path::Path,
        model_path: &std::path::Path,
        port: u16,
        context_size: u32,
    ) -> Result<Self, ModelError> {
        let mut child = tokio::process::Command::new(binary_path)
            .arg("-m")
            .arg(model_path)
            .arg("--embedding")
            .arg("--port")
            .arg(port.to_string())
            .arg("--host")
            .arg("127.0.0.1")
            .arg("-c")
            .arg(context_size.to_string())
            .arg("-b")
            .arg(context_size.to_string())
            .arg("-ub")
            .arg(context_size.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                ModelError::BackendUnavailable(format!("failed to start llama-server: {e}"))
            })?;

        let base_url = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::new();

        // Readiness poll — llama-server loads the model before opening the
        // HTTP socket, so the first connection attempts normally fail; total
        // timeout generous enough for small models (the release process
        // uses models in the tens-of-MB range, not full-size LLMs).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(ModelError::BackendUnavailable(format!(
                    "llama-server exited before becoming ready (status: {status})"
                )));
            }

            let ready = http
                .get(format!("{base_url}/health"))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if ready {
                break;
            }

            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill().await;
                return Err(ModelError::BackendUnavailable(
                    "llama-server did not respond to /health within the timeout".to_string(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let model_id = format!(
            "llama-cpp:{}",
            model_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| model_path.display().to_string())
        );

        Ok(Self {
            base_url,
            http,
            model_id,
            context_size,
            _child: child,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for LlamaCppRuntime {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, ModelError> {
        let response = self
            .http
            .post(format!("{}/v1/embeddings", self.base_url))
            .json(&OpenAiEmbedRequest { input: text })
            .send()
            .await?
            .error_for_status()
            .map_err(|e| ModelError::BackendUnavailable(e.to_string()))?
            .json::<OpenAiEmbedResponse>()
            .await?;

        response
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or_else(|| ModelError::MalformedResponse("response without embeddings".to_string()))
    }

    /// `max_input_tokens` is deterministic here — kern spawned this
    /// subprocess itself with an explicit `-c`/`-b`, so it's just an echo,
    /// no extra round-trip needed. `embedding_dim` still comes from a real
    /// canary embed: never trust a static claim about "the" model's output
    /// size when a real call can just confirm it.
    async fn capabilities(&self) -> Result<EmbeddingCapabilities, ModelError> {
        let dim = self.embed("kern capability probe").await?.len();
        Ok(EmbeddingCapabilities {
            model_id: self.model_id.clone(),
            embedding_dim: dim,
            max_input_tokens: Some(self.context_size as usize),
        })
    }
}

/// Opportunistic adapter — only used if the probe on `:11434` detects the
/// daemon. Never a required dependency (a closed design decision).
pub struct OllamaClient {
    base_url: String,
    model: String,
    http: reqwest::Client,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Serialize)]
struct ShowRequest<'a> {
    model: &'a str,
}

#[derive(Deserialize)]
struct ShowResponse {
    /// Free-form, newline-separated `key value` pairs — e.g.
    /// `"num_ctx                        256"`. This is Ollama's actual
    /// *runtime* setting for the model, which can be tighter than the
    /// model's architectural max reported in `model_info` (empirically
    /// confirmed: `all-minilm`'s `model_info["bert.context_length"]` is
    /// 512, but Ollama actually serves it with `num_ctx` 256 — reading
    /// `model_info` alone would under-report the real, enforced limit).
    parameters: Option<String>,
    model_info: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<OllamaModelInfo>,
}

/// One locally-pulled Ollama model, as reported by the real `/api/tags`
/// response — `capabilities` and `details` come straight from Ollama, not
/// inferred from the model name.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaModelInfo {
    pub name: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub details: OllamaModelDetails,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OllamaModelDetails {
    pub family: Option<String>,
    pub context_length: Option<u64>,
    pub embedding_length: Option<u64>,
}

impl OllamaModelInfo {
    pub fn supports_embedding(&self) -> bool {
        self.capabilities.iter().any(|c| c == "embedding")
    }

    pub fn supports_completion(&self) -> bool {
        self.capabilities.iter().any(|c| c == "completion")
    }
}

impl OllamaClient {
    pub fn new(model: impl Into<String>) -> Self {
        Self::with_base_url("http://localhost:11434", model)
    }

    pub fn with_base_url(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Availability probe — never treats Ollama as mandatory; the caller
    /// decides what to do if this returns `false` (fall back to the
    /// embedded backend).
    pub async fn probe(&self) -> bool {
        self.http
            .get(format!("{}/api/version", self.base_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Real, locally-pulled models — used by the setup wizard to present
    /// actual choices instead of guessing "embedding vs chat" from name
    /// patterns. `capabilities` comes straight from Ollama's own real
    /// `/api/tags` response (empirically confirmed: it reports
    /// `["embedding"]` for an embedding model, `["completion", "tools"]`
    /// for a generative one) — never inferred.
    pub async fn list_models(&self) -> Result<Vec<OllamaModelInfo>, ModelError> {
        let response = self
            .http
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await?
            .error_for_status()
            .map_err(|e| ModelError::BackendUnavailable(e.to_string()))?
            .json::<TagsResponse>()
            .await?;
        Ok(response.models)
    }

    async fn show(&self) -> Result<ShowResponse, ModelError> {
        self.http
            .post(format!("{}/api/show", self.base_url))
            .json(&ShowRequest { model: &self.model })
            .send()
            .await?
            .error_for_status()
            .map_err(|e| ModelError::BackendUnavailable(e.to_string()))?
            .json::<ShowResponse>()
            .await
            .map_err(ModelError::from)
    }

    /// `num_ctx` from the live `parameters` string first — the value Ollama
    /// will actually enforce. Falls back to a `*.context_length` key in
    /// `model_info` (the model's architectural max) only if `num_ctx` isn't
    /// present. `None` if neither is parseable — a genuinely unknown limit,
    /// not a guess.
    fn parse_max_input_tokens(show: &ShowResponse) -> Option<usize> {
        let from_params = show.parameters.as_deref().and_then(|params| {
            params.lines().find_map(|line| {
                let mut parts = line.split_whitespace();
                if parts.next()? == "num_ctx" {
                    parts.next()?.parse::<usize>().ok()
                } else {
                    None
                }
            })
        });
        from_params.or_else(|| {
            show.model_info.as_ref().and_then(|info| {
                info.iter()
                    .find(|(k, _)| k.ends_with(".context_length"))
                    .and_then(|(_, v)| v.as_u64())
                    .map(|n| n as usize)
            })
        })
    }
}

#[async_trait]
impl EmbeddingProvider for OllamaClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, ModelError> {
        let response = self
            .http
            .post(format!("{}/api/embed", self.base_url))
            .json(&EmbedRequest {
                model: &self.model,
                input: text,
            })
            .send()
            .await?
            .error_for_status()
            .map_err(|e| ModelError::BackendUnavailable(e.to_string()))?
            .json::<EmbedResponse>()
            .await?;

        response
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| ModelError::MalformedResponse("response without embeddings".to_string()))
    }

    async fn capabilities(&self) -> Result<EmbeddingCapabilities, ModelError> {
        let dim = self.embed("kern capability probe").await?.len();
        // A missing/unreachable /api/show shouldn't make an otherwise-working
        // embedder unusable — max_input_tokens degrades to `None` (caller
        // applies its own conservative default) rather than failing the
        // whole capability probe.
        let max_input_tokens = match self.show().await {
            Ok(show) => Self::parse_max_input_tokens(&show),
            Err(_) => None,
        };
        Ok(EmbeddingCapabilities {
            model_id: format!("ollama:{}", self.model),
            embedding_dim: dim,
            max_input_tokens,
        })
    }
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: String,
    format: serde_json::Value,
    stream: bool,
    options: GenerateOptions,
}

#[derive(Serialize)]
struct GenerateOptions {
    temperature: f32,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

#[derive(Deserialize)]
struct ExtractedEntities {
    entities: Vec<CandidateEntity>,
}

#[derive(Deserialize)]
struct JudgeResponse {
    decision: String,
}

fn extract_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "entities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "raw_name": {"type": "string"},
                        "raw_type_hint": {"type": "string"}
                    },
                    "required": ["raw_name"]
                }
            }
        },
        "required": ["entities"]
    })
}

fn judge_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "decision": {"type": "string", "enum": ["Merge", "NewType", "Reject"]}
        },
        "required": ["decision"]
    })
}

#[derive(Deserialize)]
struct FrontmatterKeyMapping {
    canonical_concept: Option<String>,
}

/// `canonical_concept` is constrained with a JSON Schema `enum` — this is
/// load-bearing, not decorative: an earlier version only *described* the
/// concepts in the prompt (in prose, to disambiguate close pairs like
/// `depends_on`/`supersedes`), and the model started echoing the
/// description back as the value (e.g. `"blocking prerequisite"`) instead
/// of the keyword `depends_on`. `enum` forces the value to actually be one
/// of the 11 real concept strings (or `null`), enforced by Ollama's
/// structured-output grammar, not just requested in prose.
fn frontmatter_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "canonical_concept": {
                "enum": [
                    "id", "kind", "status", "depends_on", "implements",
                    "supersedes", "causes", "owned_by", "conflicts_with",
                    "documents", "configures", null
                ]
            }
        }
    })
}

impl OllamaClient {
    async fn generate_structured<T: for<'de> Deserialize<'de>>(
        &self,
        prompt: String,
        format: serde_json::Value,
    ) -> Result<T, ModelError> {
        let response = self
            .http
            .post(format!("{}/api/generate", self.base_url))
            .json(&GenerateRequest {
                model: &self.model,
                prompt,
                format,
                stream: false,
                options: GenerateOptions { temperature: 0.0 },
            })
            .send()
            .await?
            .error_for_status()
            .map_err(|e| ModelError::BackendUnavailable(e.to_string()))?
            .json::<GenerateResponse>()
            .await?;

        serde_json::from_str(&response.response)
            .map_err(|e| ModelError::MalformedResponse(format!("{e}: {}", response.response)))
    }
}

#[async_trait]
impl ExtractionProvider for OllamaClient {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn extract(
        &self,
        chunk: &str,
        vocab: &RelationVocabulary,
    ) -> Result<Vec<CandidateEntity>, ModelError> {
        let types_hint = if vocab.canonical_types.is_empty() {
            "no canonical types yet".to_string()
        } else {
            vocab.canonical_types.join(", ")
        };
        let prompt = format!(
            "Extract candidate entities (concepts, components, artifacts) \
             mentioned in the text below. Known canonical relation types \
             (context only, don't invent relations): {types_hint}\n\n\
             Text:\n{chunk}"
        );

        let extracted: ExtractedEntities =
            self.generate_structured(prompt, extract_schema()).await?;
        Ok(extracted.entities)
    }

    async fn judge(
        &self,
        candidate: &CandidateEntity,
        nearest_existing: &[EntityType],
    ) -> Result<JudgeDecision, ModelError> {
        let existing_list = if nearest_existing.is_empty() {
            "no nearby type found".to_string()
        } else {
            nearest_existing
                .iter()
                .map(|t| format!("- {}: {}", t.name, t.description))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let prompt = format!(
            "An entity candidate fell into the ambiguous zone of similarity \
             against existing types. Decide: 'Merge' (it's the same concept \
             as an existing type), 'NewType' (it's a genuinely new concept), \
             or 'Reject' (it's not a valid entity — extraction noise).\n\n\
             Candidate: {} (type hint: {})\n\n\
             Closest existing types:\n{existing_list}",
            candidate.raw_name,
            candidate.raw_type_hint.as_deref().unwrap_or("none"),
        );

        let judged: JudgeResponse = self.generate_structured(prompt, judge_schema()).await?;
        match judged.decision.as_str() {
            "Merge" => Ok(JudgeDecision::Merge),
            "NewType" => Ok(JudgeDecision::NewType),
            "Reject" => Ok(JudgeDecision::Reject),
            other => Err(ModelError::MalformedResponse(format!(
                "unknown decision: {other}"
            ))),
        }
    }

    async fn interpret_frontmatter_schema(
        &self,
        keys: &[String],
    ) -> Result<std::collections::HashMap<String, Option<String>>, ModelError> {
        // Real finding, verified empirically: classifying every key in one
        // batched call is order-sensitive for a small model — the exact
        // same prompt/definitions reliably classified `depends_on`/
        // `implements` correctly when they came LAST in the key list, and
        // reliably misclassified them (as `causes`/`supersedes`) when they
        // came FIRST — which `parse_frontmatter_keys`' alphabetical sort
        // does for a typical SDD key set. One call per key removes the
        // ordering variable entirely: more round-trips, but each key is
        // judged on its own, and this whole method only runs once per new
        // key-shape per folder anyway (cached after that, see
        // `FrontmatterProfile`).
        let mut mapping = std::collections::HashMap::new();
        for key in keys {
            let concept = self.interpret_single_frontmatter_key(key).await?;
            mapping.insert(key.clone(), concept);
        }
        Ok(mapping)
    }
}

impl OllamaClient {
    async fn interpret_single_frontmatter_key(
        &self,
        key: &str,
    ) -> Result<Option<String>, ModelError> {
        // Short definitions for each concept — measurably improved
        // reliability over bare names alone in manual testing (models
        // otherwise conflate semantically close pairs, e.g. `depends_on`
        // vs `supersedes`). Not "final", just the current best-known
        // version.
        let canonical_concepts = "\
            id (the item's own unique identifier), \
            kind (the item's type/category, e.g. task, spec, adr), \
            status (the item's current state, e.g. draft, in_progress, done), \
            depends_on (this item cannot be completed until the referenced item is done — a blocking prerequisite), \
            implements (this item is the concrete realization of the referenced item, e.g. a task implementing a spec), \
            supersedes (this item replaces/obsoletes the referenced item, which should no longer be used), \
            causes (this item is the origin/trigger of the referenced item, e.g. an incident causing a follow-up task), \
            owned_by (the referenced item is the person/team responsible for this one), \
            conflicts_with (this item is incompatible with the referenced item — cannot both hold at once), \
            documents (this item explains/describes the referenced item), \
            configures (this item sets parameters/settings for the referenced item)";
        let prompt = format!(
            "The frontmatter key below comes from an unknown Spec-Driven \
             Development generator. Say which canonical concept it \
             corresponds to, or null if it doesn't correspond to any. The \
             canonical concepts, with what each one means:\n\
             {canonical_concepts}\n\n\
             Key: {key}"
        );

        let mapped: FrontmatterKeyMapping = self
            .generate_structured(prompt, frontmatter_schema())
            .await?;
        Ok(mapped.canonical_concept)
    }
}

/// Which embedding backend to build, and what it needs to start. An
/// open-ended alternative to a fixed dispatch enum (see docs/adr/0008) —
/// `kern-cli`'s composition root picks one of these from persisted config
/// (or a first-run wizard) instead of the previous hardcoded
/// probe-then-fallback logic.
#[derive(Debug, Clone)]
pub enum EmbeddingProviderSelection {
    Ollama {
        model: String,
        base_url: Option<String>,
    },
    LlamaCppEmbedded {
        binary_path: std::path::PathBuf,
        model_path: std::path::PathBuf,
        port: u16,
        context_size: u32,
    },
}

pub async fn build_embedding_provider(
    selection: EmbeddingProviderSelection,
) -> Result<std::sync::Arc<dyn EmbeddingProvider>, ModelError> {
    match selection {
        EmbeddingProviderSelection::Ollama { model, base_url } => {
            let client = match base_url {
                Some(url) => OllamaClient::with_base_url(url, model),
                None => OllamaClient::new(model),
            };
            Ok(std::sync::Arc::new(client))
        }
        EmbeddingProviderSelection::LlamaCppEmbedded {
            binary_path,
            model_path,
            port,
            context_size,
        } => {
            let runtime =
                LlamaCppRuntime::spawn(&binary_path, &model_path, port, context_size).await?;
            Ok(std::sync::Arc::new(runtime))
        }
    }
}

/// Only one real backend implements `ExtractionProvider` today (Ollama) —
/// kept as an enum rather than skipping straight to a bare `OllamaClient`
/// constructor for the same "keep the door open" reasoning ADR-0001 already
/// applied to `ExtractionProvider` itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionProviderKind {
    Ollama,
}

pub async fn build_extraction_provider(
    kind: ExtractionProviderKind,
    model: String,
    base_url: Option<String>,
) -> Result<std::sync::Arc<dyn ExtractionProvider>, ModelError> {
    match kind {
        ExtractionProviderKind::Ollama => {
            let client = match base_url {
                Some(url) => OllamaClient::with_base_url(url, model),
                None => OllamaClient::new(model),
            };
            Ok(std::sync::Arc::new(client))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real integration against local Ollama — skips (doesn't fail) if the
    /// daemon isn't running or the `all-minilm` model hasn't been pulled.
    /// Run `ollama pull all-minilm` (~46MB) before running locally.
    #[tokio::test]
    async fn embed_via_ollama_real_returns_non_empty_vector() {
        let client = OllamaClient::new("all-minilm");
        if !client.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let embedding = match client.embed("real embedding test").await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("all-minilm model unavailable ({e}) — skipping integration test");
                return;
            }
        };

        assert!(!embedding.is_empty());
        assert_eq!(embedding.len(), kern_vector_embedding_dim_hint());
    }

    /// Confirms a real, previously-unverified assumption this whole
    /// capability design rests on: Ollama's `/api/show` reports the model's
    /// *architectural* max in `model_info["bert.context_length"]` (512 for
    /// all-minilm), separately from the *runtime* `num_ctx` it actually
    /// enforces (256, confirmed against the real running `llama-server`
    /// process args: `-c 256 -b 256`). `capabilities()` must surface 256,
    /// not 512 — reading the architectural max alone would under-budget
    /// the chunker and the real embed call would still fail.
    #[tokio::test]
    async fn ollama_capabilities_reports_runtime_num_ctx_not_architectural_max() {
        let client = OllamaClient::new("all-minilm");
        if !client.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let caps = match client.capabilities().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("all-minilm model unavailable ({e}) — skipping integration test");
                return;
            }
        };

        assert_eq!(caps.embedding_dim, 384);
        assert_eq!(
            caps.max_input_tokens,
            Some(256),
            "expected Ollama's real runtime num_ctx (256), not bert.context_length's \
             architectural max (512) — if this changed, Ollama's default deployment \
             config for all-minilm changed, re-verify manually before updating"
        );
        assert_eq!(caps.model_id, "ollama:all-minilm");
    }

    /// Confirms `/api/tags` really reports usable capability metadata per
    /// model — the setup wizard relies on this to classify models as
    /// embedding- vs completion-capable without guessing from their name.
    #[tokio::test]
    async fn list_models_reports_real_capabilities_not_guessed_from_name() {
        let client = OllamaClient::new("all-minilm");
        if !client.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let models = match client.list_models().await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("list_models failed ({e}) — skipping integration test");
                return;
            }
        };

        let Some(all_minilm) = models.iter().find(|m| m.name.starts_with("all-minilm")) else {
            eprintln!("all-minilm not in the local Ollama pull list — skipping integration test");
            return;
        };
        assert!(
            all_minilm.supports_embedding(),
            "all-minilm should report embedding capability: {:?}",
            all_minilm.capabilities
        );
        assert!(!all_minilm.supports_completion());
    }

    /// Keeps the test honest about the expected dimension without coupling
    /// kern-model to kern-vector (a test-only dependency, not a product one).
    fn kern_vector_embedding_dim_hint() -> usize {
        384
    }

    /// Locates `llama-server` on PATH without depending on a fixed install
    /// path — CI and local machines may have it in different places.
    fn find_llama_server_binary() -> Option<std::path::PathBuf> {
        let path_var = std::env::var_os("PATH")?;
        std::env::split_paths(&path_var)
            .map(|dir| dir.join("llama-server"))
            .find(|candidate| candidate.is_file())
    }

    /// Small GGUF model used only to test the subprocess end to end — not
    /// the production model, it just needs to support `--embedding`.
    fn find_test_gguf_model() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME")?;
        let path = std::path::PathBuf::from(home)
            .join(".cache")
            .join("kern")
            .join("models")
            .join("bge-small-en-v1.5-q4_k_m.gguf");
        path.is_file().then_some(path)
    }

    /// Real integration: spawns `llama-server` as an actual subprocess and
    /// calls `/v1/embeddings` on it. Skips (doesn't fail) if the binary or
    /// the test model aren't available on the machine — CI doesn't depend
    /// on having them installed.
    #[tokio::test]
    async fn embed_via_llama_cpp_runtime_real_returns_non_empty_vector() {
        let Some(binary) = find_llama_server_binary() else {
            eprintln!("llama-server not found on PATH — skipping integration test");
            return;
        };
        let Some(model) = find_test_gguf_model() else {
            eprintln!(
                "test model ~/.cache/kern/models/bge-small-en-v1.5-q4_k_m.gguf \
                 not found — skipping integration test"
            );
            return;
        };

        let runtime = LlamaCppRuntime::spawn(&binary, &model, 8791, 512)
            .await
            .expect("llama-server should start and become ready within the timeout");

        let embedding = runtime
            .embed("real embedding test via llama-server")
            .await
            .expect("embed should return a valid vector");

        assert!(!embedding.is_empty());
    }

    /// `capabilities()` on the embedded backend is deterministic (echoes
    /// the `context_size` it was spawned with) but `embedding_dim` still
    /// has to come from a real canary embed — assert both.
    #[tokio::test]
    async fn llama_cpp_runtime_capabilities_reports_spawned_context_size_and_real_dim() {
        let Some(binary) = find_llama_server_binary() else {
            eprintln!("llama-server not found on PATH — skipping integration test");
            return;
        };
        let Some(model) = find_test_gguf_model() else {
            eprintln!(
                "test model ~/.cache/kern/models/bge-small-en-v1.5-q4_k_m.gguf \
                 not found — skipping integration test"
            );
            return;
        };

        let runtime = LlamaCppRuntime::spawn(&binary, &model, 8792, 512)
            .await
            .expect("llama-server should start and become ready within the timeout");

        let caps = runtime
            .capabilities()
            .await
            .expect("capabilities should succeed against a real running llama-server");

        assert_eq!(caps.max_input_tokens, Some(512));
        assert!(caps.embedding_dim > 0);
        assert!(caps.model_id.contains("bge-small-en-v1.5-q4_k_m.gguf"));
    }

    /// Real integration against Ollama — `llama3.2` is a common generative
    /// model (unlike `all-minilm`, which is embedding-only). Skips if
    /// unavailable.
    #[tokio::test]
    async fn extract_via_ollama_real_finds_mentioned_entities() {
        let client = OllamaClient::new("llama3.2");
        if !client.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let vocab = RelationVocabulary {
            canonical_types: vec!["depends_on".to_string()],
        };
        let candidates = match client
            .extract(
                "kern-ontology depends on kern-model to generate embeddings.",
                &vocab,
            )
            .await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("llama3.2 model unavailable ({e}) — skipping integration test");
                return;
            }
        };

        assert!(
            !candidates.is_empty(),
            "expected at least 1 extracted candidate"
        );
        assert!(candidates
            .iter()
            .any(|c| c.raw_name.to_lowercase().contains("kern")));
    }

    #[tokio::test]
    async fn judge_via_ollama_real_returns_a_valid_decision() {
        let client = OllamaClient::new("llama3.2");
        if !client.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let candidate = CandidateEntity {
            raw_name: "kern-vector".to_string(),
            raw_type_hint: Some("crate".to_string()),
        };
        let nearest = vec![EntityType {
            name: "kern-ingest".to_string(),
            description: "ingestion and chunking crate".to_string(),
        }];

        let decision = match client.judge(&candidate, &nearest).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("llama3.2 model unavailable ({e}) — skipping integration test");
                return;
            }
        };

        // Any of the 3 is a structurally valid response — what this test
        // guarantees is that the HTTP + schema + parse pipeline works end
        // to end against a real model, not which decision comes out.
        assert!(matches!(
            decision,
            JudgeDecision::Merge | JudgeDecision::NewType | JudgeDecision::Reject
        ));
    }

    #[tokio::test]
    async fn interpret_frontmatter_schema_via_ollama_real_maps_obvious_keys() {
        let client = OllamaClient::new("llama3.2");
        if !client.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let keys = vec![
            "id".to_string(),
            "kind".to_string(),
            "depends_on".to_string(),
            "favorite_color".to_string(),
        ];
        let mapping = match client.interpret_frontmatter_schema(&keys).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("llama3.2 model unavailable ({e}) — skipping integration test");
                return;
            }
        };

        assert_eq!(mapping.len(), keys.len(), "expected one entry per key");
        assert_eq!(mapping.get("id").cloned().flatten().as_deref(), Some("id"));
        assert_eq!(
            mapping.get("depends_on").cloned().flatten().as_deref(),
            Some("depends_on")
        );
    }

    /// Real finding, verified empirically across a dozen+ runs: batching
    /// all keys into one classification call is order-sensitive for a
    /// small model. `depends_on`/`implements` classified correctly 100% of
    /// the time when they appeared last in the key list, and were
    /// consistently misclassified (as `causes`/`supersedes`) with the
    /// exact same prompt/definitions when they appeared first —
    /// `parse_frontmatter_keys` sorts alphabetically, which happens to put
    /// `depends_on` first for a typical SDD key set. This test locks in
    /// the fix (per-key classification, see `interpret_frontmatter_schema`)
    /// against the specific ordering that used to fail.
    #[tokio::test]
    async fn interpret_frontmatter_schema_is_order_independent() {
        let client = OllamaClient::new("llama3.2");
        if !client.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }
        let keys = vec![
            "depends_on".to_string(),
            "id".to_string(),
            "implements".to_string(),
            "kind".to_string(),
            "status".to_string(),
        ];
        let mapping = client.interpret_frontmatter_schema(&keys).await.unwrap();
        assert_eq!(
            mapping.get("depends_on").cloned().flatten().as_deref(),
            Some("depends_on")
        );
        assert_eq!(
            mapping.get("implements").cloned().flatten().as_deref(),
            Some("implements")
        );
    }
}
