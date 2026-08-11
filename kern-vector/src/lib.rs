//! kern-vector — a thin wrapper around embedded LanceDB.
//!
//! One record per chunk: {id, file_path, content, embedding, content_hash,
//! updated_at}. No external server, no FFI.

use std::path::Path;
use std::sync::Arc;

// Re-exported via `lancedb::arrow::*` on purpose — don't depend directly on
// arrow-array/arrow-schema as their own crates, or Cargo will resolve a
// second arrow tree (a newer version) that's type-incompatible with the one
// lancedb uses internally (empirically confirmed).
use async_trait::async_trait;
use lancedb::arrow::arrow_array::{
    Array, Float32Array, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
};
use lancedb::arrow::arrow_schema::{ArrowError, DataType, Field, Schema};
use lancedb::query::{ExecutableQuery, QueryBase};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Embedding vector dimension. Placeholder — depends on the default model
/// chosen in kern-model (bge-small = 384; nomic-embed = 768). TODO: make this
/// configurable alongside the model choice, instead of a fixed constant.
pub const EMBEDDING_DIM: i32 = 384;

const TABLE_NAME: &str = "chunks";

#[derive(Debug, Error)]
pub enum VectorStoreError {
    #[error("failed to open vector index at {path}: {reason}")]
    OpenFailed { path: String, reason: String },
    #[error("chunk {0} not found")]
    ChunkNotFound(Uuid),
    #[error("LanceDB error: {0}")]
    Lance(#[from] lancedb::Error),
    #[error("Arrow error: {0}")]
    Arrow(#[from] ArrowError),
}

/// Persisted chunk record — mirrors the Arrow schema in `schema()` below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    pub id: Uuid,
    pub file_path: String,
    pub content: String,
    pub embedding: Vec<f32>,
    pub content_hash: String,
    pub updated_at: String, // TODO: swap for chrono::DateTime<Utc> once the dependency is added.
}

#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk: ChunkRecord,
    pub score: f32,
}

/// Port: local vector index. The concrete implementation (adapter) lives in
/// `LanceVectorStore`, behind this trait — kern-mcp and kern-ontology
/// program against the trait, never against LanceDB directly.
#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, record: ChunkRecord) -> Result<(), VectorStoreError>;

    /// Removes all chunks for a file — reindexing replaces only what changed,
    /// never the entire corpus.
    async fn delete_by_file(&self, file_path: &str) -> Result<(), VectorStoreError>;

    async fn search_hybrid(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, VectorStoreError>;

    /// Used by `kern status` (kern-cli) — count of indexed chunks.
    async fn count(&self) -> Result<usize, VectorStoreError>;
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("file_path", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                EMBEDDING_DIM,
            ),
            true,
        ),
        Field::new("content_hash", DataType::Utf8, false),
        Field::new("updated_at", DataType::Utf8, false),
    ]))
}

fn record_to_batch(record: &ChunkRecord) -> Result<RecordBatch, VectorStoreError> {
    let schema = schema();

    let embedding_values = Float32Array::from(record.embedding.clone());
    let embedding_field = Arc::new(Field::new("item", DataType::Float32, true));
    let embedding = lancedb::arrow::arrow_array::FixedSizeListArray::try_new(
        embedding_field,
        EMBEDDING_DIM,
        Arc::new(embedding_values),
        None,
    )?;

    Ok(RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![record.id.to_string()])) as Arc<dyn Array>,
            Arc::new(StringArray::from(vec![record.file_path.clone()])),
            Arc::new(StringArray::from(vec![record.content.clone()])),
            Arc::new(embedding),
            Arc::new(StringArray::from(vec![record.content_hash.clone()])),
            Arc::new(StringArray::from(vec![record.updated_at.clone()])),
        ],
    )?)
}

/// Real adapter over the native LanceDB crate (no FFI, no external server).
/// One directory per project (`<project>/.kern/vectors/`).
pub struct LanceVectorStore {
    table: lancedb::Table,
}

impl LanceVectorStore {
    /// Opens (or creates, if it doesn't exist yet) the vector index in the
    /// project's folder.
    pub async fn open(root: &Path) -> Result<Self, VectorStoreError> {
        let db = lancedb::connect(&root.to_string_lossy())
            .execute()
            .await
            .map_err(|e| VectorStoreError::OpenFailed {
                path: root.display().to_string(),
                reason: e.to_string(),
            })?;

        let existing = db.table_names().execute().await?;
        let table = if existing.iter().any(|n| n == TABLE_NAME) {
            db.open_table(TABLE_NAME).execute().await?
        } else {
            let schema = schema();
            let empty_batches: Vec<Result<RecordBatch, ArrowError>> = vec![];
            let reader = RecordBatchIterator::new(empty_batches, schema.clone());
            let reader: Box<dyn RecordBatchReader + Send> = Box::new(reader);
            db.create_table(TABLE_NAME, reader).execute().await?
        };

        Ok(Self { table })
    }
}

#[async_trait]
impl VectorStore for LanceVectorStore {
    async fn upsert(&self, record: ChunkRecord) -> Result<(), VectorStoreError> {
        let batch = record_to_batch(&record)?;
        let schema = batch.schema();
        let reader = RecordBatchIterator::new(vec![Ok(batch)], schema);
        let reader: Box<dyn RecordBatchReader + Send> = Box::new(reader);

        let mut merge = self.table.merge_insert(&["id"]);
        merge.when_matched_update_all(None);
        merge.when_not_matched_insert_all();
        merge.execute(reader).await?;
        Ok(())
    }

    async fn delete_by_file(&self, file_path: &str) -> Result<(), VectorStoreError> {
        let escaped = file_path.replace('\'', "''");
        self.table
            .delete(&format!("file_path = '{escaped}'"))
            .await?;
        Ok(())
    }

    async fn search_hybrid(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, VectorStoreError> {
        use futures::TryStreamExt;

        // Over-fetch by vector similarity — gives the keyword fusion enough
        // candidates to actually change the ranking, not just break ties
        // within an already-narrow top_k.
        let candidate_pool = (top_k * 4).max(20);

        let results: Vec<RecordBatch> = self
            .table
            .query()
            .nearest_to(query_embedding)?
            .limit(candidate_pool)
            .execute()
            .await?
            .try_collect()
            .await?;

        let mut scored = Vec::new();
        for batch in results {
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let file_paths = batch
                .column_by_name("file_path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let contents = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let hashes = batch
                .column_by_name("content_hash")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let updated = batch
                .column_by_name("updated_at")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let distances = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

            if let (Some(ids), Some(file_paths), Some(contents), Some(hashes), Some(updated)) =
                (ids, file_paths, contents, hashes, updated)
            {
                for i in 0..batch.num_rows() {
                    let id = Uuid::parse_str(ids.value(i)).unwrap_or_else(|_| Uuid::nil());
                    let distance = distances.map(|d| d.value(i)).unwrap_or(0.0);
                    scored.push(ScoredChunk {
                        chunk: ChunkRecord {
                            id,
                            file_path: file_paths.value(i).to_string(),
                            content: contents.value(i).to_string(),
                            embedding: vec![],
                            content_hash: hashes.value(i).to_string(),
                            updated_at: updated.value(i).to_string(),
                        },
                        score: distance,
                    });
                }
            }
        }
        // Smallest distance first — this is the vector rank that the fusion
        // below relies on (don't trust LanceDB's return order without
        // checking).
        scored.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(reciprocal_rank_fuse(scored, query_text, top_k))
    }

    async fn count(&self) -> Result<usize, VectorStoreError> {
        Ok(self.table.count_rows(None).await?)
    }
}

/// Standard Reciprocal Rank Fusion constant (Cormack et al., 2009) — not a
/// value calibrated for this corpus, it's the default established in the
/// literature, which dampens the top ranks of any list well.
const RRF_K: f32 = 60.0;

/// Combines the vector rank (`candidates` is already sorted by ascending
/// distance) with a keyword rank via Reciprocal Rank Fusion — avoids relying
/// on the raw, undocumented scale of the distance LanceDB returns (an ad-hoc
/// weighted sum would have that problem).
fn reciprocal_rank_fuse(
    candidates: Vec<ScoredChunk>,
    query_text: &str,
    top_k: usize,
) -> Vec<ScoredChunk> {
    let keywords = extract_keywords(query_text);

    let mut keyword_order: Vec<usize> = (0..candidates.len()).collect();
    keyword_order.sort_by_key(|&i| {
        std::cmp::Reverse(keyword_match_count(&candidates[i].chunk.content, &keywords))
    });
    let mut keyword_ranks = vec![0usize; candidates.len()];
    for (rank, i) in keyword_order.into_iter().enumerate() {
        keyword_ranks[i] = rank;
    }

    let mut fused: Vec<ScoredChunk> = candidates
        .into_iter()
        .enumerate()
        .map(|(vector_rank, mut candidate)| {
            candidate.score = 1.0 / (RRF_K + vector_rank as f32 + 1.0)
                + 1.0 / (RRF_K + keyword_ranks[vector_rank] as f32 + 1.0);
            candidate
        })
        .collect();

    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.truncate(top_k);
    fused
}

fn keyword_match_count(content: &str, keywords: &[String]) -> usize {
    if keywords.is_empty() {
        return 0;
    }
    let content_lower = content.to_lowercase();
    keywords
        .iter()
        .filter(|k| content_lower.contains(k.as_str()))
        .count()
}

/// Lowercase tokens of at least 3 characters — a v0 heuristic for the
/// keyword boost; not a real linguistic tokenizer (no stemming, no
/// stopwords), it just filters out punctuation and short noise.
fn extract_keywords(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_embedding(seed: f32) -> Vec<f32> {
        (0..EMBEDDING_DIM)
            .map(|i| seed + i as f32 * 0.001)
            .collect()
    }

    #[tokio::test]
    async fn upsert_and_similarity_search_finds_the_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::open(dir.path()).await.unwrap();

        let record = ChunkRecord {
            id: Uuid::new_v4(),
            file_path: "docs/a.md".to_string(),
            content: "test content".to_string(),
            embedding: fake_embedding(1.0),
            content_hash: "hash-1".to_string(),
            updated_at: "2026-08-06T00:00:00Z".to_string(),
        };
        store.upsert(record.clone()).await.unwrap();

        let results = store
            .search_hybrid(&fake_embedding(1.0), "", 5)
            .await
            .unwrap();
        assert!(!results.is_empty(), "expected to find the indexed chunk");
        assert!(results.iter().any(|r| r.chunk.file_path == "docs/a.md"));
    }

    #[tokio::test]
    async fn count_reflects_number_of_indexed_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::open(dir.path()).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 0);

        for i in 0..3 {
            store
                .upsert(ChunkRecord {
                    id: Uuid::new_v4(),
                    file_path: format!("docs/{i}.md"),
                    content: "content".to_string(),
                    embedding: fake_embedding(i as f32),
                    content_hash: format!("hash-{i}"),
                    updated_at: "2026-08-06T00:00:00Z".to_string(),
                })
                .await
                .unwrap();
        }

        assert_eq!(store.count().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn delete_by_file_removes_chunks_for_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::open(dir.path()).await.unwrap();

        let record = ChunkRecord {
            id: Uuid::new_v4(),
            file_path: "docs/b.md".to_string(),
            content: "other content".to_string(),
            embedding: fake_embedding(2.0),
            content_hash: "hash-2".to_string(),
            updated_at: "2026-08-06T00:00:00Z".to_string(),
        };
        store.upsert(record).await.unwrap();
        store.delete_by_file("docs/b.md").await.unwrap();

        let results = store
            .search_hybrid(&fake_embedding(2.0), "", 5)
            .await
            .unwrap();
        assert!(
            results.iter().all(|r| r.chunk.file_path != "docs/b.md"),
            "chunk should have been removed"
        );
    }

    #[tokio::test]
    async fn upsert_with_same_id_updates_instead_of_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::open(dir.path()).await.unwrap();
        let id = Uuid::new_v4();

        store
            .upsert(ChunkRecord {
                id,
                file_path: "docs/c.md".to_string(),
                content: "version 1".to_string(),
                embedding: fake_embedding(3.0),
                content_hash: "hash-v1".to_string(),
                updated_at: "2026-08-06T00:00:00Z".to_string(),
            })
            .await
            .unwrap();

        store
            .upsert(ChunkRecord {
                id,
                file_path: "docs/c.md".to_string(),
                content: "version 2".to_string(),
                embedding: fake_embedding(3.0),
                content_hash: "hash-v2".to_string(),
                updated_at: "2026-08-06T00:01:00Z".to_string(),
            })
            .await
            .unwrap();

        let results = store
            .search_hybrid(&fake_embedding(3.0), "", 10)
            .await
            .unwrap();
        let matches: Vec<_> = results.iter().filter(|r| r.chunk.id == id).collect();
        assert_eq!(
            matches.len(),
            1,
            "upsert of the same id should not duplicate"
        );
        assert_eq!(matches[0].chunk.content_hash, "hash-v2");
    }

    /// Proves that the keyword boost actually changes the ranking, not just
    /// breaks ties: a vectorially more distant chunk that matches the
    /// query's keyword displaces a vectorially closer chunk (but with no
    /// shared term at all) from the top_k.
    #[tokio::test]
    async fn keyword_boost_displaces_vectorially_closer_chunk_with_no_shared_term() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::open(dir.path()).await.unwrap();

        // A: identical to the query in embedding — always ranks first.
        store
            .upsert(ChunkRecord {
                id: Uuid::new_v4(),
                file_path: "docs/a.md".to_string(),
                content: "first chunk, no rare term at all".to_string(),
                embedding: fake_embedding(1.0),
                content_hash: "hash-a".to_string(),
                updated_at: "2026-08-06T00:00:00Z".to_string(),
            })
            .await
            .unwrap();
        // B: second closest by vector, also without the query's term.
        store
            .upsert(ChunkRecord {
                id: Uuid::new_v4(),
                file_path: "docs/b.md".to_string(),
                content: "second chunk, also without the term".to_string(),
                embedding: fake_embedding(1.1),
                content_hash: "hash-b".to_string(),
                updated_at: "2026-08-06T00:00:00Z".to_string(),
            })
            .await
            .unwrap();
        // C: the most distant by vector, but contains the query's exact term.
        store
            .upsert(ChunkRecord {
                id: Uuid::new_v4(),
                file_path: "docs/c.md".to_string(),
                content: "third chunk contains termoraroxyz on purpose".to_string(),
                embedding: fake_embedding(5.0),
                content_hash: "hash-c".to_string(),
                updated_at: "2026-08-06T00:00:00Z".to_string(),
            })
            .await
            .unwrap();

        let results = store
            .search_hybrid(&fake_embedding(1.0), "termoraroxyz", 2)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(
            results.iter().any(|r| r.chunk.file_path == "docs/c.md"),
            "chunk with the query's term should be in the top_k even being the most \
             distant by vector, result: {:?}",
            results
                .iter()
                .map(|r| &r.chunk.file_path)
                .collect::<Vec<_>>()
        );
        assert!(
            !results.iter().any(|r| r.chunk.file_path == "docs/b.md"),
            "chunk without the query's term should have been displaced from the top_k by the boost"
        );
    }
}
