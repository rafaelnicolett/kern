//! Outbound adapters (Adapters/Out) over SQLite — implement
//! `TypeRepository` and `InstanceRepository` via `rusqlite`, no ORM
//! (see docs/adr/0004). One file per project (`<project>/.kern/registry.db`).
//!
//! `rusqlite::Connection` is not `Sync` — every call runs in
//! `spawn_blocking` over a connection guarded by a `Mutex`, never directly
//! on the Tokio runtime.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::{
    EntityRecord, InstanceRepository, OntologyError, RelationRecord, RelationTypeRecord,
    RelationTypeStatus, TypeRepository, SEED_RELATION_TYPES,
};

fn open_connection(path: &Path) -> Result<Connection, OntologyError> {
    let conn = Connection::open(path).map_err(|e| OntologyError::OpenFailed {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(conn)
}

/// All 5 tables, created by whichever repository opens the file first —
/// `SqliteTypeRepository` and `SqliteInstanceRepository` are two
/// independent `Connection`s over the *same* physical file
/// (`<project>/.kern/registry.db`), and `list_entity_types`/
/// `list_relation_types` compute `instance_count` via a live JOIN across
/// both repositories' tables (see the comment on `entity_types` below) —
/// so every table needs to exist regardless of which repository is opened
/// first, or opened alone (as several unit tests do).
fn ensure_schema(conn: &Connection) -> Result<(), OntologyError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS entity_types (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS relation_types (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS relation_type_hits (
            type_id TEXT NOT NULL,
            file_path TEXT NOT NULL,
            PRIMARY KEY (type_id, file_path)
        );
        CREATE TABLE IF NOT EXISTS entities (
            id TEXT PRIMARY KEY,
            type_id TEXT NOT NULL,
            canonical_name TEXT NOT NULL,
            first_seen_file TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS relations (
            id TEXT PRIMARY KEY,
            type_id TEXT NOT NULL,
            source_entity_id TEXT NOT NULL,
            target_entity_id TEXT NOT NULL,
            confidence REAL NOT NULL,
            evidence_chunk_id TEXT NOT NULL,
            UNIQUE (type_id, source_entity_id, target_entity_id)
        );",
    )?;
    Ok(())
}

fn status_to_str(status: RelationTypeStatus) -> &'static str {
    match status {
        RelationTypeStatus::Candidate => "candidate",
        RelationTypeStatus::Canonical => "canonical",
    }
}

fn status_from_str(s: &str) -> RelationTypeStatus {
    match s {
        "canonical" => RelationTypeStatus::Canonical,
        _ => RelationTypeStatus::Candidate,
    }
}

pub(crate) fn now() -> String {
    // TODO: switch to chrono::Utc::now() once the dependency lands in the
    // workspace (same pending decision already noted on ChunkRecord).
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Real adapter for the Type Registry aggregate.
pub struct SqliteTypeRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteTypeRepository {
    pub fn open(path: &Path) -> Result<Self, OntologyError> {
        let conn = open_connection(path)?;
        // instance_count is intentionally NOT a stored column (see ADR-0002:
        // "derived from the Instance Graph, the Type Registry doesn't own
        // this number, only exposes it") — every query below computes it
        // live via COUNT(*) LEFT JOIN against `entities`/`relations`. A
        // stored counter was tried first and never got updated anywhere,
        // silently reporting 0 forever — a real bug found empirically while
        // verifying the ontology engine end to end.
        ensure_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn row_to_relation_type(row: &rusqlite::Row) -> rusqlite::Result<RelationTypeRecord> {
        let id: String = row.get(0)?;
        let name: String = row.get(1)?;
        let description: String = row.get(2)?;
        let status: String = row.get(3)?;
        let instance_count: i64 = row.get(4)?;
        Ok(RelationTypeRecord {
            id: Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()),
            name,
            description,
            status: status_from_str(&status),
            independent_hits: 0, // filled in by the caller, via count_hits, when relevant
            instance_count: instance_count as u64,
        })
    }

    fn row_to_entity_type(row: &rusqlite::Row) -> rusqlite::Result<crate::EntityTypeRecord> {
        let id: String = row.get(0)?;
        let name: String = row.get(1)?;
        let description: String = row.get(2)?;
        let instance_count: i64 = row.get(3)?;
        Ok(crate::EntityTypeRecord {
            id: Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()),
            name,
            description,
            instance_count: instance_count as u64,
        })
    }
}

#[async_trait]
impl TypeRepository for SqliteTypeRepository {
    async fn find_or_create_entity_type(
        &self,
        name: &str,
        description: &str,
    ) -> Result<crate::EntityTypeRecord, OntologyError> {
        let conn = self.conn.clone();
        let name = name.to_string();
        let description = description.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO entity_types (id, name, description, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![Uuid::new_v4().to_string(), name, description, now()],
            )?;
            let mut stmt = conn.prepare(
                "SELECT et.id, et.name, et.description, COUNT(e.id)
                 FROM entity_types et LEFT JOIN entities e ON e.type_id = et.id
                 WHERE et.name = ?1
                 GROUP BY et.id",
            )?;
            let record = stmt.query_row(params![name], Self::row_to_entity_type)?;
            Ok::<_, OntologyError>(record)
        })
        .await?
    }

    async fn seed_canonical_vocabulary(&self) -> Result<(), OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            for name in SEED_RELATION_TYPES {
                conn.execute(
                    "INSERT OR IGNORE INTO relation_types (id, name, description, status, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        Uuid::new_v4().to_string(),
                        name,
                        format!("seeded canonical type: {name}"),
                        status_to_str(RelationTypeStatus::Canonical),
                        now()
                    ],
                )?;
            }
            Ok::<_, OntologyError>(())
        })
        .await?
    }

    async fn find_relation_type(
        &self,
        name: &str,
    ) -> Result<Option<RelationTypeRecord>, OntologyError> {
        let conn = self.conn.clone();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT rt.id, rt.name, rt.description, rt.status, COUNT(r.id)
                 FROM relation_types rt LEFT JOIN relations r ON r.type_id = rt.id
                 WHERE rt.name = ?1
                 GROUP BY rt.id",
            )?;
            let record = stmt
                .query_row(params![name], Self::row_to_relation_type)
                .optional()?;
            Ok::<_, OntologyError>(record)
        })
        .await?
    }

    async fn register_candidate_type(
        &self,
        name: &str,
        description: &str,
    ) -> Result<RelationTypeRecord, OntologyError> {
        let conn = self.conn.clone();
        let name = name.to_string();
        let description = description.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO relation_types (id, name, description, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    Uuid::new_v4().to_string(),
                    name,
                    description,
                    status_to_str(RelationTypeStatus::Candidate),
                    now()
                ],
            )?;
            let mut stmt = conn.prepare(
                "SELECT rt.id, rt.name, rt.description, rt.status, COUNT(r.id)
                 FROM relation_types rt LEFT JOIN relations r ON r.type_id = rt.id
                 WHERE rt.name = ?1
                 GROUP BY rt.id",
            )?;
            let record = stmt.query_row(params![name], Self::row_to_relation_type)?;
            Ok::<_, OntologyError>(record)
        })
        .await?
    }

    async fn record_independent_hit(
        &self,
        type_id: Uuid,
        file_path: &str,
    ) -> Result<u32, OntologyError> {
        let conn = self.conn.clone();
        let file_path = file_path.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO relation_type_hits (type_id, file_path) VALUES (?1, ?2)",
                params![type_id.to_string(), file_path],
            )?;
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM relation_type_hits WHERE type_id = ?1",
                params![type_id.to_string()],
                |row| row.get(0),
            )?;
            Ok::<_, OntologyError>(count as u32)
        })
        .await?
    }

    async fn promote_to_canonical(&self, type_id: Uuid) -> Result<(), OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.execute(
                "UPDATE relation_types SET status = 'canonical' WHERE id = ?1",
                params![type_id.to_string()],
            )?;
            Ok::<_, OntologyError>(())
        })
        .await?
    }

    async fn list_entity_types(&self) -> Result<Vec<crate::EntityTypeRecord>, OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT et.id, et.name, et.description, COUNT(e.id)
                 FROM entity_types et LEFT JOIN entities e ON e.type_id = et.id
                 GROUP BY et.id
                 ORDER BY et.name",
            )?;
            let rows = stmt
                .query_map(params![], Self::row_to_entity_type)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, OntologyError>(rows)
        })
        .await?
    }

    async fn list_relation_types(&self) -> Result<Vec<RelationTypeRecord>, OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT rt.id, rt.name, rt.description, rt.status, COUNT(r.id)
                 FROM relation_types rt LEFT JOIN relations r ON r.type_id = rt.id
                 GROUP BY rt.id
                 ORDER BY rt.name",
            )?;
            let rows = stmt
                .query_map(params![], Self::row_to_relation_type)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, OntologyError>(rows)
        })
        .await?
    }
}

/// Real adapter for the Instance Graph aggregate.
pub struct SqliteInstanceRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteInstanceRepository {
    pub fn open(path: &Path) -> Result<Self, OntologyError> {
        let conn = open_connection(path)?;
        ensure_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn row_to_entity(row: &rusqlite::Row) -> rusqlite::Result<EntityRecord> {
        let id: String = row.get(0)?;
        let type_id: String = row.get(1)?;
        let canonical_name: String = row.get(2)?;
        let first_seen_file: String = row.get(3)?;
        let updated_at: String = row.get(4)?;
        Ok(EntityRecord {
            id: Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()),
            type_id: Uuid::parse_str(&type_id).unwrap_or_else(|_| Uuid::nil()),
            canonical_name,
            first_seen_file,
            updated_at,
        })
    }

    fn row_to_relation(row: &rusqlite::Row) -> rusqlite::Result<RelationRecord> {
        let id: String = row.get(0)?;
        let type_id: String = row.get(1)?;
        let source_entity_id: String = row.get(2)?;
        let target_entity_id: String = row.get(3)?;
        let confidence: f64 = row.get(4)?;
        let evidence_chunk_id: String = row.get(5)?;
        Ok(RelationRecord {
            id: Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()),
            type_id: Uuid::parse_str(&type_id).unwrap_or_else(|_| Uuid::nil()),
            source_entity_id: Uuid::parse_str(&source_entity_id).unwrap_or_else(|_| Uuid::nil()),
            target_entity_id: Uuid::parse_str(&target_entity_id).unwrap_or_else(|_| Uuid::nil()),
            confidence: confidence as f32,
            evidence_chunk_id: Uuid::parse_str(&evidence_chunk_id).unwrap_or_else(|_| Uuid::nil()),
        })
    }
}

#[async_trait]
impl InstanceRepository for SqliteInstanceRepository {
    async fn find_or_create_entity(
        &self,
        type_id: Uuid,
        canonical_name: &str,
        first_seen_file: &str,
    ) -> Result<EntityRecord, OntologyError> {
        let conn = self.conn.clone();
        let canonical_name = canonical_name.to_string();
        let first_seen_file = first_seen_file.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let existing = conn
                .query_row(
                    "SELECT id, type_id, canonical_name, first_seen_file, updated_at
                     FROM entities WHERE type_id = ?1 AND canonical_name = ?2",
                    params![type_id.to_string(), canonical_name],
                    Self::row_to_entity,
                )
                .optional()?;
            if let Some(entity) = existing {
                return Ok::<_, OntologyError>(entity);
            }

            let id = Uuid::new_v4();
            let updated_at = now();
            // first_seen_file is immutable after creation — it's only
            // written in this INSERT, never in an UPDATE.
            conn.execute(
                "INSERT INTO entities (id, type_id, canonical_name, first_seen_file, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id.to_string(),
                    type_id.to_string(),
                    canonical_name,
                    first_seen_file,
                    updated_at,
                ],
            )?;

            Ok(EntityRecord {
                id,
                type_id,
                canonical_name,
                first_seen_file,
                updated_at,
            })
        })
        .await?
    }

    /// Get-or-create by `(type_id, source_entity_id, target_entity_id)` —
    /// real bug found empirically: `kern serve` reprocesses the whole
    /// corpus from scratch on every invocation (no incremental cache
    /// yet), and a plain unconditional `INSERT` here meant every restart
    /// against an already-populated project duplicated every single
    /// frontmatter-derived relation — not just wasted work, a real
    /// correctness bug: `direct_relations`/`query_ontological`'s reported
    /// relation counts grew without bound across ordinary restarts.
    /// `INSERT OR IGNORE` against the table's `UNIQUE` constraint mirrors
    /// the same idiom `SqliteTypeRepository::find_or_create_entity_type`
    /// already uses.
    async fn record_relation(&self, relation: RelationRecord) -> Result<(), OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO relations (id, type_id, source_entity_id, target_entity_id, confidence, evidence_chunk_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    relation.id.to_string(),
                    relation.type_id.to_string(),
                    relation.source_entity_id.to_string(),
                    relation.target_entity_id.to_string(),
                    relation.confidence,
                    relation.evidence_chunk_id.to_string(),
                ],
            )?;
            Ok::<_, OntologyError>(())
        })
        .await?
    }

    async fn related_entities(
        &self,
        entity_id: Uuid,
        depth: u32,
    ) -> Result<Vec<EntityRecord>, OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut frontier = vec![entity_id];
            let mut visited = std::collections::HashSet::new();
            visited.insert(entity_id);
            let mut result = Vec::new();

            for _ in 0..depth.max(1) {
                if frontier.is_empty() {
                    break;
                }
                let mut next_frontier = Vec::new();
                for current in &frontier {
                    let mut stmt = conn.prepare(
                        "SELECT target_entity_id FROM relations WHERE source_entity_id = ?1
                         UNION
                         SELECT source_entity_id FROM relations WHERE target_entity_id = ?1",
                    )?;
                    let neighbor_ids: Vec<String> = stmt
                        .query_map(params![current.to_string()], |row| row.get(0))?
                        .collect::<rusqlite::Result<_>>()?;

                    for id_str in neighbor_ids {
                        let Ok(id) = Uuid::parse_str(&id_str) else {
                            continue;
                        };
                        if visited.insert(id) {
                            next_frontier.push(id);
                            if let Some(entity) = conn
                                .query_row(
                                    "SELECT id, type_id, canonical_name, first_seen_file, updated_at
                                     FROM entities WHERE id = ?1",
                                    params![id_str],
                                    Self::row_to_entity,
                                )
                                .optional()?
                            {
                                result.push(entity);
                            }
                        }
                    }
                }
                frontier = next_frontier;
            }

            Ok::<_, OntologyError>(result)
        })
        .await?
    }

    async fn find_entities_by_name(
        &self,
        name_query: &str,
    ) -> Result<Vec<EntityRecord>, OntologyError> {
        let conn = self.conn.clone();
        let pattern = format!("%{}%", name_query.to_lowercase());
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT id, type_id, canonical_name, first_seen_file, updated_at
                 FROM entities WHERE LOWER(canonical_name) LIKE ?1
                 ORDER BY canonical_name",
            )?;
            let rows = stmt
                .query_map(params![pattern], Self::row_to_entity)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, OntologyError>(rows)
        })
        .await?
    }

    async fn direct_relations(
        &self,
        entity_id: Uuid,
    ) -> Result<Vec<RelationRecord>, OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT id, type_id, source_entity_id, target_entity_id, confidence, evidence_chunk_id
                 FROM relations WHERE source_entity_id = ?1 OR target_entity_id = ?1",
            )?;
            let rows = stmt
                .query_map(params![entity_id.to_string()], Self::row_to_relation)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, OntologyError>(rows)
        })
        .await?
    }

    async fn find_path(
        &self,
        from: Uuid,
        to: Uuid,
        max_depth: u32,
    ) -> Result<Option<Vec<RelationRecord>>, OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            if from == to {
                return Ok::<_, OntologyError>(Some(Vec::new()));
            }

            // BFS: each visited node stores (relation used to reach it, previous node).
            let mut visited: std::collections::HashMap<Uuid, (RelationRecord, Uuid)> =
                std::collections::HashMap::new();
            let mut frontier = vec![from];
            let mut found = false;

            for _ in 0..max_depth.max(1) {
                if frontier.is_empty() || found {
                    break;
                }
                let mut next_frontier = Vec::new();
                for current in &frontier {
                    let mut stmt = conn.prepare(
                        "SELECT id, type_id, source_entity_id, target_entity_id, confidence, evidence_chunk_id
                         FROM relations WHERE source_entity_id = ?1 OR target_entity_id = ?1",
                    )?;
                    let edges: Vec<RelationRecord> = stmt
                        .query_map(params![current.to_string()], Self::row_to_relation)?
                        .collect::<rusqlite::Result<_>>()?;

                    for edge in edges {
                        let neighbor = if edge.source_entity_id == *current {
                            edge.target_entity_id
                        } else {
                            edge.source_entity_id
                        };
                        if neighbor == *current || visited.contains_key(&neighbor) || neighbor == from
                        {
                            continue;
                        }
                        visited.insert(neighbor, (edge.clone(), *current));
                        if neighbor == to {
                            found = true;
                            break;
                        }
                        next_frontier.push(neighbor);
                    }
                    if found {
                        break;
                    }
                }
                frontier = next_frontier;
            }

            if !visited.contains_key(&to) {
                return Ok(None);
            }

            let mut path = Vec::new();
            let mut cursor = to;
            while let Some((edge, prev)) = visited.get(&cursor) {
                path.push(edge.clone());
                cursor = *prev;
            }
            path.reverse();
            Ok(Some(path))
        })
        .await?
    }

    async fn retype_entity(
        &self,
        entity_id: Uuid,
        new_type_id: Uuid,
    ) -> Result<EntityRecord, OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.execute(
                "UPDATE entities SET type_id = ?1 WHERE id = ?2",
                params![new_type_id.to_string(), entity_id.to_string()],
            )?;
            let mut stmt = conn.prepare(
                "SELECT id, type_id, canonical_name, first_seen_file, updated_at
                 FROM entities WHERE id = ?1",
            )?;
            let record = stmt.query_row(params![entity_id.to_string()], Self::row_to_entity)?;
            Ok::<_, OntologyError>(record)
        })
        .await?
    }
}

/// Real adapter for the Frontmatter Profile aggregate.
pub struct SqliteFrontmatterProfileRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteFrontmatterProfileRepository {
    pub fn open(path: &Path) -> Result<Self, OntologyError> {
        let conn = open_connection(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS frontmatter_profiles (
                id TEXT PRIMARY KEY,
                folder_scope TEXT NOT NULL,
                key_fingerprint TEXT NOT NULL,
                field_mapping TEXT NOT NULL,
                learned_at TEXT NOT NULL,
                UNIQUE(folder_scope, key_fingerprint)
            );",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

#[async_trait]
impl crate::frontmatter::FrontmatterProfileRepository for SqliteFrontmatterProfileRepository {
    async fn find(
        &self,
        folder_scope: &str,
        key_fingerprint: &str,
    ) -> Result<Option<crate::frontmatter::FrontmatterProfile>, OntologyError> {
        let conn = self.conn.clone();
        let folder_scope = folder_scope.to_string();
        let key_fingerprint = key_fingerprint.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let record = conn
                .query_row(
                    "SELECT id, folder_scope, key_fingerprint, field_mapping, learned_at
                     FROM frontmatter_profiles WHERE folder_scope = ?1 AND key_fingerprint = ?2",
                    params![folder_scope, key_fingerprint],
                    |row| {
                        let id: String = row.get(0)?;
                        let folder_scope: String = row.get(1)?;
                        let key_fingerprint: String = row.get(2)?;
                        let field_mapping_json: String = row.get(3)?;
                        let learned_at: String = row.get(4)?;
                        Ok((
                            id,
                            folder_scope,
                            key_fingerprint,
                            field_mapping_json,
                            learned_at,
                        ))
                    },
                )
                .optional()?;

            let Some((id, folder_scope, key_fingerprint, field_mapping_json, learned_at)) = record
            else {
                return Ok::<_, OntologyError>(None);
            };
            let field_mapping = serde_json::from_str(&field_mapping_json).map_err(|e| {
                OntologyError::OpenFailed {
                    path: "frontmatter_profiles.field_mapping".to_string(),
                    reason: e.to_string(),
                }
            })?;

            Ok(Some(crate::frontmatter::FrontmatterProfile {
                id: Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()),
                folder_scope,
                key_fingerprint,
                field_mapping,
                learned_at,
            }))
        })
        .await?
    }

    async fn save(
        &self,
        profile: crate::frontmatter::FrontmatterProfile,
    ) -> Result<(), OntologyError> {
        let conn = self.conn.clone();
        let field_mapping_json = serde_json::to_string(&profile.field_mapping).map_err(|e| {
            OntologyError::OpenFailed {
                path: "frontmatter_profiles.field_mapping".to_string(),
                reason: e.to_string(),
            }
        })?;
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO frontmatter_profiles (id, folder_scope, key_fingerprint, field_mapping, learned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    profile.id.to_string(),
                    profile.folder_scope,
                    profile.key_fingerprint,
                    field_mapping_json,
                    profile.learned_at,
                ],
            )?;
            Ok::<_, OntologyError>(())
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RelationTypeStatus, PROMOTION_THRESHOLD};

    #[tokio::test]
    async fn seed_seeds_the_8_canonical_types() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteTypeRepository::open(&dir.path().join("registry.db")).unwrap();
        repo.seed_canonical_vocabulary().await.unwrap();

        for name in SEED_RELATION_TYPES {
            let record = repo.find_relation_type(name).await.unwrap().unwrap();
            assert_eq!(record.status, RelationTypeStatus::Canonical);
        }
    }

    #[tokio::test]
    async fn instance_count_reflects_real_entities_and_relations() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        let types = SqliteTypeRepository::open(&db_path).unwrap();
        let instances = SqliteInstanceRepository::open(&db_path).unwrap();
        types.seed_canonical_vocabulary().await.unwrap();

        let entity_type = types
            .find_or_create_entity_type("Crate", "a crate")
            .await
            .unwrap();
        assert_eq!(
            entity_type.instance_count, 0,
            "no instance created yet — should be 0, not a stale stored value"
        );

        let a = instances
            .find_or_create_entity(entity_type.id, "kern-a", "a.md")
            .await
            .unwrap();
        instances
            .find_or_create_entity(entity_type.id, "kern-b", "b.md")
            .await
            .unwrap();

        let refreshed = types
            .find_or_create_entity_type("Crate", "a crate")
            .await
            .unwrap();
        assert_eq!(
            refreshed.instance_count, 2,
            "instance_count should reflect the 2 real entities just created"
        );
        let listed = types.list_entity_types().await.unwrap();
        let crate_type = listed.iter().find(|t| t.name == "Crate").unwrap();
        assert_eq!(crate_type.instance_count, 2);

        let depends_on = types
            .find_relation_type("depends_on")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(depends_on.instance_count, 0);

        let b = instances
            .find_or_create_entity(entity_type.id, "kern-b", "b.md")
            .await
            .unwrap();
        instances
            .record_relation(RelationRecord {
                id: Uuid::new_v4(),
                type_id: depends_on.id,
                source_entity_id: a.id,
                target_entity_id: b.id,
                confidence: 1.0,
                evidence_chunk_id: Uuid::new_v4(),
            })
            .await
            .unwrap();

        let depends_on_refreshed = types
            .find_relation_type("depends_on")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            depends_on_refreshed.instance_count, 1,
            "instance_count should reflect the 1 real relation just recorded"
        );
    }

    #[tokio::test]
    async fn seed_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteTypeRepository::open(&dir.path().join("registry.db")).unwrap();
        repo.seed_canonical_vocabulary().await.unwrap();
        repo.seed_canonical_vocabulary().await.unwrap();

        let record = repo
            .find_relation_type("depends_on")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.status, RelationTypeStatus::Canonical);
    }

    #[tokio::test]
    async fn register_candidate_is_get_or_create() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteTypeRepository::open(&dir.path().join("registry.db")).unwrap();

        let first = repo
            .register_candidate_type("relates_loosely_to", "emergent test type")
            .await
            .unwrap();
        assert_eq!(first.status, RelationTypeStatus::Candidate);

        let second = repo
            .register_candidate_type("relates_loosely_to", "description ignored on the 2nd call")
            .await
            .unwrap();
        assert_eq!(first.id, second.id, "get-or-create shouldn't duplicate");
    }

    #[tokio::test]
    async fn promotion_to_canonical_after_threshold_of_independent_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteTypeRepository::open(&dir.path().join("registry.db")).unwrap();

        let candidate = repo
            .register_candidate_type("influences", "emergent type")
            .await
            .unwrap();

        let mut last_count = 0;
        for i in 0..PROMOTION_THRESHOLD {
            last_count = repo
                .record_independent_hit(candidate.id, &format!("doc-{i}.md"))
                .await
                .unwrap();
        }
        assert_eq!(last_count, PROMOTION_THRESHOLD);

        // The same file again should not count as a 4th hit.
        let repeated = repo
            .record_independent_hit(candidate.id, "doc-0.md")
            .await
            .unwrap();
        assert_eq!(
            repeated, PROMOTION_THRESHOLD,
            "the same file doesn't count twice"
        );

        repo.promote_to_canonical(candidate.id).await.unwrap();
        let promoted = repo
            .find_relation_type("influences")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(promoted.status, RelationTypeStatus::Canonical);
    }

    #[tokio::test]
    async fn find_or_create_entity_does_not_duplicate_and_preserves_first_seen_file() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteInstanceRepository::open(&dir.path().join("registry.db")).unwrap();
        let type_id = Uuid::new_v4();

        let first = repo
            .find_or_create_entity(type_id, "kern-ontology", "arch.md")
            .await
            .unwrap();
        let second = repo
            .find_or_create_entity(type_id, "kern-ontology", "other-file.md")
            .await
            .unwrap();

        assert_eq!(first.id, second.id, "same entity, shouldn't duplicate");
        assert_eq!(
            second.first_seen_file, "arch.md",
            "first_seen_file is immutable after creation"
        );
    }

    #[tokio::test]
    async fn relations_require_evidence_and_related_entities_walks_the_graph() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteInstanceRepository::open(&dir.path().join("registry.db")).unwrap();
        let type_id = Uuid::new_v4();

        let a = repo
            .find_or_create_entity(type_id, "kern-ingest", "a.md")
            .await
            .unwrap();
        let b = repo
            .find_or_create_entity(type_id, "kern-model", "b.md")
            .await
            .unwrap();
        let c = repo
            .find_or_create_entity(type_id, "kern-vector", "c.md")
            .await
            .unwrap();

        repo.record_relation(RelationRecord {
            id: Uuid::new_v4(),
            type_id,
            source_entity_id: a.id,
            target_entity_id: b.id,
            confidence: 0.9,
            evidence_chunk_id: Uuid::new_v4(),
        })
        .await
        .unwrap();
        repo.record_relation(RelationRecord {
            id: Uuid::new_v4(),
            type_id,
            source_entity_id: b.id,
            target_entity_id: c.id,
            confidence: 0.9,
            evidence_chunk_id: Uuid::new_v4(),
        })
        .await
        .unwrap();

        let depth1 = repo.related_entities(a.id, 1).await.unwrap();
        assert_eq!(depth1.len(), 1);
        assert_eq!(depth1[0].id, b.id);

        let depth2 = repo.related_entities(a.id, 2).await.unwrap();
        assert_eq!(depth2.len(), 2, "depth 2 should reach c via b");
        assert!(depth2.iter().any(|e| e.id == c.id));
    }

    /// Real bug found empirically on a dogfood corpus: `kern serve`
    /// reprocesses the whole corpus on every restart (no incremental
    /// cache), and every restart against an already-populated project
    /// used to record the exact same frontmatter-derived relation again
    /// — with a fresh `id` and `evidence_chunk_id` each time, so nothing
    /// deduplicated it. This simulates two "indexing passes" recording
    /// the same logical relation and asserts only one row survives.
    #[tokio::test]
    async fn record_relation_is_idempotent_across_repeated_indexing_passes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteInstanceRepository::open(&dir.path().join("registry.db")).unwrap();
        let type_id = Uuid::new_v4();

        let source = repo
            .find_or_create_entity(type_id, "TASK-010b", "a.md")
            .await
            .unwrap();
        let target = repo
            .find_or_create_entity(type_id, "TASK-010a", "a.md")
            .await
            .unwrap();

        // First "indexing pass".
        repo.record_relation(RelationRecord {
            id: Uuid::new_v4(),
            type_id,
            source_entity_id: source.id,
            target_entity_id: target.id,
            confidence: 1.0,
            evidence_chunk_id: Uuid::new_v4(),
        })
        .await
        .unwrap();

        // Second "indexing pass" — same logical relation, fresh id and
        // evidence_chunk_id, exactly like a second `kern serve` restart
        // reprocessing the same file would produce.
        repo.record_relation(RelationRecord {
            id: Uuid::new_v4(),
            type_id,
            source_entity_id: source.id,
            target_entity_id: target.id,
            confidence: 1.0,
            evidence_chunk_id: Uuid::new_v4(),
        })
        .await
        .unwrap();

        let relations = repo.direct_relations(source.id).await.unwrap();
        assert_eq!(
            relations.len(),
            1,
            "the same (type, source, target) relation recorded twice should not duplicate"
        );
    }

    #[tokio::test]
    async fn find_entities_by_name_is_case_insensitive_and_by_substring() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteInstanceRepository::open(&dir.path().join("registry.db")).unwrap();
        let type_id = Uuid::new_v4();

        repo.find_or_create_entity(type_id, "kern-ontology", "a.md")
            .await
            .unwrap();
        repo.find_or_create_entity(type_id, "kern-vector", "b.md")
            .await
            .unwrap();

        let found = repo.find_entities_by_name("ONTOLOGY").await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].canonical_name, "kern-ontology");

        let found_all_kern = repo.find_entities_by_name("kern").await.unwrap();
        assert_eq!(found_all_kern.len(), 2);
    }

    #[tokio::test]
    async fn find_path_finds_the_shortest_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteInstanceRepository::open(&dir.path().join("registry.db")).unwrap();
        let type_id = Uuid::new_v4();

        let a = repo
            .find_or_create_entity(type_id, "a", "a.md")
            .await
            .unwrap();
        let b = repo
            .find_or_create_entity(type_id, "b", "b.md")
            .await
            .unwrap();
        let c = repo
            .find_or_create_entity(type_id, "c", "c.md")
            .await
            .unwrap();

        repo.record_relation(RelationRecord {
            id: Uuid::new_v4(),
            type_id,
            source_entity_id: a.id,
            target_entity_id: b.id,
            confidence: 1.0,
            evidence_chunk_id: Uuid::new_v4(),
        })
        .await
        .unwrap();
        repo.record_relation(RelationRecord {
            id: Uuid::new_v4(),
            type_id,
            source_entity_id: b.id,
            target_entity_id: c.id,
            confidence: 1.0,
            evidence_chunk_id: Uuid::new_v4(),
        })
        .await
        .unwrap();

        let path = repo.find_path(a.id, c.id, 5).await.unwrap();
        assert!(path.is_some());
        assert_eq!(path.unwrap().len(), 2, "a->b->c is 2 hops");
    }

    #[tokio::test]
    async fn find_path_returns_none_without_a_connection() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteInstanceRepository::open(&dir.path().join("registry.db")).unwrap();
        let type_id = Uuid::new_v4();

        let a = repo
            .find_or_create_entity(type_id, "a", "a.md")
            .await
            .unwrap();
        let isolated = repo
            .find_or_create_entity(type_id, "isolated", "z.md")
            .await
            .unwrap();

        let path = repo.find_path(a.id, isolated.id, 5).await.unwrap();
        assert!(path.is_none());
    }

    #[tokio::test]
    async fn list_types_returns_registered_types() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        let types = SqliteTypeRepository::open(&db_path).unwrap();
        types.seed_canonical_vocabulary().await.unwrap();
        types
            .find_or_create_entity_type("Crate", "a crate")
            .await
            .unwrap();

        let entity_types = types.list_entity_types().await.unwrap();
        assert_eq!(entity_types.len(), 1);
        assert_eq!(entity_types[0].name, "Crate");

        let relation_types = types.list_relation_types().await.unwrap();
        assert_eq!(relation_types.len(), SEED_RELATION_TYPES.len());
    }
}
