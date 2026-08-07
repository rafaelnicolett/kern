//! kern-vector — wrapper sobre LanceDB embarcado.
//!
//! Um registro por chunk: {id, file_path, content, embedding, content_hash,
//! updated_at} — ver docs/architecture/data/er-ingestao-indexacao.md no
//! workspace de delivery. Sem servidor externo, sem FFI.

use std::path::Path;
use std::sync::Arc;

// Reexportado via `lancedb::arrow::*` de propósito — não depender direto de
// arrow-array/arrow-schema como crate própria, senão o Cargo resolve uma
// segunda árvore de arrow (versão mais nova) incompatível em tipo com a que
// o lancedb usa internamente (achado real, ver sprint-status.md).
use async_trait::async_trait;
use lancedb::arrow::arrow_array::{
    Array, Float32Array, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
};
use lancedb::arrow::arrow_schema::{ArrowError, DataType, Field, Schema};
use lancedb::query::{ExecutableQuery, QueryBase};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Dimensão do vetor de embedding. Placeholder — depende do modelo default
/// escolhido em kern-model (bge-small = 384; nomic-embed = 768). TODO(S2-BKD-04):
/// tornar configurável junto com a escolha do modelo, não uma constante fixa.
pub const EMBEDDING_DIM: i32 = 384;

const TABLE_NAME: &str = "chunks";

#[derive(Debug, Error)]
pub enum VectorStoreError {
    #[error("falha ao abrir índice vetorial em {path}: {reason}")]
    OpenFailed { path: String, reason: String },
    #[error("chunk {0} não encontrado")]
    ChunkNotFound(Uuid),
    #[error("erro do LanceDB: {0}")]
    Lance(#[from] lancedb::Error),
    #[error("erro Arrow: {0}")]
    Arrow(#[from] ArrowError),
}

/// Registro de chunk persistido — espelha o schema de
/// docs/architecture/data/er-ingestao-indexacao.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    pub id: Uuid,
    pub file_path: String,
    pub content: String,
    pub embedding: Vec<f32>,
    pub content_hash: String,
    pub updated_at: String, // TODO: trocar por chrono::DateTime<Utc> quando a dependência entrar.
}

#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk: ChunkRecord,
    pub score: f32,
}

/// Port: índice vetorial local. Implementação concreta (adapter) fica em
/// `LanceVectorStore`, atrás deste trait — kern-mcp e kern-ontology
/// programam contra o trait, nunca contra LanceDB diretamente.
#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, record: ChunkRecord) -> Result<(), VectorStoreError>;

    /// Remove todos os chunks de um arquivo — reindexação substitui só o
    /// que mudou, nunca o corpus inteiro.
    async fn delete_by_file(&self, file_path: &str) -> Result<(), VectorStoreError>;

    async fn search_hybrid(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, VectorStoreError>;

    /// Usado por `kern status` (kern-cli) — contagem de chunks indexados.
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

/// Adapter real sobre o crate nativo do LanceDB (sem FFI, sem servidor
/// externo). Um diretório por projeto (`<projeto>/.kern/vectors/`).
pub struct LanceVectorStore {
    table: lancedb::Table,
}

impl LanceVectorStore {
    /// Abre (ou cria, se ainda não existir) o índice vetorial na pasta do
    /// projeto.
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
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, VectorStoreError> {
        // TODO(S2-BKD-05): "hybrid" de verdade combina similaridade vetorial
        // com keyword boost — hoje só a busca vetorial está implementada.
        use futures::TryStreamExt;

        let results: Vec<RecordBatch> = self
            .table
            .query()
            .nearest_to(query_embedding)?
            .limit(top_k)
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
                    let score = distances.map(|d| d.value(i)).unwrap_or(0.0);
                    scored.push(ScoredChunk {
                        chunk: ChunkRecord {
                            id,
                            file_path: file_paths.value(i).to_string(),
                            content: contents.value(i).to_string(),
                            embedding: vec![],
                            content_hash: hashes.value(i).to_string(),
                            updated_at: updated.value(i).to_string(),
                        },
                        score,
                    });
                }
            }
        }
        Ok(scored)
    }

    async fn count(&self) -> Result<usize, VectorStoreError> {
        Ok(self.table.count_rows(None).await?)
    }
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
    async fn upsert_e_busca_por_similaridade_encontra_o_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::open(dir.path()).await.unwrap();

        let record = ChunkRecord {
            id: Uuid::new_v4(),
            file_path: "docs/a.md".to_string(),
            content: "conteúdo de teste".to_string(),
            embedding: fake_embedding(1.0),
            content_hash: "hash-1".to_string(),
            updated_at: "2026-08-06T00:00:00Z".to_string(),
        };
        store.upsert(record.clone()).await.unwrap();

        let results = store.search_hybrid(&fake_embedding(1.0), 5).await.unwrap();
        assert!(!results.is_empty(), "esperava encontrar o chunk indexado");
        assert!(results.iter().any(|r| r.chunk.file_path == "docs/a.md"));
    }

    #[tokio::test]
    async fn count_reflete_numero_de_chunks_indexados() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::open(dir.path()).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 0);

        for i in 0..3 {
            store
                .upsert(ChunkRecord {
                    id: Uuid::new_v4(),
                    file_path: format!("docs/{i}.md"),
                    content: "conteúdo".to_string(),
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
    async fn delete_by_file_remove_os_chunks_do_arquivo() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::open(dir.path()).await.unwrap();

        let record = ChunkRecord {
            id: Uuid::new_v4(),
            file_path: "docs/b.md".to_string(),
            content: "outro conteúdo".to_string(),
            embedding: fake_embedding(2.0),
            content_hash: "hash-2".to_string(),
            updated_at: "2026-08-06T00:00:00Z".to_string(),
        };
        store.upsert(record).await.unwrap();
        store.delete_by_file("docs/b.md").await.unwrap();

        let results = store.search_hybrid(&fake_embedding(2.0), 5).await.unwrap();
        assert!(
            results.iter().all(|r| r.chunk.file_path != "docs/b.md"),
            "chunk deveria ter sido removido"
        );
    }

    #[tokio::test]
    async fn upsert_do_mesmo_id_atualiza_em_vez_de_duplicar() {
        let dir = tempfile::tempdir().unwrap();
        let store = LanceVectorStore::open(dir.path()).await.unwrap();
        let id = Uuid::new_v4();

        store
            .upsert(ChunkRecord {
                id,
                file_path: "docs/c.md".to_string(),
                content: "versão 1".to_string(),
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
                content: "versão 2".to_string(),
                embedding: fake_embedding(3.0),
                content_hash: "hash-v2".to_string(),
                updated_at: "2026-08-06T00:01:00Z".to_string(),
            })
            .await
            .unwrap();

        let results = store.search_hybrid(&fake_embedding(3.0), 10).await.unwrap();
        let matches: Vec<_> = results.iter().filter(|r| r.chunk.id == id).collect();
        assert_eq!(matches.len(), 1, "upsert do mesmo id não deveria duplicar");
        assert_eq!(matches[0].chunk.content_hash, "hash-v2");
    }
}
