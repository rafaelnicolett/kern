//! kern-ontology — type registry, relation vocabulary, incremental diff
//! engine. The core subdomain of the project — this is what the fallback
//! rate of `judge()` measures.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

mod frontmatter;
mod metrics;
mod sqlite;

pub use frontmatter::{
    folder_scope, frontmatter_value_as_strings, key_fingerprint, parse_frontmatter_keys,
    parse_frontmatter_values, FrontmatterProfile, FrontmatterProfileRepository,
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
    /// Updates an existing entity's type in place — the id (and every
    /// `Relation` already pointing at it) never changes, only `type_id`
    /// does. Used when a placeholder entity created by a forward reference
    /// (see `OntologyEngine::resolve_or_create_placeholder_entity`) is
    /// later discovered to have a real, specific type once its own file
    /// gets ingested — without this, that placeholder would stay a
    /// permanently disconnected `unresolved-reference`, and a *second*,
    /// correctly-typed entity would get created alongside it, splitting
    /// what should be one entity's relations across two records.
    /// `first_seen_file` is untouched — it stays immutable per its own
    /// invariant even when the type is corrected later.
    async fn retype_entity(
        &self,
        entity_id: Uuid,
        new_type_id: Uuid,
    ) -> Result<EntityRecord, OntologyError>;
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

/// Result of deterministic, frontmatter-driven ingestion for one file —
/// bypasses the merge/new-type/judge decision entirely, since frontmatter
/// already states the entity's kind and relations explicitly (no embedding
/// distance needed, no LLM call except the one-time schema interpretation
/// already covered by `learn_or_reuse_frontmatter_profile`).
#[derive(Debug, Clone)]
pub struct FrontmatterIngestOutcome {
    pub entity: EntityRecord,
    pub relations_created: usize,
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
    /// Embedding of each entity type's `description`, keyed by `type_id`.
    /// `find_or_create_entity_type` is get-or-create — a type's description
    /// never changes after creation — so once embedded, a type's vector is
    /// valid for the lifetime of this engine. Without this cache,
    /// `nearest_entity_type` re-embedded every existing type's description
    /// on every single candidate, an O(candidates x types) blowup that
    /// dominated real indexing time on any corpus large enough to
    /// accumulate more than a handful of types.
    type_embedding_cache: tokio::sync::RwLock<std::collections::HashMap<Uuid, Vec<f32>>>,
    /// Embedding of a candidate's own probe text, keyed by
    /// `trim().to_lowercase()` of that text. A real corpus mentions the
    /// same term dozens of times — embedding is a deterministic function of
    /// the text, so a repeat mention reuses the earlier vector instead of
    /// paying another network round-trip. This does NOT change how a
    /// repeat candidate is classified: only the embedding lookup is
    /// skipped, the distance is still computed fresh against whatever
    /// entity types currently exist.
    candidate_embedding_cache: tokio::sync::RwLock<std::collections::HashMap<String, Vec<f32>>>,
    /// In-memory mirror of `types.list_entity_types()`, invalidated (not
    /// incrementally updated — simplicity over precision here) after any
    /// call that can add or change an entity type. Avoids a SQLite table
    /// scan on every single candidate — `nearest_entity_type` calls this
    /// once per candidate.
    entity_type_list_cache: tokio::sync::RwLock<Option<Vec<EntityTypeRecord>>>,
    /// Same idea as `entity_type_list_cache`, for `types.list_relation_types()`
    /// (read once per chunk in `process_chunk`, invalidated by
    /// `evaluate_promotion` and any frontmatter-driven relation type
    /// creation).
    relation_type_list_cache: tokio::sync::RwLock<Option<Vec<RelationTypeRecord>>>,
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
            type_embedding_cache: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            candidate_embedding_cache: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            entity_type_list_cache: tokio::sync::RwLock::new(None),
            relation_type_list_cache: tokio::sync::RwLock::new(None),
        }
    }

    /// Cached counterpart to `types.list_entity_types()` — see
    /// `entity_type_list_cache`.
    async fn cached_entity_types(&self) -> Result<Vec<EntityTypeRecord>, OntologyError> {
        if let Some(cached) = self.entity_type_list_cache.read().await.as_ref() {
            return Ok(cached.clone());
        }
        let fresh = self.types.list_entity_types().await?;
        *self.entity_type_list_cache.write().await = Some(fresh.clone());
        Ok(fresh)
    }

    async fn invalidate_entity_type_list_cache(&self) {
        *self.entity_type_list_cache.write().await = None;
    }

    /// Cached counterpart to `types.list_relation_types()` — see
    /// `relation_type_list_cache`.
    async fn cached_relation_types(&self) -> Result<Vec<RelationTypeRecord>, OntologyError> {
        if let Some(cached) = self.relation_type_list_cache.read().await.as_ref() {
            return Ok(cached.clone());
        }
        let fresh = self.types.list_relation_types().await?;
        *self.relation_type_list_cache.write().await = Some(fresh.clone());
        Ok(fresh)
    }

    async fn invalidate_relation_type_list_cache(&self) {
        *self.relation_type_list_cache.write().await = None;
    }

    /// Cached counterpart to `embedder.embed()` for a candidate's own probe
    /// text — see `candidate_embedding_cache`.
    async fn candidate_text_embedding(&self, text: &str) -> Result<Vec<f32>, OntologyError> {
        let key = text.trim().to_lowercase();
        if let Some(cached) = self.candidate_embedding_cache.read().await.get(&key) {
            return Ok(cached.clone());
        }
        let embedding = self.embedder.embed(text).await?;
        self.candidate_embedding_cache
            .write()
            .await
            .insert(key, embedding.clone());
        Ok(embedding)
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
        let relation_type_names: Vec<String> = self
            .cached_relation_types()
            .await?
            .into_iter()
            .map(|t| t.name)
            .collect();
        let vocab = kern_model::RelationVocabulary {
            canonical_types: relation_type_names.clone(),
        };
        let candidates = self.extraction.extract(chunk_content, &vocab).await?;

        let mut outcomes = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            // Real bug found empirically: the model sometimes extracts a
            // relation-type's own *field name* (e.g. "depends_on") as if
            // it were a domain entity — usually from prose that discusses
            // the frontmatter mechanism itself, not the domain it
            // describes. Reject outright rather than merge/create-type: a
            // candidate literally named after reserved vocabulary is
            // almost certainly this, not a real entity, and letting it
            // into the entity table poisons query_ontological's
            // entity-mention matching for every future question that
            // happens to use the same word (see kern-mcp's
            // `query_ontological_prefers_an_exact_match_over_a_longer_substring_collision`
            // regression test for the exact failure this caused).
            if relation_type_names
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&candidate.raw_name))
            {
                outcomes.push(ClassificationOutcome::Rejected {
                    reason: format!(
                        "candidate '{}' collides with a reserved relation-type name — likely \
                         the model extracted the field name itself rather than a domain entity",
                        candidate.raw_name
                    ),
                });
                continue;
            }
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
        let entity_types = self.cached_entity_types().await?;
        if entity_types.is_empty() {
            return Ok((1.0, None));
        }

        let probe_text = candidate
            .raw_type_hint
            .as_deref()
            .unwrap_or(&candidate.raw_name);
        let candidate_embedding = self.candidate_text_embedding(probe_text).await?;

        let mut best: Option<(EntityTypeRecord, f32)> = None;
        for entity_type in entity_types {
            let desc_embedding = self.type_description_embedding(&entity_type).await?;
            let distance = 1.0 - cosine_similarity(&candidate_embedding, &desc_embedding);
            if best.as_ref().map(|(_, d)| distance < *d).unwrap_or(true) {
                best = Some((entity_type, distance));
            }
        }
        let (entity_type, distance) = best.expect("entity_types is not empty in this branch");
        Ok((distance, Some(entity_type)))
    }

    /// Cached embedding of `entity_type.description` — a type's description
    /// is fixed at creation (`find_or_create_entity_type` is get-or-create),
    /// so this only ever calls `embed()` once per distinct `type_id` for the
    /// lifetime of this engine.
    async fn type_description_embedding(
        &self,
        entity_type: &EntityTypeRecord,
    ) -> Result<Vec<f32>, OntologyError> {
        if let Some(cached) = self.type_embedding_cache.read().await.get(&entity_type.id) {
            return Ok(cached.clone());
        }
        let embedding = self.embedder.embed(&entity_type.description).await?;
        self.type_embedding_cache
            .write()
            .await
            .insert(entity_type.id, embedding.clone());
        Ok(embedding)
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
        self.invalidate_entity_type_list_cache().await;
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
        self.invalidate_relation_type_list_cache().await;
        if record.status == RelationTypeStatus::Canonical {
            return Ok(record);
        }

        let hits = self
            .types
            .record_independent_hit(record.id, file_path)
            .await?;
        if hits >= PROMOTION_THRESHOLD {
            self.types.promote_to_canonical(record.id).await?;
            self.invalidate_relation_type_list_cache().await;
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

    /// Deterministic counterpart to `process_chunk`: given a file whose
    /// frontmatter maps to a learned/cached `FrontmatterProfile`, resolves
    /// the file's own entity (kind = the value of the field mapped to the
    /// `kind` concept, name = the value mapped to `id`) and creates a real
    /// `Relation` for every field mapped to one of the 8 canonical relation
    /// concepts (`depends_on`, `implements`, ...). No embedding distance,
    /// no `judge()` call for classifying THIS candidate — frontmatter
    /// already states the answer. `None` if the file has no frontmatter
    /// (falls back to prose extraction via `process_chunk`, outside this
    /// method) or the frontmatter has no `id`-mapped field to name the
    /// entity by (falls back to a name derived from the file path).
    ///
    /// **"Deterministic" has one real caveat**: which frontmatter *key*
    /// maps to which canonical *concept* is still decided by one LLM call
    /// per new key-shape (`interpret_frontmatter_schema`, cached
    /// thereafter per folder scope). That single classification can be
    /// wrong — observed empirically with `llama3.2`: a `depends_on` key
    /// was mapped to the `supersedes` concept instead. Because the
    /// mapping is cached, one misclassification silently affects every
    /// subsequent file with the same key shape in that folder, until the
    /// cache is invalidated (there's no invalidation mechanism in v0 — see
    /// `FrontmatterProfile`'s own doc). This is a real, measured
    /// reliability gap, not a hypothetical.
    ///
    /// `evidence_chunk_id` should be the id of the chunk that actually
    /// contains this frontmatter block — `kern-ingest`'s chunker never
    /// splits before the first heading, so that's always the file's first
    /// chunk. The caller (kern-cli's catch_up_scan) is the one that knows
    /// chunk ids, hence it's a parameter rather than looked up here.
    ///
    /// A relation can reference an id that hasn't been ingested yet (e.g.
    /// file B lists `depends_on: [A]` before A is ever scanned, which
    /// happens for real with an ordinary numbered corpus — directory
    /// traversal order isn't guaranteed to match id order). The target
    /// gets a placeholder entity under a generic `unresolved-reference`
    /// type. When A is *later* ingested for real, its own call into this
    /// method finds that placeholder by exact name and retypes it in
    /// place (`InstanceRepository::retype_entity`) instead of creating a
    /// second, disconnected entity — the relation B recorded earlier still
    /// points at the same entity id, which now carries A's real type.
    /// `first_seen_file` is never touched by a retype — it stays pinned to
    /// whichever file was scanned first, per its own immutability
    /// invariant, even though that file only ever saw the placeholder.
    pub async fn process_frontmatter(
        &self,
        file_path: &str,
        content: &str,
        evidence_chunk_id: Uuid,
    ) -> Result<Option<FrontmatterIngestOutcome>, OntologyError> {
        let Some(profile) = self
            .learn_or_reuse_frontmatter_profile(std::path::Path::new(file_path), content)
            .await?
        else {
            return Ok(None);
        };
        let Some(values) = parse_frontmatter_values(content) else {
            return Ok(None);
        };

        // Invert `field_mapping` (frontmatter key -> concept) so we can go
        // concept -> key -> value.
        let mut concept_to_key: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        for (key, concept) in &profile.field_mapping {
            if let Some(c) = concept {
                concept_to_key.insert(c.as_str(), key.as_str());
            }
        }
        let value_of = |concept: &str| -> Option<String> {
            let key = concept_to_key.get(concept)?;
            let value = values.get(*key)?;
            frontmatter_value_as_strings(value).into_iter().next()
        };

        let own_name = value_of("id").unwrap_or_else(|| derive_name_from_path(file_path));
        let own_kind = value_of("kind").unwrap_or_else(|| "spec-item".to_string());

        let entity_type = self
            .types
            .find_or_create_entity_type(&own_kind, &format!("frontmatter kind '{own_kind}'"))
            .await?;
        self.invalidate_entity_type_list_cache().await;

        // If a forward reference from another file already created a
        // placeholder for this exact id, retype it in place instead of
        // creating a second, disconnected entity — any relation recorded
        // against the placeholder's id stays valid and now points at a
        // correctly-typed entity, closing the gap documented on this
        // method instead of just working around it.
        let existing = self
            .instances
            .find_entities_by_name(&own_name)
            .await?
            .into_iter()
            .find(|e| e.canonical_name.eq_ignore_ascii_case(&own_name));
        let entity = match existing {
            Some(e) if e.type_id != entity_type.id => {
                self.instances.retype_entity(e.id, entity_type.id).await?
            }
            Some(e) => e,
            None => {
                self.instances
                    .find_or_create_entity(entity_type.id, &own_name, file_path)
                    .await?
            }
        };

        let mut relations_created = 0usize;
        for concept in RELATION_CONCEPTS {
            let Some(key) = concept_to_key.get(concept) else {
                continue;
            };
            let Some(value) = values.get(*key) else {
                continue;
            };
            let targets = frontmatter_value_as_strings(value);
            if targets.is_empty() {
                continue;
            }

            // The 8 relation concepts are exactly the 8 seeded canonical
            // types (see SEED_RELATION_TYPES) — they always already exist
            // after `seed_canonical_vocabulary`, so this is a lookup, not a
            // candidate registration.
            let relation_type = match self.types.find_relation_type(concept).await? {
                Some(rt) => rt,
                None => {
                    let rt = self
                        .types
                        .register_candidate_type(
                            concept,
                            &format!("frontmatter relation '{concept}'"),
                        )
                        .await?;
                    self.invalidate_relation_type_list_cache().await;
                    rt
                }
            };

            for target_name in targets {
                let target_entity = self
                    .resolve_or_create_placeholder_entity(&target_name, file_path)
                    .await?;
                self.instances
                    .record_relation(RelationRecord {
                        id: Uuid::new_v4(),
                        type_id: relation_type.id,
                        source_entity_id: entity.id,
                        target_entity_id: target_entity.id,
                        // Deterministic, not inferred — frontmatter stated
                        // it explicitly, there's no distance/judge score to
                        // report here.
                        confidence: 1.0,
                        evidence_chunk_id,
                    })
                    .await?;
                relations_created += 1;
            }
        }

        Ok(Some(FrontmatterIngestOutcome {
            entity,
            relations_created,
        }))
    }

    /// Finds an entity by exact (case-insensitive) canonical name across
    /// all types, or creates a placeholder for it — see the known
    /// limitation documented on `process_frontmatter`.
    async fn resolve_or_create_placeholder_entity(
        &self,
        canonical_name: &str,
        referencing_file: &str,
    ) -> Result<EntityRecord, OntologyError> {
        let matches = self.instances.find_entities_by_name(canonical_name).await?;
        if let Some(exact) = matches
            .into_iter()
            .find(|e| e.canonical_name.eq_ignore_ascii_case(canonical_name))
        {
            return Ok(exact);
        }

        let placeholder_type = self
            .types
            .find_or_create_entity_type(
                "unresolved-reference",
                "placeholder for an id referenced by frontmatter before its own file was ingested",
            )
            .await?;
        self.invalidate_entity_type_list_cache().await;
        self.instances
            .find_or_create_entity(placeholder_type.id, canonical_name, referencing_file)
            .await
    }
}

/// The 8 canonical relation concepts a frontmatter field can map to — kept
/// in sync with `SEED_RELATION_TYPES` on purpose (same vocabulary, this is
/// just the subset `interpret_frontmatter_schema` is allowed to map a key
/// to that represents a relation rather than `id`/`kind`/`status`).
const RELATION_CONCEPTS: [&str; 8] = [
    "depends_on",
    "supersedes",
    "implements",
    "causes",
    "owned_by",
    "conflicts_with",
    "documents",
    "configures",
];

/// Fallback entity name when frontmatter has no field mapped to the `id`
/// concept — the file stem (e.g. `docs/specs/TASK-0001.md` -> `TASK-0001`).
fn derive_name_from_path(file_path: &str) -> String {
    std::path::Path::new(file_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file_path.to_string())
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

    /// Real integration: frontmatter -> real entity with the right kind and
    /// name, no distance/judge involved. Pulls in a real Ollama call only
    /// for the one-time schema interpretation (skips if unavailable).
    #[tokio::test]
    async fn process_frontmatter_creates_a_real_entity_from_id_and_kind() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());
        let content = "---\nid: TASK-0042\nkind: task\ndepends_on: []\n---\n\n# Task\n";

        let outcome = engine
            .process_frontmatter("docs/specs/TASK-0042.md", content, Uuid::new_v4())
            .await
            .unwrap()
            .expect("frontmatter present, should not be None");

        assert_eq!(outcome.entity.canonical_name, "TASK-0042");
        assert_eq!(outcome.relations_created, 0);

        let entity_types = engine.types.list_entity_types().await.unwrap();
        let task_type = entity_types
            .iter()
            .find(|t| t.name == "task")
            .expect("should have created a 'task' entity type from the kind field");
        assert_eq!(task_type.instance_count, 1);
    }

    /// Real integration: a `depends_on` field becomes a real `Relation`
    /// between two real entities, routable by `query_ontological` — this
    /// is the capability the vector-only prose path can't provide.
    #[tokio::test]
    async fn process_frontmatter_creates_a_real_relation_between_two_known_entities() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        engine
            .process_frontmatter(
                "docs/specs/TASK-0001.md",
                "---\nid: TASK-0001\nkind: task\n---\n\n# First task\n",
                Uuid::new_v4(),
            )
            .await
            .unwrap()
            .unwrap();

        let outcome = engine
            .process_frontmatter(
                "docs/specs/TASK-0002.md",
                "---\nid: TASK-0002\nkind: task\ndepends_on: [TASK-0001]\n---\n\n# Second task\n",
                Uuid::new_v4(),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(outcome.relations_created, 1);

        let task_0001 = engine
            .instances
            .find_entities_by_name("TASK-0001")
            .await
            .unwrap();
        assert_eq!(task_0001.len(), 1, "TASK-0001 should exist as one entity");

        let relations = engine
            .instances
            .direct_relations(outcome.entity.id)
            .await
            .unwrap();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].target_entity_id, task_0001[0].id);
        // Deliberately not asserting the relation landed under the
        // "depends_on" type specifically: which of the 8 canonical
        // concepts the frontmatter key gets mapped to is a real LLM call
        // (interpret_frontmatter_schema), and a small model can
        // legitimately misclassify a close call (observed once: mapped
        // "depends_on" to "supersedes" instead) — that's the same
        // real-world imprecision the fallback-rate metric exists to
        // track, not a bug in this code path. What this test guarantees is
        // the mechanism: a relation concept really gets created/looked up
        // and a real Relation really gets recorded against the correct,
        // pre-existing target entity.
        let relation_types = engine.types.list_relation_types().await.unwrap();
        let used_type = relation_types
            .iter()
            .find(|t| t.id == relations[0].type_id)
            .expect("the relation's type_id should resolve to a real relation type");
        assert!(
            RELATION_CONCEPTS.contains(&used_type.name.as_str()),
            "relation should land under one of the 8 canonical concepts, got: {}",
            used_type.name
        );
        assert_eq!(used_type.instance_count, 1);
    }

    /// A forward reference (target not ingested yet) still creates a real
    /// relation, pointing at a placeholder entity — documented v0
    /// limitation, not a silent failure.
    #[tokio::test]
    async fn process_frontmatter_forward_reference_creates_a_placeholder_target() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let outcome = engine
            .process_frontmatter(
                "docs/specs/TASK-0002.md",
                "---\nid: TASK-0002\nkind: task\ndepends_on: [TASK-9999]\n---\n\n# Depends on something not seen yet\n",
                Uuid::new_v4(),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(outcome.relations_created, 1);

        let placeholder = engine
            .instances
            .find_entities_by_name("TASK-9999")
            .await
            .unwrap();
        assert_eq!(placeholder.len(), 1);

        let entity_types = engine.types.list_entity_types().await.unwrap();
        assert!(
            entity_types
                .iter()
                .any(|t| t.name == "unresolved-reference"),
            "forward reference should have created the placeholder type"
        );
    }

    /// The real fix for the forward-reference gap: once the referenced
    /// file is actually ingested, the placeholder gets retyped in place —
    /// same entity id, so the relation recorded earlier keeps pointing at
    /// a single, now-correctly-typed entity instead of an orphan.
    #[tokio::test]
    async fn process_frontmatter_retypes_placeholder_when_real_file_is_later_ingested() {
        let probe = OllamaClient::new("llama3.2");
        if !probe.probe().await {
            eprintln!("Ollama is not running on :11434 — skipping integration test");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        // TASK-0002 references TASK-0001 before TASK-0001 has ever been
        // ingested — creates a placeholder for TASK-0001.
        let task_0002 = engine
            .process_frontmatter(
                "docs/specs/TASK-0002.md",
                "---\nid: TASK-0002\nkind: task\ndepends_on: [TASK-0001]\n---\n\n# Second task\n",
                Uuid::new_v4(),
            )
            .await
            .unwrap()
            .unwrap();

        let placeholder_before = engine
            .instances
            .find_entities_by_name("TASK-0001")
            .await
            .unwrap();
        assert_eq!(placeholder_before.len(), 1);
        let placeholder_id = placeholder_before[0].id;
        let placeholder_type_id = placeholder_before[0].type_id;

        // TASK-0001's real file is ingested afterwards.
        let task_0001 = engine
            .process_frontmatter(
                "docs/specs/TASK-0001.md",
                "---\nid: TASK-0001\nkind: task\n---\n\n# First task\n",
                Uuid::new_v4(),
            )
            .await
            .unwrap()
            .unwrap();

        // Same entity id — retyped in place, not a second entity.
        assert_eq!(
            task_0001.entity.id, placeholder_id,
            "the real TASK-0001 entity should be the SAME id as the earlier placeholder, not a new one"
        );
        assert_ne!(
            task_0001.entity.type_id, placeholder_type_id,
            "the entity's type should have changed away from the placeholder type"
        );

        let entity_types = engine.types.list_entity_types().await.unwrap();
        let task_type = entity_types.iter().find(|t| t.name == "task").unwrap();
        assert_eq!(
            task_type.instance_count, 2,
            "both TASK-0001 (retyped) and TASK-0002 should now count under 'task'"
        );
        let unresolved = entity_types
            .iter()
            .find(|t| t.name == "unresolved-reference")
            .unwrap();
        assert_eq!(
            unresolved.instance_count, 0,
            "no entity should be left under the placeholder type after the retype"
        );

        // The relation TASK-0002 recorded earlier still resolves, now to
        // the correctly-typed entity.
        let relations = engine
            .instances
            .direct_relations(task_0002.entity.id)
            .await
            .unwrap();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].target_entity_id, task_0001.entity.id);
    }

    #[tokio::test]
    async fn process_frontmatter_returns_none_without_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());

        let outcome = engine
            .process_frontmatter(
                "docs/notes.md",
                "# just prose\nno frontmatter here\n",
                Uuid::new_v4(),
            )
            .await
            .unwrap();

        assert!(outcome.is_none());
    }

    /// Call-counting `EmbeddingProvider` — deliberately returns the same
    /// vector for every input, so every candidate lands in the low-distance
    /// merge branch (never the ambiguous zone) and the only thing this test
    /// needs to assert is *how many times* `embed()` was called.
    struct CountingEmbedder {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl kern_model::EmbeddingProvider for CountingEmbedder {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, kern_model::ModelError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![1.0, 0.0, 0.0])
        }

        async fn capabilities(
            &self,
        ) -> Result<kern_model::EmbeddingCapabilities, kern_model::ModelError> {
            Ok(kern_model::EmbeddingCapabilities {
                model_id: "fake".to_string(),
                embedding_dim: 3,
                max_input_tokens: None,
            })
        }
    }

    /// Returns one pre-set batch of candidates per call, in order — no
    /// network involved. `judge`/`interpret_frontmatter_schema` panic if
    /// called: with `CountingEmbedder` above, every candidate's distance to
    /// every type is 0.0, always below the low-distance threshold, so this
    /// test's path should never reach the ambiguous zone.
    struct FixedExtraction {
        rounds: std::sync::Mutex<std::collections::VecDeque<Vec<CandidateEntity>>>,
    }

    #[async_trait]
    impl kern_model::ExtractionProvider for FixedExtraction {
        fn model_id(&self) -> &str {
            "fake"
        }

        async fn extract(
            &self,
            _chunk: &str,
            _vocab: &kern_model::RelationVocabulary,
        ) -> Result<Vec<CandidateEntity>, kern_model::ModelError> {
            Ok(self.rounds.lock().unwrap().pop_front().unwrap_or_default())
        }

        async fn judge(
            &self,
            _candidate: &CandidateEntity,
            _nearest_existing: &[kern_model::EntityType],
        ) -> Result<kern_model::JudgeDecision, kern_model::ModelError> {
            panic!("judge() should not be called — every candidate is forced into the low-distance merge branch")
        }

        async fn interpret_frontmatter_schema(
            &self,
            _keys: &[String],
        ) -> Result<std::collections::HashMap<String, Option<String>>, kern_model::ModelError>
        {
            panic!("not exercised by this test")
        }
    }

    #[tokio::test]
    async fn type_description_embeddings_are_cached_across_candidates_and_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        let types: Arc<dyn TypeRepository> =
            Arc::new(SqliteTypeRepository::open(&db_path).unwrap());
        let instances: Arc<dyn InstanceRepository> =
            Arc::new(SqliteInstanceRepository::open(&db_path).unwrap());
        let frontmatter_profiles: Arc<dyn FrontmatterProfileRepository> =
            Arc::new(SqliteFrontmatterProfileRepository::open(&db_path).unwrap());

        let embedder = Arc::new(CountingEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let extraction: Arc<dyn kern_model::ExtractionProvider> = Arc::new(FixedExtraction {
            rounds: std::sync::Mutex::new(
                vec![
                    vec![
                        CandidateEntity {
                            raw_name: "alpha".to_string(),
                            raw_type_hint: None,
                        },
                        CandidateEntity {
                            raw_name: "beta".to_string(),
                            raw_type_hint: None,
                        },
                    ],
                    vec![
                        CandidateEntity {
                            raw_name: "gamma".to_string(),
                            raw_type_hint: None,
                        },
                        CandidateEntity {
                            raw_name: "delta".to_string(),
                            raw_type_hint: None,
                        },
                        CandidateEntity {
                            raw_name: "epsilon".to_string(),
                            raw_type_hint: None,
                        },
                    ],
                ]
                .into(),
            ),
        });

        let engine = OntologyEngine::new(
            types.clone(),
            instances,
            extraction,
            frontmatter_profiles,
            embedder.clone(),
            AmbiguousZoneConfig::default(),
        );

        // 4 pre-existing entity types — the first candidate that looks at
        // each one has to embed its description; every candidate after that
        // should reuse the cached vector instead of embedding it again.
        for name in ["TypeA", "TypeB", "TypeC", "TypeD"] {
            types
                .find_or_create_entity_type(name, &format!("description of {name}"))
                .await
                .unwrap();
        }

        engine.process_chunk("chunk 1", "docs/a.md").await.unwrap(); // 2 candidates
        engine.process_chunk("chunk 2", "docs/b.md").await.unwrap(); // 3 candidates

        // Without the cache: 5 candidates x (4 type embeds + 1 candidate
        // embed) = 25 embed() calls. With the cache: the 4 type
        // descriptions are embedded once ever (4), plus 1 embed per
        // candidate (5) = 9.
        assert_eq!(
            embedder.calls.load(std::sync::atomic::Ordering::SeqCst),
            9,
            "type description embeddings should be cached, not recomputed per candidate"
        );
    }

    #[tokio::test]
    async fn candidate_embeddings_are_cached_by_normalized_text() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        let types: Arc<dyn TypeRepository> =
            Arc::new(SqliteTypeRepository::open(&db_path).unwrap());
        let instances: Arc<dyn InstanceRepository> =
            Arc::new(SqliteInstanceRepository::open(&db_path).unwrap());
        let frontmatter_profiles: Arc<dyn FrontmatterProfileRepository> =
            Arc::new(SqliteFrontmatterProfileRepository::open(&db_path).unwrap());

        let embedder = Arc::new(CountingEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let extraction: Arc<dyn kern_model::ExtractionProvider> = Arc::new(FixedExtraction {
            rounds: std::sync::Mutex::new(
                vec![
                    // Round 1: the exact same name twice in one chunk.
                    vec![
                        CandidateEntity {
                            raw_name: "alpha".to_string(),
                            raw_type_hint: None,
                        },
                        CandidateEntity {
                            raw_name: "alpha".to_string(),
                            raw_type_hint: None,
                        },
                    ],
                    // Round 2: a different-case repeat of the same name,
                    // plus one genuinely new name.
                    vec![
                        CandidateEntity {
                            raw_name: "ALPHA".to_string(),
                            raw_type_hint: None,
                        },
                        CandidateEntity {
                            raw_name: "beta".to_string(),
                            raw_type_hint: None,
                        },
                    ],
                ]
                .into(),
            ),
        });

        let engine = OntologyEngine::new(
            types.clone(),
            instances,
            extraction,
            frontmatter_profiles,
            embedder.clone(),
            AmbiguousZoneConfig::default(),
        );

        for name in ["TypeA", "TypeB"] {
            types
                .find_or_create_entity_type(name, &format!("description of {name}"))
                .await
                .unwrap();
        }

        engine.process_chunk("chunk 1", "docs/a.md").await.unwrap();
        engine.process_chunk("chunk 2", "docs/b.md").await.unwrap();

        // Type descriptions: 2 types, embedded once ever = 2.
        // Candidate embeds: "alpha" (round 1, first occurrence) = 1;
        // second "alpha" in round 1 = cached, 0; "ALPHA" in round 2 =
        // cached via normalized key, 0; "beta" in round 2 = 1.
        // Total = 2 + 1 + 0 + 0 + 1 = 4.
        assert_eq!(
            embedder.calls.load(std::sync::atomic::Ordering::SeqCst),
            4,
            "repeat candidate text (including case variation) should reuse the cached embedding"
        );
    }

    #[tokio::test]
    async fn entity_type_list_cache_is_invalidated_when_a_new_type_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        let types: Arc<dyn TypeRepository> =
            Arc::new(SqliteTypeRepository::open(&db_path).unwrap());
        let instances: Arc<dyn InstanceRepository> =
            Arc::new(SqliteInstanceRepository::open(&db_path).unwrap());
        let frontmatter_profiles: Arc<dyn FrontmatterProfileRepository> =
            Arc::new(SqliteFrontmatterProfileRepository::open(&db_path).unwrap());

        let embedder = Arc::new(CountingEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let extraction: Arc<dyn kern_model::ExtractionProvider> = Arc::new(FixedExtraction {
            rounds: std::sync::Mutex::new(
                vec![
                    // Round 1: no entity types exist yet — must create one.
                    vec![CandidateEntity {
                        raw_name: "widget".to_string(),
                        raw_type_hint: None,
                    }],
                    // Round 2: the exact same name again — if the type list
                    // cache weren't invalidated after round 1's creation,
                    // this would incorrectly see an empty type list again
                    // and create a SECOND "widget" type instead of merging.
                    vec![CandidateEntity {
                        raw_name: "widget".to_string(),
                        raw_type_hint: None,
                    }],
                ]
                .into(),
            ),
        });

        let engine = OntologyEngine::new(
            types.clone(),
            instances,
            extraction,
            frontmatter_profiles,
            embedder,
            AmbiguousZoneConfig::default(),
        );

        let round1 = engine.process_chunk("chunk 1", "docs/a.md").await.unwrap();
        assert!(
            matches!(round1[0], ClassificationOutcome::NewType { .. }),
            "no existing types — first occurrence must create a new type"
        );

        let round2 = engine.process_chunk("chunk 2", "docs/b.md").await.unwrap();
        assert!(
            matches!(round2[0], ClassificationOutcome::Merged { .. }),
            "second occurrence should merge into the type created in round 1, \
             not create a duplicate — got {:?}",
            round2[0]
        );

        let entity_types = types.list_entity_types().await.unwrap();
        assert_eq!(
            entity_types.len(),
            1,
            "stale type list cache would have caused a duplicate 'widget' type"
        );
    }

    /// Real bug found empirically: the model sometimes extracts a
    /// relation-type's own field name (e.g. "depends_on") as a candidate
    /// entity, which later poisons `query_ontological`'s entity-mention
    /// matching (see kern-mcp's regression test for the exact failure
    /// this caused on a real corpus). A candidate whose name collides
    /// with a reserved relation-type name must be rejected before it
    /// ever reaches the entity table — and, as a side effect, before it
    /// costs an embed() call at all.
    #[tokio::test]
    async fn candidate_colliding_with_a_reserved_relation_type_name_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        let types: Arc<dyn TypeRepository> =
            Arc::new(SqliteTypeRepository::open(&db_path).unwrap());
        types.seed_canonical_vocabulary().await.unwrap();
        let instances: Arc<dyn InstanceRepository> =
            Arc::new(SqliteInstanceRepository::open(&db_path).unwrap());
        let frontmatter_profiles: Arc<dyn FrontmatterProfileRepository> =
            Arc::new(SqliteFrontmatterProfileRepository::open(&db_path).unwrap());

        let embedder = Arc::new(CountingEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let extraction: Arc<dyn kern_model::ExtractionProvider> = Arc::new(FixedExtraction {
            rounds: std::sync::Mutex::new(
                vec![vec![
                    // Same casing quirk seen in the real failure — the
                    // model extracted it lowercase with an underscore,
                    // matching the seeded relation type name exactly.
                    CandidateEntity {
                        raw_name: "depends_on".to_string(),
                        raw_type_hint: None,
                    },
                    CandidateEntity {
                        raw_name: "TASK-010b".to_string(),
                        raw_type_hint: None,
                    },
                ]]
                .into(),
            ),
        });

        let engine = OntologyEngine::new(
            types,
            instances,
            extraction,
            frontmatter_profiles,
            embedder.clone(),
            AmbiguousZoneConfig::default(),
        );

        let outcomes = engine
            .process_chunk("chunk mentioning depends_on and TASK-010b", "docs/a.md")
            .await
            .unwrap();

        assert_eq!(outcomes.len(), 2);
        assert!(
            matches!(outcomes[0], ClassificationOutcome::Rejected { .. }),
            "'depends_on' should be rejected as reserved vocabulary, got {:?}",
            outcomes[0]
        );
        assert!(
            matches!(outcomes[1], ClassificationOutcome::NewType { .. }),
            "'TASK-010b' is a real candidate and should still be classified normally, got {:?}",
            outcomes[1]
        );

        // The rejected candidate never reached embed() at all. TASK-010b
        // also costs zero embed calls here, but for an unrelated reason:
        // with no entity types created yet, `nearest_entity_type` returns
        // early before embedding anything (see
        // `process_chunk_with_no_existing_types_creates_new_type`'s real
        // counterpart) — asserting 0 total confirms the rejected
        // candidate specifically added no embed cost of its own.
        assert_eq!(
            embedder.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the reserved-vocabulary rejection should short-circuit before any embed() call"
        );
    }
}
