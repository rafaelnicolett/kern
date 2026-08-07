//! Adapters de saída (Adapters/Out) sobre SQLite — implementam
//! `TypeRepository` e `InstanceRepository` via `rusqlite`, sem ORM (ver
//! docs/adr/0004 no workspace de delivery). Um arquivo por projeto
//! (`<projeto>/.kern/registry.db`).
//!
//! `rusqlite::Connection` não é `Sync` — cada chamada roda em
//! `spawn_blocking` sobre uma conexão guardada por `Mutex`, nunca direto no
//! runtime Tokio (Skill lang-rust, seção 2.4).

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
    // TODO: trocar por chrono::Utc::now() quando a dependência entrar no
    // workspace (mesma decisão pendente já registrada em ChunkRecord).
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Adapter real do Agregado 1 (Registro de Tipos).
pub struct SqliteTypeRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteTypeRepository {
    pub fn open(path: &Path) -> Result<Self, OntologyError> {
        let conn = open_connection(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entity_types (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                description TEXT NOT NULL,
                created_at TEXT NOT NULL,
                instance_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS relation_types (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                description TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                instance_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS relation_type_hits (
                type_id TEXT NOT NULL,
                file_path TEXT NOT NULL,
                PRIMARY KEY (type_id, file_path)
            );",
        )?;
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
            independent_hits: 0, // preenchido por quem chama, via count_hits, quando relevante
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
                "INSERT OR IGNORE INTO entity_types (id, name, description, created_at, instance_count)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![Uuid::new_v4().to_string(), name, description, now()],
            )?;
            let mut stmt = conn.prepare(
                "SELECT id, name, description, instance_count FROM entity_types WHERE name = ?1",
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
                    "INSERT OR IGNORE INTO relation_types (id, name, description, status, created_at, instance_count)
                     VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                    params![
                        Uuid::new_v4().to_string(),
                        name,
                        format!("tipo canônico semeado: {name}"),
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
                "SELECT id, name, description, status, instance_count FROM relation_types WHERE name = ?1",
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
                "INSERT OR IGNORE INTO relation_types (id, name, description, status, created_at, instance_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                params![
                    Uuid::new_v4().to_string(),
                    name,
                    description,
                    status_to_str(RelationTypeStatus::Candidate),
                    now()
                ],
            )?;
            let mut stmt = conn.prepare(
                "SELECT id, name, description, status, instance_count FROM relation_types WHERE name = ?1",
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
                "SELECT id, name, description, instance_count FROM entity_types ORDER BY name",
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
                "SELECT id, name, description, status, instance_count FROM relation_types ORDER BY name",
            )?;
            let rows = stmt
                .query_map(params![], Self::row_to_relation_type)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, OntologyError>(rows)
        })
        .await?
    }
}

/// Adapter real do Agregado 2 (Grafo de Instâncias).
pub struct SqliteInstanceRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteInstanceRepository {
    pub fn open(path: &Path) -> Result<Self, OntologyError> {
        let conn = open_connection(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entities (
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
                evidence_chunk_id TEXT NOT NULL
            );",
        )?;
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
            // first_seen_file é imutável após a criação (invariante do
            // Agregado 2) — só é escrito neste INSERT, nunca num UPDATE.
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

    async fn record_relation(&self, relation: RelationRecord) -> Result<(), OntologyError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.execute(
                "INSERT INTO relations (id, type_id, source_entity_id, target_entity_id, confidence, evidence_chunk_id)
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

            // BFS: cada nó visitado guarda (relação usada pra chegar nele, nó anterior).
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
}

/// Adapter real do Agregado 3 (Perfil de Frontmatter).
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
    async fn seed_semeia_os_8_tipos_canonicos() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteTypeRepository::open(&dir.path().join("registry.db")).unwrap();
        repo.seed_canonical_vocabulary().await.unwrap();

        for name in SEED_RELATION_TYPES {
            let record = repo.find_relation_type(name).await.unwrap().unwrap();
            assert_eq!(record.status, RelationTypeStatus::Canonical);
        }
    }

    #[tokio::test]
    async fn seed_e_idempotente() {
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
    async fn register_candidate_e_get_or_create() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteTypeRepository::open(&dir.path().join("registry.db")).unwrap();

        let first = repo
            .register_candidate_type("relates_loosely_to", "tipo emergente de teste")
            .await
            .unwrap();
        assert_eq!(first.status, RelationTypeStatus::Candidate);

        let second = repo
            .register_candidate_type("relates_loosely_to", "descrição ignorada na 2a chamada")
            .await
            .unwrap();
        assert_eq!(first.id, second.id, "get-or-create não deveria duplicar");
    }

    #[tokio::test]
    async fn promocao_a_canonico_apos_threshold_de_arquivos_independentes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteTypeRepository::open(&dir.path().join("registry.db")).unwrap();

        let candidate = repo
            .register_candidate_type("influences", "tipo emergente")
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

        // Mesmo arquivo de novo não deveria contar como um 4º hit.
        let repeated = repo
            .record_independent_hit(candidate.id, "doc-0.md")
            .await
            .unwrap();
        assert_eq!(
            repeated, PROMOTION_THRESHOLD,
            "mesmo arquivo não conta duas vezes"
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
    async fn find_or_create_entity_nao_duplica_e_preserva_first_seen_file() {
        let dir = tempfile::tempdir().unwrap();
        let repo = SqliteInstanceRepository::open(&dir.path().join("registry.db")).unwrap();
        let type_id = Uuid::new_v4();

        let first = repo
            .find_or_create_entity(type_id, "kern-ontology", "arch.md")
            .await
            .unwrap();
        let second = repo
            .find_or_create_entity(type_id, "kern-ontology", "outro-arquivo.md")
            .await
            .unwrap();

        assert_eq!(first.id, second.id, "mesma entidade, não deveria duplicar");
        assert_eq!(
            second.first_seen_file, "arch.md",
            "first_seen_file é imutável após a criação"
        );
    }

    #[tokio::test]
    async fn relations_exigem_evidencia_e_related_entities_percorre_o_grafo() {
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
        assert_eq!(depth2.len(), 2, "profundidade 2 deveria alcançar c via b");
        assert!(depth2.iter().any(|e| e.id == c.id));
    }

    #[tokio::test]
    async fn find_entities_by_name_e_case_insensitive_e_por_substring() {
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
    async fn find_path_encontra_o_caminho_mais_curto() {
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
        assert_eq!(path.unwrap().len(), 2, "a->b->c são 2 saltos");
    }

    #[tokio::test]
    async fn find_path_retorna_none_sem_conexao() {
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
    async fn list_types_retorna_tipos_registrados() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        let types = SqliteTypeRepository::open(&db_path).unwrap();
        types.seed_canonical_vocabulary().await.unwrap();
        types
            .find_or_create_entity_type("Crate", "um crate")
            .await
            .unwrap();

        let entity_types = types.list_entity_types().await.unwrap();
        assert_eq!(entity_types.len(), 1);
        assert_eq!(entity_types[0].name, "Crate");

        let relation_types = types.list_relation_types().await.unwrap();
        assert_eq!(relation_types.len(), SEED_RELATION_TYPES.len());
    }
}
