//! kern-ingest — watcher + chunking markdown-aware.
//!
//! Detecta mudança real (hash, não touch) na pasta observada, converte
//! formatos não-MD via subprocesso externo configurável, e produz `Chunk`s
//! que kern-vector indexa. Nunca processa o corpus inteiro — só o arquivo
//! que mudou.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pulldown_cmark::{Event as MdEvent, Options, Parser, Tag};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("falha ao ler arquivo {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("subprocesso de conversão falhou: {0}")]
    ConversionFailed(String),
    #[error("falha ao observar a pasta {path}: {source}")]
    WatchFailed {
        path: PathBuf,
        #[source]
        source: notify::Error,
    },
}

/// Unidade indexada — ver docs/architecture/data/er-ingestao-indexacao.md
/// no workspace de delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub id: Uuid,
    pub file_path: PathBuf,
    pub content: String,
    /// blake3 do conteúdo real — distingue mudança real de touch sem
    /// conteúdo novo.
    pub content_hash: String,
    /// Presentes só se o chunk veio de um arquivo convertido de formato
    /// não-MD — contrato de proveniência obrigatório.
    pub source_original: Option<PathBuf>,
    pub source_page: Option<String>,
    pub source_section: Option<String>,
}

/// blake3 do conteúdo — usado tanto pro hash por-arquivo (dedup de touch)
/// quanto pro `content_hash` de cada chunk.
pub fn hash_content(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

/// Compara o hash atual do arquivo contra o último processado. `None` em
/// `known_hashes` (arquivo nunca visto) sempre conta como mudança real.
/// Pura — sem I/O — testável sem tocar disco. Ver cenário BDD "Touch sem
/// conteúdo novo não dispara reprocessamento".
pub fn is_real_change(
    file_path: &Path,
    content: &str,
    known_hashes: &HashMap<PathBuf, String>,
) -> Option<String> {
    let new_hash = hash_content(content);
    match known_hashes.get(file_path) {
        Some(existing) if existing == &new_hash => None,
        _ => Some(new_hash),
    }
}

/// Port: chunking markdown-aware — nunca corta header, bloco de código ou
/// tabela no meio. Pura/síncrona: é CPU-bound, o chamador decide se roda em
/// `spawn_blocking` (ver Skill lang-rust, regra de não bloquear o runtime).
pub trait MarkdownChunker: Send + Sync {
    fn chunk(&self, file_path: &Path, content: &str) -> Vec<Chunk>;
}

/// Implementação real: divide o documento nos limites de heading (`#`, `##`,
/// ...). Como só heading dispara um novo corte, blocos de código e tabelas
/// — que nunca são eventos de heading — nunca ficam partidos ao meio.
pub struct StructuralMarkdownChunker;

impl MarkdownChunker for StructuralMarkdownChunker {
    fn chunk(&self, file_path: &Path, content: &str) -> Vec<Chunk> {
        let parser = Parser::new_ext(content, Options::ENABLE_TABLES).into_offset_iter();

        let mut boundaries: Vec<usize> = vec![0];
        for (event, range) in parser {
            if let MdEvent::Start(Tag::Heading { .. }) = event {
                if range.start != 0 {
                    boundaries.push(range.start);
                }
            }
        }
        boundaries.push(content.len());
        boundaries.dedup();

        boundaries
            .windows(2)
            .filter_map(|w| {
                let (start, end) = (w[0], w[1]);
                let slice = content.get(start..end)?;
                if slice.trim().is_empty() {
                    return None;
                }
                Some(Chunk {
                    id: Uuid::new_v4(),
                    file_path: file_path.to_path_buf(),
                    content: slice.to_string(),
                    content_hash: hash_content(slice),
                    source_original: None,
                    source_page: None,
                    source_section: None,
                })
            })
            .collect()
    }
}

/// Evento bruto do watcher — o que mudou, sem julgar se é reprocessamento
/// real (isso é responsabilidade de `is_real_change`, mais barato de testar
/// isolado do filesystem).
#[derive(Debug, Clone)]
pub struct FileChangedEvent {
    pub path: PathBuf,
}

/// Watcher da pasta observada — orientado a evento via `notify`, nunca
/// polling em loop (Skill lang-rust, seção 2.4).
pub struct Watcher {
    pub root: PathBuf,
}

impl Watcher {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Sobe o watcher e envia um `FileChangedEvent` por escrita detectada.
    /// TODO(S2-BKD-05): hoje reporta toda escrita — o filtro de mudança real
    /// (`is_real_change`) roda no consumidor do canal, que mantém o mapa de
    /// hashes conhecidos (estado de aplicação, não do watcher em si).
    pub async fn run(
        &self,
        tx: tokio::sync::mpsc::Sender<FileChangedEvent>,
    ) -> Result<(), IngestError> {
        use notify::{Event, RecursiveMode, Watcher as _};

        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::channel::<notify::Result<Event>>(100);

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let _ = raw_tx.blocking_send(res);
        })
        .map_err(|source| IngestError::WatchFailed {
            path: self.root.clone(),
            source,
        })?;

        watcher
            .watch(&self.root, RecursiveMode::Recursive)
            .map_err(|source| IngestError::WatchFailed {
                path: self.root.clone(),
                source,
            })?;

        while let Some(res) = raw_rx.recv().await {
            if let Ok(event) = res {
                if event.kind.is_modify() || event.kind.is_create() {
                    for path in event.paths {
                        if tx.send(FileChangedEvent { path }).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

// TODO(S2-BKD-02): função/adapter que invoca o subprocesso de conversão
// configurável (MarkItDown/Pandoc) para arquivo não-MD, populando
// source_original/source_page/source_section no Chunk resultante.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_muda_com_conteudo_diferente() {
        assert_ne!(hash_content("a"), hash_content("b"));
        assert_eq!(hash_content("a"), hash_content("a"));
    }

    #[test]
    fn touch_sem_mudanca_real_nao_reprocessa() {
        let mut known = HashMap::new();
        let path = PathBuf::from("a.md");
        let h = hash_content("conteúdo");
        known.insert(path.clone(), h);

        assert_eq!(is_real_change(&path, "conteúdo", &known), None);
        assert!(is_real_change(&path, "conteúdo novo", &known).is_some());
    }

    #[test]
    fn arquivo_nunca_visto_conta_como_mudanca() {
        let known = HashMap::new();
        let path = PathBuf::from("novo.md");
        assert!(is_real_change(&path, "qualquer coisa", &known).is_some());
    }

    #[test]
    fn chunking_nunca_corta_bloco_de_codigo_ou_tabela() {
        let content = "\
# Título

Texto antes.

```rust
fn f() {
    // continua
}
```

## Segunda seção

| a | b |
|---|---|
| 1 | 2 |
";
        let chunker = StructuralMarkdownChunker;
        let chunks = chunker.chunk(Path::new("doc.md"), content);

        assert!(
            chunks.len() >= 2,
            "esperava pelo menos 2 chunks (2 headings)"
        );
        for c in &chunks {
            let opens = c.content.matches("```").count();
            assert_eq!(
                opens % 2,
                0,
                "bloco de código cortado no meio: {:?}",
                c.content
            );
        }
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert!(joined.contains("| a | b |") && joined.contains("| 1 | 2 |"));
    }

    #[test]
    fn documento_sem_heading_vira_um_chunk_so() {
        let content = "Só texto solto, sem heading nenhum.\n";
        let chunker = StructuralMarkdownChunker;
        let chunks = chunker.chunk(Path::new("doc.md"), content);
        assert_eq!(chunks.len(), 1);
    }
}
