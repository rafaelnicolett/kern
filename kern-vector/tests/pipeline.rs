//! Integração fim a fim: watcher/chunk (kern-ingest) -> embed (kern-model,
//! via Ollama real) -> index (kern-vector, LanceDB real) -> search_hybrid.
//!
//! Objetivo do Sprint S2: "o corpus de dogfooding passa a estar
//! pesquisável" (sprint-backlog.yaml, S2). Pula (não falha) se Ollama ou o
//! modelo `all-minilm` não estiverem disponíveis localmente — mesma
//! convenção dos testes de kern-model.

use std::path::Path;

use kern_ingest::{MarkdownChunker, StructuralMarkdownChunker};
use kern_model::{EmbeddingProvider, OllamaClient};
use kern_vector::{ChunkRecord, LanceVectorStore, VectorStore};

#[tokio::test]
async fn pipeline_ingestao_indexacao_e_busca_fim_a_fim() {
    let provider = OllamaClient::new("all-minilm");
    if !provider.probe().await {
        eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
        return;
    }

    let markdown = "\
# Motor de Ontologia

O motor de ontologia decide merge, novo tipo ou judge para cada candidato.

# Ingestão de Arquivos

O watcher observa a pasta e dispara reindexação só em mudança real de hash.
";

    let chunker = StructuralMarkdownChunker;
    let chunks = chunker.chunk(Path::new("corpus/doc.md"), markdown);
    assert_eq!(chunks.len(), 2, "esperava 2 chunks (2 headings)");

    let dir = tempfile::tempdir().unwrap();
    let store = match LanceVectorStore::open(dir.path()).await {
        Ok(s) => s,
        Err(e) => panic!("falha ao abrir LanceVectorStore: {e}"),
    };

    for chunk in &chunks {
        let embedding = match provider.embed(&chunk.content).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("modelo all-minilm indisponível ({e}) — pulando teste de integração");
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
            .expect("upsert falhou");
    }

    // Consulta com um termo que só aparece semanticamente perto do chunk de
    // ontologia — confirma que o pipeline inteiro (chunk -> embed -> index)
    // devolve o chunk certo, não qualquer um.
    let query_embedding = provider
        .embed("decisão de merge ou novo tipo no motor de ontologia")
        .await
        .expect("embed da query falhou");

    let results = store
        .search_hybrid(&query_embedding, 1)
        .await
        .expect("search_hybrid falhou");

    assert_eq!(results.len(), 1);
    assert!(
        results[0].chunk.content.contains("Motor de Ontologia"),
        "esperava o chunk de ontologia como resultado mais próximo, veio: {:?}",
        results[0].chunk.content
    );
}
