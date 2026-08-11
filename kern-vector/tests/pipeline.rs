//! End-to-end integration: watcher/chunk (kern-ingest) -> embed (kern-model,
//! via real Ollama) -> index (kern-vector, real LanceDB) -> search_hybrid.
//!
//! Goal: "the dogfooding corpus becomes searchable." Skips (doesn't fail) if
//! Ollama or the `all-minilm` model aren't available locally — same
//! convention as the kern-model tests.

use std::path::Path;

use kern_ingest::{
    BudgetAwareMarkdownChunker, HeuristicTokenCounter, MarkdownChunker, StructuralMarkdownChunker,
};
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
    let mut store = match LanceVectorStore::open(dir.path()).await {
        Ok(s) => s,
        Err(e) => panic!("failed to open LanceVectorStore: {e}"),
    };
    let caps = provider
        .capabilities()
        .await
        .expect("capabilities should succeed against a real running Ollama");
    store
        .ensure_table(caps.embedding_dim as i32)
        .await
        .expect("ensure_table should succeed against a fresh directory");

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

/// Locates `llama-server` on PATH — same helper as kern-model's own
/// integration tests, duplicated here rather than shared because it's
/// test-only glue, not product code.
fn find_llama_server_binary() -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join("llama-server"))
        .find(|candidate| candidate.is_file())
}

fn find_test_gguf_model() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let path = std::path::PathBuf::from(home)
        .join(".cache")
        .join("kern")
        .join("models")
        .join("bge-small-en-v1.5-q4_k_m.gguf");
    path.is_file().then_some(path)
}

/// Direct empirical closure of the bug that started the plug-and-play
/// provider initiative: a real, oversized Markdown section blew past an
/// embedding model's real context window and `llama-server` returned
/// `"input (1202 tokens) is too large to process... current batch size:
/// 512"`. Reproduced here against the exact kind of real backend that
/// surfaced it — kern's own bundled `LlamaCppRuntime` (raw
/// `/v1/embeddings`), **not** `OllamaClient`: empirically, Ollama's
/// `/api/embed` wrapper silently truncates oversized input instead of
/// erroring (confirmed directly against a real running Ollama — a
/// separate, quieter version of the same underlying problem, but it never
/// reproduces this specific hard failure). This test proves both halves
/// for real: (1) the OLD unwrapped `StructuralMarkdownChunker` alone
/// still produces a chunk the real subprocess rejects, and (2)
/// `BudgetAwareMarkdownChunker`, with the budget explicitly set to what
/// the runtime was spawned with, produces only chunks it accepts — every
/// single one.
#[tokio::test]
async fn budget_aware_chunker_closes_the_context_window_bug_against_real_llama_cpp() {
    let Some(binary) = find_llama_server_binary() else {
        eprintln!("llama-server not found on PATH — skipping integration test");
        return;
    };
    let Some(model) = find_test_gguf_model() else {
        eprintln!(
            "test model ~/.cache/kern/models/bge-small-en-v1.5-q4_k_m.gguf \
             not found — skipping integration test"
        );
        return;
    };

    // context_size deliberately small (well under this model's real
    // architectural max) so a realistic fixture reliably exceeds it,
    // without needing an implausibly huge one.
    let budget = 64;
    let runtime =
        match kern_model::LlamaCppRuntime::spawn(&binary, &model, 8793, budget as u32).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("llama-server failed to start ({e}) — skipping integration test");
                return;
            }
        };

    // One section with enough repeated real prose to exceed the budget on
    // its own — realistic (a long design-notes paragraph), not
    // adversarially malformed input.
    let big_section = "The kern architecture separates ingestion, embedding, and ontology \
                        concerns into distinct crates connected only through traits, so a \
                        real backend can be swapped without touching domain logic. "
        .repeat(20);
    let markdown = format!(
        "# Design Notes\n\n{big_section}\n\n# Short Section\n\nJust a short paragraph here.\n"
    );

    // (1) Reproduce the original bug for real: the OLD chunker, unwrapped,
    // still produces at least one chunk the real backend rejects.
    let old_chunker = StructuralMarkdownChunker;
    let old_chunks = old_chunker.chunk(Path::new("design-notes.md"), &markdown);
    let mut reproduced_original_bug = false;
    for chunk in &old_chunks {
        if runtime.embed(&chunk.content).await.is_err() {
            reproduced_original_bug = true;
            break;
        }
    }
    assert!(
        reproduced_original_bug,
        "expected the unwrapped chunker to still produce at least one chunk the real backend \
         rejects (context_size: {budget}) — if this no longer reproduces, the fixture needs \
         to be bigger, not the assertion removed"
    );

    // (2) The fix: BudgetAwareMarkdownChunker, budget matching exactly
    // what the runtime was spawned with — every resulting chunk must
    // embed successfully against the same real backend.
    let new_chunker = BudgetAwareMarkdownChunker::new(
        StructuralMarkdownChunker,
        Box::new(HeuristicTokenCounter),
        budget,
    );
    let new_chunks = new_chunker.chunk(Path::new("design-notes.md"), &markdown);
    assert!(
        new_chunks.len() > old_chunks.len(),
        "expected the budget-aware chunker to produce more, smaller chunks"
    );

    for chunk in &new_chunks {
        runtime.embed(&chunk.content).await.unwrap_or_else(|e| {
            panic!(
                "budget-aware chunk still rejected by the real backend (context_size: \
                 {budget}): {e}\nchunk content: {:?}",
                chunk.content
            )
        });
    }
}
