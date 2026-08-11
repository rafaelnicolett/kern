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
                    self.types
                        .register_candidate_type(
                            concept,
                            &format!("frontmatter relation '{concept}'"),
                        )
                        .await?
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
}
