//! End-to-end integration: watcher/chunk (kern-ingest) -> embed (kern-model,
//! via real Ollama) -> index (kern-vector, real LanceDB) -> search_hybrid.
//!
//! Goal: "the dogfooding corpus becomes searchable." Skips (doesn't fail) if
//! Ollama or the `all-minilm` model aren't available locally — same
//! convention as the kern-model tests.

use std::path::Path;

use kern_ingest::{MarkdownChunker, StructuralMarkdownChunker};
use kern_model::{EmbeddingProvider, OllamaClient};
use kern_vector::{ChunkRecord, LanceVectorStore, VectorStore};

#[tokio::test]
async fn pipeline_ingestion_indexing_and_search_end_to_end() {
    let provider = OllamaClient::new("all-minilm");
    if !provider.probe().await {
        eprintln!("Ollama is not running on :11434 — skipping integration test");
        return;
    }

    let markdown = "\
# Ontology Engine

The ontology engine decides merge, new type, or judge for each candidate.

# File Ingestion

The watcher observes the folder and triggers reindexing only on a real hash change.
";

    let chunker = StructuralMarkdownChunker;
    let chunks = chunker.chunk(Path::new("corpus/doc.md"), markdown);
    assert_eq!(chunks.len(), 2, "expected 2 chunks (2 headings)");

    let dir = tempfile::tempdir().unwrap();
    let store = match LanceVectorStore::open(dir.path()).await {
        Ok(s) => s,
        Err(e) => panic!("failed to open LanceVectorStore: {e}"),
    };

    for chunk in &chunks {
        let embedding = match provider.embed(&chunk.content).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("all-minilm model unavailable ({e}) — skipping integration test");
                return;
            }
        };

        store
            .upsert(ChunkRecord {
                id: chunk.id,
                file_path: chunk.file_path.to_string_lossy().to_string(),
                content: chunk.content.clone(),
                embedding,
                content_hash: chunk.content_hash.clone(),
                updated_at: "2026-08-06T00:00:00Z".to_string(),
            })
            .await
            .expect("upsert failed");
    }

    // Query with a term that only appears semantically close to the
    // ontology chunk — confirms that the whole pipeline (chunk -> embed ->
    // index) returns the right chunk, not just any chunk.
    let query_embedding = provider
        .embed("decision to merge or create a new type in the ontology engine")
        .await
        .expect("query embed failed");

    let results = store
        .search_hybrid(
            &query_embedding,
            "decision to merge or create a new type in the ontology engine",
            1,
        )
        .await
        .expect("search_hybrid failed");

    assert_eq!(results.len(), 1);
    assert!(
        results[0].chunk.content.contains("Ontology Engine"),
        "expected the ontology chunk as the closest result, got: {:?}",
        results[0].chunk.content
    );
}
