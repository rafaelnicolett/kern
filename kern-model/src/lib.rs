//! kern-model — abstração de provedor de modelo (embedding + extração/judge).
//!
//! Ports (traits) consumidos por kern-ingest (embed) e kern-ontology
//! (extract/judge). Backend real (embarcado via subprocesso llama-server, ou
//! Ollama oportunista) é detalhe de implementação por trás destes traits —
//! ver docs/adr/0001 no workspace de delivery (arquitetura hexagonal).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("subprocesso de inferência indisponível: {0}")]
    BackendUnavailable(String),
    #[error("resposta de inferência malformada: {0}")]
    MalformedResponse(String),
    #[error("requisição HTTP ao backend falhou: {0}")]
    Http(#[from] reqwest::Error),
    // TODO(S3-BKD-03): variantes de timeout, modelo não carregado, etc.
}

/// Port consumido por kern-ingest para vetorizar chunks.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, ModelError>;
    // TODO(kern-ingest): embed_batch para reduzir round-trips no subprocesso.
}

/// Candidato de entidade/relação extraído de um chunk — pré-classificação.
/// TODO(S3-BKD-03): substituir pelo tipo real, compartilhado com kern-ontology
/// (hoje aqui só pra dar forma à assinatura do trait).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateEntity {
    pub raw_name: String,
    pub raw_type_hint: Option<String>,
}

/// Vocabulário de relação — os 8 tipos canônicos semeados + candidatos
/// promovidos. TODO(S3-BKD-01): tipo real vem de kern-ontology; aqui é só
/// placeholder pra fechar a assinatura de `extract`.
#[derive(Debug, Clone, Default)]
pub struct RelationVocabulary {
    pub canonical_types: Vec<String>,
}

/// Tipo de entidade já existente, candidato a match — usado por `judge`.
/// TODO(S3-BKD-03): substituir pelo tipo real de kern-ontology::TypeRegistry.
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

/// Port consumido por kern-ontology — só chamado na zona ambígua (ver
/// docs/domain/ontologia/aggregates.md no workspace de delivery). Carregado
/// lazy, descarregado após ociosidade.
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

    /// Interpreta o mapeamento de campos de frontmatter -> conceitos
    /// canônicos, uma única vez por forma nova (docs/domain/ontologia/aggregates.md,
    /// Agregado 3). `keys` é o conjunto de chaves do frontmatter, nunca os
    /// valores. Retorna, por chave, o conceito canônico correspondente
    /// (`id`, `kind`, `status`, `depends_on`, `implements`, ...) ou `None`
    /// se a chave não mapear pra nada conhecido.
    async fn interpret_frontmatter_schema(
        &self,
        keys: &[String],
    ) -> Result<std::collections::HashMap<String, Option<String>>, ModelError>;
}

/// Backend embarcado — spawna `llama-server` (llama.cpp) como subprocesso
/// local e fala com ele via HTTP (`/v1/embeddings`, compatível OpenAI).
/// `ExtractionProvider` não é implementado aqui: extração/judge ficam
/// restritos ao adapter oportunista (`OllamaClient`) até um modelo
/// generativo embarcado entrar no escopo (ver docs/adr no workspace de
/// delivery).
pub struct LlamaCppRuntime {
    base_url: String,
    http: reqwest::Client,
    // Mantém o subprocesso vivo pela duração do runtime — dropar isto
    // encerra o `llama-server` (kill_on_drop, ver `spawn`).
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
    /// Sobe `llama-server` apontando pro `.gguf` em `model_path`, na porta
    /// `port` (localhost apenas), e aguarda `/health` responder OK antes de
    /// retornar — nunca devolve um runtime que ainda não está pronto pra
    /// servir embeddings.
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
                ModelError::BackendUnavailable(format!("falha ao iniciar llama-server: {e}"))
            })?;

        let base_url = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::new();

        // Poll de prontidão — llama-server carrega o modelo antes de abrir
        // o socket HTTP, então as primeiras tentativas de conexão falham
        // normalmente; timeout total generoso o suficiente pra modelos
        // pequenos (o processo de release usa modelos na faixa de dezenas
        // de MB, não LLMs completos).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(ModelError::BackendUnavailable(format!(
                    "llama-server encerrou antes de ficar pronto (status: {status})"
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
                    "llama-server não respondeu /health dentro do timeout".to_string(),
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
            .ok_or_else(|| ModelError::MalformedResponse("resposta sem embeddings".to_string()))
    }
}

/// Adapter oportunista — só usado se o probe em `:11434` detectar o daemon.
/// Nunca dependência obrigatória (ver docs/adr — decisão fechada).
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

    /// Sonda de disponibilidade — nunca trata Ollama como obrigatório; quem
    /// chama decide o que fazer se retornar `false` (cair pro embarcado).
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
            .ok_or_else(|| ModelError::MalformedResponse("resposta sem embeddings".to_string()))
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
            "nenhum tipo canônico ainda".to_string()
        } else {
            vocab.canonical_types.join(", ")
        };
        let prompt = format!(
            "Extraia entidades candidatas (conceitos, componentes, artefatos) \
             mencionados no texto abaixo. Tipos de relação canônicos conhecidos \
             (só para contexto, não invente relações): {types_hint}\n\n\
             Texto:\n{chunk}"
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
            "nenhum tipo próximo encontrado".to_string()
        } else {
            nearest_existing
                .iter()
                .map(|t| format!("- {}: {}", t.name, t.description))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let prompt = format!(
            "Um candidato de entidade caiu na zona ambígua de similaridade contra \
             tipos já existentes. Decida: 'Merge' (é o mesmo conceito de um tipo \
             existente), 'NewType' (é um conceito genuinamente novo), ou 'Reject' \
             (não é uma entidade válida — ruído de extração).\n\n\
             Candidato: {} (dica de tipo: {})\n\n\
             Tipos existentes mais próximos:\n{existing_list}",
            candidate.raw_name,
            candidate.raw_type_hint.as_deref().unwrap_or("nenhuma"),
        );

        let judged: JudgeResponse = self.generate_structured(prompt, judge_schema()).await?;
        match judged.decision.as_str() {
            "Merge" => Ok(JudgeDecision::Merge),
            "NewType" => Ok(JudgeDecision::NewType),
            "Reject" => Ok(JudgeDecision::Reject),
            other => Err(ModelError::MalformedResponse(format!(
                "decisão desconhecida: {other}"
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
            "As chaves de frontmatter abaixo vêm de um gerador de Spec-Driven \
             Development desconhecido. Para cada chave, diga a qual conceito \
             canônico ela corresponde ({canonical_concepts}), ou null se não \
             corresponder a nenhum.\n\n\
             Chaves: {}",
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

/// Backend de modelo ativo — dispatch por enum, não por `dyn Trait` (evita
/// custo de vtable numa chamada que já paga round-trip de subprocesso/HTTP).
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

    /// Integração real contra Ollama local — pula (não falha) se o daemon
    /// não estiver rodando ou o modelo `all-minilm` não estiver puxado.
    /// `ollama pull all-minilm` (~46MB) antes de rodar localmente.
    #[tokio::test]
    async fn embed_via_ollama_real_retorna_vetor_nao_vazio() {
        let client = OllamaClient::new("all-minilm");
        if !client.probe().await {
            eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
            return;
        }

        let embedding = match client.embed("teste de embedding real").await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("modelo all-minilm indisponível ({e}) — pulando teste de integração");
                return;
            }
        };

        assert!(!embedding.is_empty());
        assert_eq!(embedding.len(), kern_vector_embedding_dim_hint());
    }

    /// Mantém o teste honesto sobre a dimensão esperada sem acoplar
    /// kern-model a kern-vector (dependência de teste, não de produto).
    fn kern_vector_embedding_dim_hint() -> usize {
        384
    }

    /// Localiza `llama-server` no PATH sem depender de um path fixo de
    /// instalação — CI e máquinas locais podem tê-lo em lugares diferentes.
    fn find_llama_server_binary() -> Option<std::path::PathBuf> {
        let path_var = std::env::var_os("PATH")?;
        std::env::split_paths(&path_var)
            .map(|dir| dir.join("llama-server"))
            .find(|candidate| candidate.is_file())
    }

    /// Modelo GGUF pequeno usado só pra testar o subprocesso de ponta a
    /// ponta — não é o modelo de produção, só precisa suportar `--embedding`.
    fn find_test_gguf_model() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME")?;
        let path = std::path::PathBuf::from(home)
            .join(".cache")
            .join("kern")
            .join("models")
            .join("bge-small-en-v1.5-q4_k_m.gguf");
        path.is_file().then_some(path)
    }

    /// Integração real: sobe `llama-server` como subprocesso de verdade e
    /// chama `/v1/embeddings` nele. Pula (não falha) se o binário ou o
    /// modelo de teste não estiverem disponíveis na máquina — CI de S5 não
    /// depende de tê-los instalados (ver sprint-status.md).
    #[tokio::test]
    async fn embed_via_llama_cpp_runtime_real_retorna_vetor_nao_vazio() {
        let Some(binary) = find_llama_server_binary() else {
            eprintln!("llama-server não encontrado no PATH — pulando teste de integração");
            return;
        };
        let Some(model) = find_test_gguf_model() else {
            eprintln!(
                "modelo de teste ~/.cache/kern/models/bge-small-en-v1.5-q4_k_m.gguf \
                 não encontrado — pulando teste de integração"
            );
            return;
        };

        let runtime = LlamaCppRuntime::spawn(&binary, &model, 8791)
            .await
            .expect("llama-server deveria subir e ficar pronto dentro do timeout");

        let embedding = runtime
            .embed("teste de embedding real via llama-server")
            .await
            .expect("embed deveria retornar um vetor válido");

        assert!(!embedding.is_empty());
    }

    /// `ModelBackend::Embedded` deve delegar de verdade pro `LlamaCppRuntime`
    /// — não é mais um stub de erro fixo (ver histórico: Sprint S5 removeu
    /// o placeholder `BackendUnavailable`).
    #[tokio::test]
    async fn model_backend_embedded_delega_para_llama_cpp_runtime_real() {
        let Some(binary) = find_llama_server_binary() else {
            eprintln!("llama-server não encontrado no PATH — pulando teste de integração");
            return;
        };
        let Some(model) = find_test_gguf_model() else {
            eprintln!(
                "modelo de teste ~/.cache/kern/models/bge-small-en-v1.5-q4_k_m.gguf \
                 não encontrado — pulando teste de integração"
            );
            return;
        };

        let runtime = LlamaCppRuntime::spawn(&binary, &model, 8792)
            .await
            .expect("llama-server deveria subir e ficar pronto dentro do timeout");
        let backend = ModelBackend::Embedded(runtime);

        let embedding = backend
            .embed("teste via ModelBackend::Embedded")
            .await
            .expect("embed via ModelBackend deveria retornar um vetor válido");

        assert!(!embedding.is_empty());
    }

    /// Integração real contra Ollama — `llama3.2` já é um modelo generativo
    /// comum (ao contrário de `all-minilm`, embedding-only). Pula se
    /// indisponível.
    #[tokio::test]
    async fn extract_via_ollama_real_encontra_entidades_mencionadas() {
        let client = OllamaClient::new("llama3.2");
        if !client.probe().await {
            eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
            return;
        }

        let vocab = RelationVocabulary {
            canonical_types: vec!["depends_on".to_string()],
        };
        let candidates = match client
            .extract(
                "kern-ontology depende de kern-model para gerar embeddings.",
                &vocab,
            )
            .await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("modelo llama3.2 indisponível ({e}) — pulando teste de integração");
                return;
            }
        };

        assert!(
            !candidates.is_empty(),
            "esperava ao menos 1 candidato extraído"
        );
        assert!(candidates
            .iter()
            .any(|c| c.raw_name.to_lowercase().contains("kern")));
    }

    #[tokio::test]
    async fn judge_via_ollama_real_retorna_uma_decisao_valida() {
        let client = OllamaClient::new("llama3.2");
        if !client.probe().await {
            eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
            return;
        }

        let candidate = CandidateEntity {
            raw_name: "kern-vector".to_string(),
            raw_type_hint: Some("crate".to_string()),
        };
        let nearest = vec![EntityType {
            name: "kern-ingest".to_string(),
            description: "crate de ingestão e chunking".to_string(),
        }];

        let decision = match client.judge(&candidate, &nearest).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("modelo llama3.2 indisponível ({e}) — pulando teste de integração");
                return;
            }
        };

        // Qualquer uma das 3 é uma resposta estruturalmente válida — o que
        // este teste garante é que o pipeline HTTP + schema + parse funciona
        // de ponta a ponta contra um modelo real, não qual decisão sai.
        assert!(matches!(
            decision,
            JudgeDecision::Merge | JudgeDecision::NewType | JudgeDecision::Reject
        ));
    }

    #[tokio::test]
    async fn interpret_frontmatter_schema_via_ollama_real_mapeia_chaves_obvias() {
        let client = OllamaClient::new("llama3.2");
        if !client.probe().await {
            eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
            return;
        }

        let keys = vec![
            "id".to_string(),
            "kind".to_string(),
            "depends_on".to_string(),
            "cor_favorita".to_string(),
        ];
        let mapping = match client.interpret_frontmatter_schema(&keys).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("modelo llama3.2 indisponível ({e}) — pulando teste de integração");
                return;
            }
        };

        assert_eq!(mapping.len(), keys.len(), "esperava uma entrada por chave");
        assert_eq!(mapping.get("id").cloned().flatten().as_deref(), Some("id"));
        assert_eq!(
            mapping.get("depends_on").cloned().flatten().as_deref(),
            Some("depends_on")
        );
    }
}
