//! kern-ontology — type registry, vocabulário de relação, motor de diff
//! incremental. O core subdomain do projeto (ver docs/domain/ontologia/ no
//! workspace de delivery) — é o que a taxa de fallback do `judge()` mede.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

mod frontmatter;
mod metrics;
mod sqlite;

pub use frontmatter::{
    folder_scope, key_fingerprint, parse_frontmatter_keys, FrontmatterProfile,
    FrontmatterProfileRepository,
};
pub use metrics::FallbackMetrics;
pub use sqlite::{
    SqliteFrontmatterProfileRepository, SqliteInstanceRepository, SqliteTypeRepository,
};

// kern-model é dependência declarada no Cargo.toml deste crate — usada a
// partir de S3-BKD-04 (CandidateEntity, EntityType, JudgeDecision nas
// assinaturas de OntologyEngine::process_candidate).

#[derive(Debug, Error)]
pub enum OntologyError {
    #[error("entidade não encontrada: {0}")]
    EntityNotFound(Uuid),
    #[error("tipo de relação não encontrado: {0}")]
    RelationTypeNotFound(String),
    #[error("candidato na zona ambígua sem decisão do judge")]
    JudgeUndecided,
    #[error("erro do provedor de modelo: {0}")]
    Model(#[from] kern_model::ModelError),
    #[error("erro do SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("falha ao abrir banco em {path}: {reason}")]
    OpenFailed { path: String, reason: String },
    #[error("tarefa de banco cancelada/pane: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),
}

/// Status de um `RelationType` — ver Agregado 1 em
/// docs/domain/ontologia/aggregates.md (workspace de delivery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationTypeStatus {
    Candidate,
    Canonical,
}

/// Threshold de N hits independentes até promoção a canônico — fechado em 3
/// (docs/domain/ontologia/aggregates.md).
pub const PROMOTION_THRESHOLD: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityTypeRecord {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub instance_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationTypeRecord {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub status: RelationTypeStatus,
    pub independent_hits: u32,
    pub instance_count: u64,
}

/// Os 8 tipos canônicos semeados no boot do projeto — não passam por
/// promoção (docs/domain/ontologia/aggregates.md, Agregado 1, invariante 3).
pub const SEED_RELATION_TYPES: [&str; 8] = [
    "depends_on",
    "supersedes",
    "implements",
    "causes",
    "owned_by",
    "conflicts_with",
    "documents",
    "configures",
];

/// Port: persistência do Agregado 1 (Registro de Tipos). Adapter real
/// (`SqliteTypeRepository`) fica atrás deste trait — ver docs/adr/0004.
#[async_trait]
pub trait TypeRepository: Send + Sync {
    async fn seed_canonical_vocabulary(&self) -> Result<(), OntologyError>;
    /// Tipos de entidade não passam pelo fluxo de promoção curada dos tipos
    /// de relação (docs/domain/ontologia/aggregates.md não define threshold
    /// pra entity types) — get-or-create simples.
    async fn find_or_create_entity_type(
        &self,
        name: &str,
        description: &str,
    ) -> Result<EntityTypeRecord, OntologyError>;
    async fn find_relation_type(
        &self,
        name: &str,
    ) -> Result<Option<RelationTypeRecord>, OntologyError>;
    /// Get-or-create: se `name` já existe (candidate ou canonical), retorna
    /// o registro existente sem duplicar.
    async fn register_candidate_type(
        &self,
        name: &str,
        description: &str,
    ) -> Result<RelationTypeRecord, OntologyError>;
    /// Registra que `type_id` apareceu em `file_path` — deduplicado por
    /// arquivo (hits de arquivos diferentes, nunca o mesmo arquivo contado
    /// duas vezes). Retorna a contagem de arquivos independentes após o
    /// registro.
    async fn record_independent_hit(
        &self,
        type_id: Uuid,
        file_path: &str,
    ) -> Result<u32, OntologyError>;
    async fn promote_to_canonical(&self, type_id: Uuid) -> Result<(), OntologyError>;
    /// Usado por `get_ontology_schema` (kern-mcp).
    async fn list_entity_types(&self) -> Result<Vec<EntityTypeRecord>, OntologyError>;
    /// Usado por `get_ontology_schema` (kern-mcp) e pelo roteamento semântico
    /// de `query_ontological`.
    async fn list_relation_types(&self) -> Result<Vec<RelationTypeRecord>, OntologyError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRecord {
    pub id: Uuid,
    pub type_id: Uuid,
    pub canonical_name: String,
    /// Imutável após a criação (docs/domain/ontologia/aggregates.md, Agregado 2, invariante 2).
    pub first_seen_file: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationRecord {
    pub id: Uuid,
    pub type_id: Uuid,
    pub source_entity_id: Uuid,
    pub target_entity_id: Uuid,
    pub confidence: f32,
    /// Nunca nulo — toda relação cita evidência (docs/domain/ontologia/aggregates.md, Agregado 2, invariante 3).
    pub evidence_chunk_id: Uuid,
}

/// Port: persistência do Agregado 2 (Grafo de Instâncias).
#[async_trait]
pub trait InstanceRepository: Send + Sync {
    async fn find_or_create_entity(
        &self,
        type_id: Uuid,
        canonical_name: &str,
        first_seen_file: &str,
    ) -> Result<EntityRecord, OntologyError>;
    async fn record_relation(&self, relation: RelationRecord) -> Result<(), OntologyError>;
    async fn related_entities(
        &self,
        entity_id: Uuid,
        depth: u32,
    ) -> Result<Vec<EntityRecord>, OntologyError>;
    /// Usado por `query_by_concept` (kern-mcp) — busca por substring
    /// case-insensitive no nome canônico, não match exato só.
    async fn find_entities_by_name(
        &self,
        name_query: &str,
    ) -> Result<Vec<EntityRecord>, OntologyError>;
    /// Usado por `explain_relation` (kern-mcp) — caminho mais curto (BFS)
    /// entre duas entidades, com as relações percorridas em ordem. `None`
    /// se não houver caminho até `max_depth` saltos.
    async fn find_path(
        &self,
        from: Uuid,
        to: Uuid,
        max_depth: u32,
    ) -> Result<Option<Vec<RelationRecord>>, OntologyError>;
    /// Usado por `query_by_concept` (kern-mcp) — as arestas onde a entidade
    /// é origem ou alvo, não só os vizinhos resultantes.
    async fn direct_relations(&self, entity_id: Uuid)
        -> Result<Vec<RelationRecord>, OntologyError>;
}

/// Decisão do motor de ontologia pra um candidato — ver
/// docs/domain/ontologia/event-storming.md, passo 3.
#[derive(Debug, Clone)]
pub enum ClassificationOutcome {
    Merged {
        judge_called: bool,
        entity: EntityRecord,
    },
    NewType {
        judge_called: bool,
        entity: EntityRecord,
        entity_type: EntityTypeRecord,
    },
    Rejected {
        reason: String,
    },
}

/// Threshold da zona ambígua — placeholder documentado, não-final (ver
/// docs/domain/ontologia/aggregates.md e docs/adr/0004 no workspace de
/// delivery). Configurável via TOML na implementação real.
#[derive(Debug, Clone, Copy)]
pub struct AmbiguousZoneConfig {
    pub low_distance_max: f32,
    pub high_distance_min: f32,
}

impl Default for AmbiguousZoneConfig {
    fn default() -> Self {
        Self {
            low_distance_max: 0.15,
            high_distance_min: 0.35,
        }
    }
}

/// Motor de decisão real — recebe um candidato + a distância de embedding
/// contra o tipo mais próximo já existente, decide merge/novo-tipo/judge sem
/// chamar `ExtractionProvider::judge()` fora da zona ambígua (é o que
/// mantém a taxa de fallback baixa — KPI North Star do projeto).
pub struct OntologyEngine {
    types: std::sync::Arc<dyn TypeRepository>,
    instances: std::sync::Arc<dyn InstanceRepository>,
    extraction: std::sync::Arc<dyn kern_model::ExtractionProvider>,
    frontmatter_profiles: std::sync::Arc<dyn FrontmatterProfileRepository>,
    zone: AmbiguousZoneConfig,
    /// Taxa de fallback para o judge() — KPI North Star (docs/adr/0005).
    pub metrics: std::sync::Arc<FallbackMetrics>,
}

impl OntologyEngine {
    pub fn new(
        types: std::sync::Arc<dyn TypeRepository>,
        instances: std::sync::Arc<dyn InstanceRepository>,
        extraction: std::sync::Arc<dyn kern_model::ExtractionProvider>,
        frontmatter_profiles: std::sync::Arc<dyn FrontmatterProfileRepository>,
        zone: AmbiguousZoneConfig,
    ) -> Self {
        Self {
            types,
            instances,
            extraction,
            frontmatter_profiles,
            zone,
            metrics: std::sync::Arc::new(FallbackMetrics::new()),
        }
    }

    /// Ver docs/domain/ontologia/event-storming.md, passo 3:
    /// - distância baixa -> funde direto (sem judge)
    /// - distância alta -> cria tipo novo direto (sem judge)
    /// - zona ambígua -> chama judge() (único caminho caro)
    ///
    /// Um span por caso de uso (docs/adr/0005) — `bc` identifica a Bounded
    /// Context pra correlação em rastros distribuídos.
    #[tracing::instrument(skip(self, candidate), fields(bc = "ontologia", candidate = %candidate.raw_name))]
    pub async fn process_candidate(
        &self,
        candidate: kern_model::CandidateEntity,
        distance_to_nearest: f32,
        nearest_type: Option<&EntityTypeRecord>,
        file_path: &str,
    ) -> Result<ClassificationOutcome, OntologyError> {
        if distance_to_nearest <= self.zone.low_distance_max {
            let nearest = nearest_type.ok_or_else(|| {
                OntologyError::RelationTypeNotFound(
                    "distância baixa reportada sem tipo mais próximo".to_string(),
                )
            })?;
            let entity = self
                .instances
                .find_or_create_entity(nearest.id, &candidate.raw_name, file_path)
                .await?;
            self.metrics.record(false);
            return Ok(ClassificationOutcome::Merged {
                judge_called: false,
                entity,
            });
        }

        if distance_to_nearest >= self.zone.high_distance_min {
            let outcome = self.create_new_type(&candidate, file_path).await;
            self.metrics.record(false);
            return outcome;
        }

        // Zona ambígua — único caminho que paga o custo do judge().
        let nearest_hint = nearest_type
            .map(|t| {
                vec![kern_model::EntityType {
                    name: t.name.clone(),
                    description: t.description.clone(),
                }]
            })
            .unwrap_or_default();

        let outcome = match self.extraction.judge(&candidate, &nearest_hint).await? {
            kern_model::JudgeDecision::Merge => {
                let nearest = nearest_type.ok_or(OntologyError::JudgeUndecided)?;
                let entity = self
                    .instances
                    .find_or_create_entity(nearest.id, &candidate.raw_name, file_path)
                    .await?;
                Ok(ClassificationOutcome::Merged {
                    judge_called: true,
                    entity,
                })
            }
            kern_model::JudgeDecision::NewType => {
                match self.create_new_type(&candidate, file_path).await? {
                    ClassificationOutcome::NewType {
                        entity,
                        entity_type,
                        ..
                    } => Ok(ClassificationOutcome::NewType {
                        judge_called: true,
                        entity,
                        entity_type,
                    }),
                    other => Ok(other),
                }
            }
            kern_model::JudgeDecision::Reject => Ok(ClassificationOutcome::Rejected {
                reason: format!("judge() rejeitou candidato '{}'", candidate.raw_name),
            }),
        };
        self.metrics.record(true);
        outcome
    }

    async fn create_new_type(
        &self,
        candidate: &kern_model::CandidateEntity,
        file_path: &str,
    ) -> Result<ClassificationOutcome, OntologyError> {
        let type_name = candidate
            .raw_type_hint
            .clone()
            .unwrap_or_else(|| candidate.raw_name.clone());
        let entity_type = self
            .types
            .find_or_create_entity_type(
                &type_name,
                &format!("tipo inferido a partir de '{}'", candidate.raw_name),
            )
            .await?;
        let entity = self
            .instances
            .find_or_create_entity(entity_type.id, &candidate.raw_name, file_path)
            .await?;
        Ok(ClassificationOutcome::NewType {
            judge_called: false,
            entity,
            entity_type,
        })
    }

    /// Registra que `relation_type_name` apareceu em `file_path` e promove a
    /// canônico se atingir `PROMOTION_THRESHOLD` arquivos independentes. Get-
    /// or-create do tipo candidato: chamar isso pra um tipo que não existe
    /// ainda o cria como candidate.
    pub async fn evaluate_promotion(
        &self,
        relation_type_name: &str,
        description: &str,
        file_path: &str,
    ) -> Result<RelationTypeRecord, OntologyError> {
        let record = self
            .types
            .register_candidate_type(relation_type_name, description)
            .await?;
        if record.status == RelationTypeStatus::Canonical {
            return Ok(record);
        }

        let hits = self
            .types
            .record_independent_hit(record.id, file_path)
            .await?;
        if hits >= PROMOTION_THRESHOLD {
            self.types.promote_to_canonical(record.id).await?;
            return self
                .types
                .find_relation_type(relation_type_name)
                .await?
                .ok_or_else(|| {
                    OntologyError::RelationTypeNotFound(relation_type_name.to_string())
                });
        }
        Ok(record)
    }

    /// Fluxo de duas etapas do frontmatter (docs/domain/ontologia — seção
    /// "Dando Sentido à Ontologia"): parse determinístico primeiro, LLM só
    /// na primeira ocorrência de uma forma nova. `None` se o arquivo não tem
    /// frontmatter — cai pro fallback de extração via prosa (fora deste
    /// método).
    pub async fn learn_or_reuse_frontmatter_profile(
        &self,
        file_path: &std::path::Path,
        content: &str,
    ) -> Result<Option<FrontmatterProfile>, OntologyError> {
        let Some(keys) = parse_frontmatter_keys(content) else {
            return Ok(None);
        };
        let scope = folder_scope(file_path);
        let fingerprint = key_fingerprint(&keys);

        if let Some(cached) = self.frontmatter_profiles.find(&scope, &fingerprint).await? {
            return Ok(Some(cached));
        }

        // Primeira ocorrência desta forma neste escopo — única chamada cara.
        let field_mapping = self.extraction.interpret_frontmatter_schema(&keys).await?;
        let profile = FrontmatterProfile {
            id: Uuid::new_v4(),
            folder_scope: scope,
            key_fingerprint: fingerprint,
            field_mapping,
            learned_at: sqlite::now(),
        };
        self.frontmatter_profiles.save(profile.clone()).await?;
        Ok(Some(profile))
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use kern_model::{CandidateEntity, OllamaClient};
    use std::sync::Arc;

    fn engine(dir: &std::path::Path) -> OntologyEngine {
        let db_path = dir.join("registry.db");
        let types: Arc<dyn TypeRepository> =
            Arc::new(SqliteTypeRepository::open(&db_path).unwrap());
        let instances: Arc<dyn InstanceRepository> =
            Arc::new(SqliteInstanceRepository::open(&db_path).unwrap());
        let extraction: Arc<dyn kern_model::ExtractionProvider> =
            Arc::new(OllamaClient::new("llama3.2"));
        let frontmatter_profiles: Arc<dyn FrontmatterProfileRepository> =
            Arc::new(SqliteFrontmatterProfileRepository::open(&db_path).unwrap());
        OntologyEngine::new(
            types,
            instances,
            extraction,
            frontmatter_profiles,
            AmbiguousZoneConfig::default(),
        )
    }

    #[tokio::test]
    async fn distancia_baixa_funde_sem_chamar_judge() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let existing_type = engine
            .types
            .find_or_create_entity_type("Crate", "um crate do workspace kern")
            .await
            .unwrap();

        let candidate = CandidateEntity {
            raw_name: "kern-ontology".to_string(),
            raw_type_hint: Some("Crate".to_string()),
        };

        let outcome = engine
            .process_candidate(candidate, 0.05, Some(&existing_type), "docs/a.md")
            .await
            .unwrap();

        match outcome {
            ClassificationOutcome::Merged {
                judge_called,
                entity,
            } => {
                assert!(!judge_called, "distância baixa não deveria chamar judge()");
                assert_eq!(entity.type_id, existing_type.id);
            }
            other => panic!("esperava Merged, veio {other:?}"),
        }
    }

    #[tokio::test]
    async fn distancia_alta_cria_tipo_novo_sem_chamar_judge() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let candidate = CandidateEntity {
            raw_name: "kern-mcp".to_string(),
            raw_type_hint: Some("Crate".to_string()),
        };

        let outcome = engine
            .process_candidate(candidate, 0.9, None, "docs/b.md")
            .await
            .unwrap();

        match outcome {
            ClassificationOutcome::NewType {
                judge_called,
                entity_type,
                ..
            } => {
                assert!(!judge_called, "distância alta não deveria chamar judge()");
                assert_eq!(entity_type.name, "Crate");
            }
            other => panic!("esperava NewType, veio {other:?}"),
        }
    }

    /// Integração real contra Ollama (llama3.2) — pula se indisponível.
    #[tokio::test]
    async fn zona_ambigua_chama_judge_e_aplica_a_decisao() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let existing_type = engine
            .types
            .find_or_create_entity_type("Crate", "um crate Rust do workspace")
            .await
            .unwrap();

        let candidate = CandidateEntity {
            raw_name: "kern-cli".to_string(),
            raw_type_hint: Some("Crate".to_string()),
        };

        // Distância no meio da zona ambígua (default: 0.15..0.35).
        let outcome = engine
            .process_candidate(candidate, 0.25, Some(&existing_type), "docs/c.md")
            .await
            .unwrap();

        let judge_was_called = matches!(
            outcome,
            ClassificationOutcome::Merged {
                judge_called: true,
                ..
            } | ClassificationOutcome::NewType {
                judge_called: true,
                ..
            } | ClassificationOutcome::Rejected { .. }
        );
        assert!(
            judge_was_called,
            "zona ambígua deveria ter chamado judge(): {outcome:?}"
        );
    }

    #[tokio::test]
    async fn evaluate_promotion_promove_apos_threshold_via_engine() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let mut last = None;
        for i in 0..PROMOTION_THRESHOLD {
            last = Some(
                engine
                    .evaluate_promotion("influences", "tipo emergente", &format!("doc-{i}.md"))
                    .await
                    .unwrap(),
            );
        }

        assert_eq!(last.unwrap().status, RelationTypeStatus::Canonical);
    }

    #[tokio::test]
    async fn evaluate_promotion_nao_promove_antes_do_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let record = engine
            .evaluate_promotion("influences", "tipo emergente", "doc-0.md")
            .await
            .unwrap();

        assert_eq!(record.status, RelationTypeStatus::Candidate);
    }

    #[tokio::test]
    async fn arquivo_sem_frontmatter_retorna_none() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let result = engine
            .learn_or_reuse_frontmatter_profile(
                std::path::Path::new("docs/livre.md"),
                "# Sem frontmatter\nsó prosa.\n",
            )
            .await
            .unwrap();

        assert!(result.is_none());
    }

    /// Integração real contra Ollama — primeira chamada aprende (chama o
    /// extrator), segunda chamada com a MESMA forma reusa o cache sem
    /// chamar o extrator de novo (verificado indiretamente: o id do perfil
    /// retornado é idêntico nas duas chamadas).
    #[tokio::test]
    async fn segunda_ocorrencia_da_mesma_forma_reusa_o_perfil_cacheado() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());
        let content = "---\nid: TASK-0001\nkind: task\ndepends_on: [TASK-0000]\n---\n\n# Tarefa\n";

        let first = engine
            .learn_or_reuse_frontmatter_profile(
                std::path::Path::new(".specify/specs/a.md"),
                content,
            )
            .await
            .unwrap()
            .expect("deveria aprender um perfil na primeira ocorrência");

        let second = engine
            .learn_or_reuse_frontmatter_profile(
                std::path::Path::new(".specify/specs/b.md"), // arquivo diferente, mesma forma
                content,
            )
            .await
            .unwrap()
            .expect("deveria reusar o perfil cacheado na segunda ocorrência");

        assert_eq!(
            first.id, second.id,
            "mesma forma no mesmo escopo deveria reusar o perfil, não aprender de novo"
        );
        assert_eq!(
            first.field_mapping.get("id").cloned().flatten().as_deref(),
            Some("id")
        );
    }

    #[tokio::test]
    async fn metrica_de_fallback_reflete_chamadas_reais_ao_judge() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());
        assert_eq!(engine.metrics.fallback_rate(), 0.0);

        let existing_type = engine
            .types
            .find_or_create_entity_type("Crate", "um crate")
            .await
            .unwrap();

        // 2 candidatos fora da zona ambígua — sem custo de judge().
        for name in ["kern-a", "kern-b"] {
            engine
                .process_candidate(
                    kern_model::CandidateEntity {
                        raw_name: name.to_string(),
                        raw_type_hint: Some("Crate".to_string()),
                    },
                    0.05, // distância baixa
                    Some(&existing_type),
                    "docs/x.md",
                )
                .await
                .unwrap();
        }

        assert_eq!(engine.metrics.total(), 2);
        assert_eq!(engine.metrics.fallback_total(), 0);
        assert_eq!(engine.metrics.fallback_rate(), 0.0);
    }
}
