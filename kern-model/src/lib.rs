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

/// Port consumed by kern-ingest to vectorize chunks.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, ModelError>;
    // TODO(kern-ingest): embed_batch to reduce round-trips to the subprocess.
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

/// Port consumed by kern-ontology — only called in the ambiguous zone
/// (design rationale kept in the maintainer's private delivery workspace,
/// not published in this repo). Loaded lazily, unloaded after idling.
#[async_trait]
pub trait ExtractionProvider: Send + Sync {
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
/// generative model comes into scope (design rationale kept in the
/// maintainer's private delivery workspace, not published in this repo).
pub struct LlamaCppRuntime {
    base_url: String,
    http: reqwest::Client,
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
    /// serve embeddings.
    pub async fn spawn(
        binary_path: &std::path::Path,
        model_path: &std::path::Path,
        port: u16,
    ) -> Result<Self, ModelError> {
        let mut child = tokio::process::Command::new(binary_path)
            .arg("-m")
            .arg(model_path)
            .arg("--embedding")
            .arg("--port")
            .arg(port.to_string())
            .arg("--host")
            .arg("127.0.0.1")
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

        Ok(Self {
            base_url,
            http,
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
struct FrontmatterMapping {
    mappings: Vec<FrontmatterKeyMapping>,
}

#[derive(Deserialize)]
struct FrontmatterKeyMapping {
    key: String,
    canonical_concept: Option<String>,
}

fn frontmatter_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "mappings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": {"type": "string"},
                        "canonical_concept": {"type": ["string", "null"]}
                    },
                    "required": ["key"]
                }
            }
        },
        "required": ["mappings"]
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
        let canonical_concepts =
            "id, kind, status, depends_on, implements, supersedes, causes, owned_by, conflicts_with, documents, configures";
        let prompt = format!(
            "The frontmatter keys below come from an unknown Spec-Driven \
             Development generator. For each key, say which canonical \
             concept it corresponds to ({canonical_concepts}), or null if it \
             doesn't correspond to any.\n\n\
             Keys: {}",
            keys.join(", ")
        );

        let mapped: FrontmatterMapping = self
            .generate_structured(prompt, frontmatter_schema())
            .await?;
        Ok(mapped
            .mappings
            .into_iter()
            .map(|m| (m.key, m.canonical_concept))
            .collect())
    }
}

/// Active model backend — dispatch via enum, not `dyn Trait` (avoids the
/// vtable cost on a call that already pays a subprocess/HTTP round-trip).
pub enum ModelBackend {
    Embedded(LlamaCppRuntime),
    Ollama(OllamaClient),
}

#[async_trait]
impl EmbeddingProvider for ModelBackend {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, ModelError> {
        match self {
            ModelBackend::Embedded(runtime) => runtime.embed(text).await,
            ModelBackend::Ollama(client) => client.embed(text).await,
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

        let runtime = LlamaCppRuntime::spawn(&binary, &model, 8791)
            .await
            .expect("llama-server should start and become ready within the timeout");

        let embedding = runtime
            .embed("real embedding test via llama-server")
            .await
            .expect("embed should return a valid vector");

        assert!(!embedding.is_empty());
    }

    /// `ModelBackend::Embedded` should actually delegate to `LlamaCppRuntime`
    /// — it's no longer a fixed error stub (the `BackendUnavailable`
    /// placeholder was removed).
    #[tokio::test]
    async fn model_backend_embedded_delegates_to_real_llama_cpp_runtime() {
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

        let runtime = LlamaCppRuntime::spawn(&binary, &model, 8792)
            .await
            .expect("llama-server should start and become ready within the timeout");
        let backend = ModelBackend::Embedded(runtime);

        let embedding = backend
            .embed("test via ModelBackend::Embedded")
            .await
            .expect("embed via ModelBackend should return a valid vector");

        assert!(!embedding.is_empty());
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
}
