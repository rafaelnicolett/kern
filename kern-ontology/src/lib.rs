//! kern-ontology — type registry, relation vocabulary, incremental diff
//! engine. The core subdomain of the project (design rationale kept in the
//! maintainer's private delivery workspace, not published in this repo) —
//! this is what the fallback rate of `judge()` measures.

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

// kern-model is a dependency declared in this crate's Cargo.toml — used
// for CandidateEntity, EntityType, JudgeDecision in the signatures of
// OntologyEngine::process_candidate.

#[derive(Debug, Error)]
pub enum OntologyError {
    #[error("entity not found: {0}")]
    EntityNotFound(Uuid),
    #[error("relation type not found: {0}")]
    RelationTypeNotFound(String),
    #[error("candidate in the ambiguous zone without a judge decision")]
    JudgeUndecided,
    #[error("model provider error: {0}")]
    Model(#[from] kern_model::ModelError),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("failed to open database at {path}: {reason}")]
    OpenFailed { path: String, reason: String },
    #[error("database task cancelled/panicked: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),
}

/// Status of a `RelationType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationTypeStatus {
    Candidate,
    Canonical,
}

/// Threshold of independent-hit count required before promotion to
/// canonical — fixed at 3.
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

/// The 8 canonical types seeded at project boot — they never go through
/// promotion.
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

/// Port: persistence for the Type Registry aggregate. The real adapter
/// (`SqliteTypeRepository`) sits behind this trait.
#[async_trait]
pub trait TypeRepository: Send + Sync {
    async fn seed_canonical_vocabulary(&self) -> Result<(), OntologyError>;
    /// Entity types don't go through the curated promotion flow that
    /// relation types do — simple get-or-create.
    async fn find_or_create_entity_type(
        &self,
        name: &str,
        description: &str,
    ) -> Result<EntityTypeRecord, OntologyError>;
    async fn find_relation_type(
        &self,
        name: &str,
    ) -> Result<Option<RelationTypeRecord>, OntologyError>;
    /// Get-or-create: if `name` already exists (candidate or canonical),
    /// returns the existing record without duplicating it.
    async fn register_candidate_type(
        &self,
        name: &str,
        description: &str,
    ) -> Result<RelationTypeRecord, OntologyError>;
    /// Records that `type_id` appeared in `file_path` — deduplicated by
    /// file (hits from different files, never the same file counted
    /// twice). Returns the count of independent files after recording.
    async fn record_independent_hit(
        &self,
        type_id: Uuid,
        file_path: &str,
    ) -> Result<u32, OntologyError>;
    async fn promote_to_canonical(&self, type_id: Uuid) -> Result<(), OntologyError>;
    /// Used by `get_ontology_schema` (kern-mcp).
    async fn list_entity_types(&self) -> Result<Vec<EntityTypeRecord>, OntologyError>;
    /// Used by `get_ontology_schema` (kern-mcp) and by the semantic
    /// routing of `query_ontological`.
    async fn list_relation_types(&self) -> Result<Vec<RelationTypeRecord>, OntologyError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRecord {
    pub id: Uuid,
    pub type_id: Uuid,
    pub canonical_name: String,
    /// Immutable after creation.
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
    /// Never null — every relation cites evidence.
    pub evidence_chunk_id: Uuid,
}

/// Port: persistence for the Instance Graph aggregate.
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
    /// Used by `query_by_concept` (kern-mcp) — case-insensitive substring
    /// search on the canonical name, not just an exact match.
    async fn find_entities_by_name(
        &self,
        name_query: &str,
    ) -> Result<Vec<EntityRecord>, OntologyError>;
    /// Used by `explain_relation` (kern-mcp) — shortest path (BFS)
    /// between two entities, with the traversed relations in order.
    /// `None` if there is no path within `max_depth` hops.
    async fn find_path(
        &self,
        from: Uuid,
        to: Uuid,
        max_depth: u32,
    ) -> Result<Option<Vec<RelationRecord>>, OntologyError>;
    /// Used by `query_by_concept` (kern-mcp) — the edges where the entity
    /// is either source or target, not just the resulting neighbors.
    async fn direct_relations(&self, entity_id: Uuid)
        -> Result<Vec<RelationRecord>, OntologyError>;
}

/// Decision made by the ontology engine for a candidate.
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

/// Ambiguous zone threshold — documented placeholder, not final.
/// Configurable via TOML in the real implementation.
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

/// The real decision engine — takes a candidate + the embedding distance
/// against the closest existing type, decides merge/new-type/judge without
/// calling `ExtractionProvider::judge()` outside the ambiguous zone (this
/// is what keeps the fallback rate low — the project's North Star KPI).
pub struct OntologyEngine {
    types: std::sync::Arc<dyn TypeRepository>,
    instances: std::sync::Arc<dyn InstanceRepository>,
    extraction: std::sync::Arc<dyn kern_model::ExtractionProvider>,
    frontmatter_profiles: std::sync::Arc<dyn FrontmatterProfileRepository>,
    /// Used only to find the closest entity type for a candidate
    /// (`nearest_entity_type`) — never to embed chunks, that's the
    /// responsibility of kern-ingest/kern-vector.
    embedder: std::sync::Arc<dyn kern_model::EmbeddingProvider>,
    zone: AmbiguousZoneConfig,
    /// Fallback rate for judge() — North Star KPI.
    pub metrics: std::sync::Arc<FallbackMetrics>,
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

impl OntologyEngine {
    pub fn new(
        types: std::sync::Arc<dyn TypeRepository>,
        instances: std::sync::Arc<dyn InstanceRepository>,
        extraction: std::sync::Arc<dyn kern_model::ExtractionProvider>,
        frontmatter_profiles: std::sync::Arc<dyn FrontmatterProfileRepository>,
        embedder: std::sync::Arc<dyn kern_model::EmbeddingProvider>,
        zone: AmbiguousZoneConfig,
    ) -> Self {
        Self {
            types,
            instances,
            extraction,
            frontmatter_profiles,
            embedder,
            zone,
            metrics: std::sync::Arc::new(FallbackMetrics::new()),
        }
    }

    /// Real entry point of ingestion: extracts candidates from the chunk,
    /// finds the distance of each one against the closest already-known
    /// entity type, and applies the decision via `process_candidate`.
    /// Prose-based extraction only — the frontmatter-driven path
    /// (`learn_or_reuse_frontmatter_profile`) is a separate dimension,
    /// not yet wired into this method.
    pub async fn process_chunk(
        &self,
        chunk_content: &str,
        file_path: &str,
    ) -> Result<Vec<ClassificationOutcome>, OntologyError> {
        let vocab = kern_model::RelationVocabulary {
            canonical_types: self
                .types
                .list_relation_types()
                .await?
                .into_iter()
                .map(|t| t.name)
                .collect(),
        };
        let candidates = self.extraction.extract(chunk_content, &vocab).await?;

        let mut outcomes = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let (distance, nearest) = self.nearest_entity_type(&candidate).await?;
            let outcome = self
                .process_candidate(candidate, distance, nearest.as_ref(), file_path)
                .await?;
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }

    /// Embedding distance between the candidate and the closest existing
    /// entity type — input for `process_candidate`. With no existing
    /// types yet, returns the maximum distance (forces creation of a new
    /// type, never a merge with nothing to compare against).
    async fn nearest_entity_type(
        &self,
        candidate: &kern_model::CandidateEntity,
    ) -> Result<(f32, Option<EntityTypeRecord>), OntologyError> {
        let entity_types = self.types.list_entity_types().await?;
        if entity_types.is_empty() {
            return Ok((1.0, None));
        }

        let probe_text = candidate
            .raw_type_hint
            .as_deref()
            .unwrap_or(&candidate.raw_name);
        let candidate_embedding = self.embedder.embed(probe_text).await?;

        let mut best: Option<(EntityTypeRecord, f32)> = None;
        for entity_type in entity_types {
            let desc_embedding = self.embedder.embed(&entity_type.description).await?;
            let distance = 1.0 - cosine_similarity(&candidate_embedding, &desc_embedding);
            if best.as_ref().map(|(_, d)| distance < *d).unwrap_or(true) {
                best = Some((entity_type, distance));
            }
        }
        let (entity_type, distance) = best.expect("entity_types is not empty in this branch");
        Ok((distance, Some(entity_type)))
    }

    /// Decision logic:
    /// - low distance -> merges directly (no judge)
    /// - high distance -> creates a new type directly (no judge)
    /// - ambiguous zone -> calls judge() (the only costly path)
    ///
    /// One span per use case — `bc` identifies the Bounded Context for
    /// correlation across distributed traces.
    #[tracing::instrument(skip(self, candidate), fields(bc = "ontology", candidate = %candidate.raw_name))]
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
                    "low distance reported without a nearest type".to_string(),
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

        // Ambiguous zone — the only path that pays the cost of judge().
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
                reason: format!("judge() rejected candidate '{}'", candidate.raw_name),
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
                &format!("type inferred from '{}'", candidate.raw_name),
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

    /// Registers that `relation_type_name` appeared in `file_path` and
    /// promotes it to canonical once it reaches `PROMOTION_THRESHOLD`
    /// independent files. Get-or-create for the candidate type: calling
    /// this for a type that doesn't exist yet creates it as a candidate.
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

    /// Two-step frontmatter flow: deterministic parse first, LLM only on
    /// the first occurrence of a new shape. `None` if the file has no
    /// frontmatter — falls back to prose-based extraction (outside this
    /// method).
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

        // First occurrence of this shape in this scope — the only expensive call.
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
        let embedder: Arc<dyn kern_model::EmbeddingProvider> =
            Arc::new(OllamaClient::new("all-minilm"));
        OntologyEngine::new(
            types,
            instances,
            extraction,
            frontmatter_profiles,
            embedder,
            AmbiguousZoneConfig::default(),
        )
    }

    #[tokio::test]
    async fn low_distance_merges_without_calling_judge() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let existing_type = engine
            .types
            .find_or_create_entity_type("Crate", "a crate from the kern workspace")
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
                assert!(!judge_called, "low distance shouldn't call judge()");
                assert_eq!(entity.type_id, existing_type.id);
            }
            other => panic!("expected Merged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn high_distance_creates_new_type_without_calling_judge() {
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
                assert!(!judge_called, "high distance shouldn't call judge()");
                assert_eq!(entity_type.name, "Crate");
            }
            other => panic!("expected NewType, got {other:?}"),
        }
    }

    /// Real integration against Ollama (llama3.2) — skips if unavailable.
    #[tokio::test]
    async fn ambiguous_zone_calls_judge_and_applies_the_decision() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let existing_type = engine
            .types
            .find_or_create_entity_type("Crate", "a Rust crate from the workspace")
            .await
            .unwrap();

        let candidate = CandidateEntity {
            raw_name: "kern-cli".to_string(),
            raw_type_hint: Some("Crate".to_string()),
        };

        // Distance in the middle of the ambiguous zone (default: 0.15..0.35).
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
            "ambiguous zone should have called judge(): {outcome:?}"
        );
    }

    #[tokio::test]
    async fn evaluate_promotion_promotes_after_threshold_via_engine() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let mut last = None;
        for i in 0..PROMOTION_THRESHOLD {
            last = Some(
                engine
                    .evaluate_promotion("influences", "emergent type", &format!("doc-{i}.md"))
                    .await
                    .unwrap(),
            );
        }

        assert_eq!(last.unwrap().status, RelationTypeStatus::Canonical);
    }

    #[tokio::test]
    async fn evaluate_promotion_does_not_promote_before_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let record = engine
            .evaluate_promotion("influences", "emergent type", "doc-0.md")
            .await
            .unwrap();

        assert_eq!(record.status, RelationTypeStatus::Candidate);
    }

    #[tokio::test]
    async fn file_without_frontmatter_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let result = engine
            .learn_or_reuse_frontmatter_profile(
                std::path::Path::new("docs/freeform.md"),
                "# No frontmatter\njust prose.\n",
            )
            .await
            .unwrap();

        assert!(result.is_none());
    }

    /// Real integration against Ollama — the first call learns (calls the
    /// extractor), the second call with the SAME shape reuses the cache
    /// without calling the extractor again (verified indirectly: the
    /// returned profile id is identical across both calls).
    #[tokio::test]
    async fn second_occurrence_of_the_same_shape_reuses_the_cached_profile() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());
        let content = "---\nid: TASK-0001\nkind: task\ndepends_on: [TASK-0000]\n---\n\n# Task\n";

        let first = engine
            .learn_or_reuse_frontmatter_profile(
                std::path::Path::new(".specify/specs/a.md"),
                content,
            )
            .await
            .unwrap()
            .expect("should learn a profile on the first occurrence");

        let second = engine
            .learn_or_reuse_frontmatter_profile(
                std::path::Path::new(".specify/specs/b.md"), // different file, same shape
                content,
            )
            .await
            .unwrap()
            .expect("should reuse the cached profile on the second occurrence");

        assert_eq!(
            first.id, second.id,
            "same shape in the same scope should reuse the profile, not learn again"
        );
        assert_eq!(
            first.field_mapping.get("id").cloned().flatten().as_deref(),
            Some("id")
        );
    }

    #[tokio::test]
    async fn fallback_metric_reflects_real_calls_to_judge() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());
        assert_eq!(engine.metrics.fallback_rate(), 0.0);

        let existing_type = engine
            .types
            .find_or_create_entity_type("Crate", "a crate")
            .await
            .unwrap();

        // 2 candidates outside the ambiguous zone — no cost from judge().
        for name in ["kern-a", "kern-b"] {
            engine
                .process_candidate(
                    kern_model::CandidateEntity {
                        raw_name: name.to_string(),
                        raw_type_hint: Some("Crate".to_string()),
                    },
                    0.05, // low distance
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

    /// Real end-to-end integration: extract() via Ollama over a real
    /// prose chunk, distance computed against an already-existing entity
    /// type via real embedding, decision applied — the path that
    /// catch_up_scan (kern-cli) now exercises on every chunk. Skips if
    /// Ollama is unavailable.
    #[tokio::test]
    async fn process_chunk_extracts_and_classifies_candidates_via_real_ollama() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());
        engine
            .types
            .find_or_create_entity_type("Crate", "a Rust crate from the kern workspace")
            .await
            .unwrap();

        let outcomes = engine
            .process_chunk(
                "kern-ontology is the crate that implements the incremental ontology engine.",
                "docs/ontology.md",
            )
            .await
            .unwrap();

        assert!(
            !outcomes.is_empty(),
            "expected at least one classified candidate"
        );
    }

    /// With no entity type existing yet, the first candidate never merges
    /// (there's nothing to compare against) — it always creates a new
    /// type.
    #[tokio::test]
    async fn process_chunk_with_no_existing_types_creates_new_type() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let outcomes = engine
            .process_chunk(
                "kern-vector wraps embedded LanceDB for vector indexing.",
                "docs/vector.md",
            )
            .await
            .unwrap();

        assert!(!outcomes.is_empty());
        assert!(
            outcomes
                .iter()
                .all(|o| matches!(o, ClassificationOutcome::NewType { .. })),
            "with no existing types, every candidate should create a new type: {outcomes:?}"
        );
    }
}
