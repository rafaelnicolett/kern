//! kern-ingest — markdown-aware watcher + chunking.
//!
//! Detects real change (hash, not touch) in the watched folder, converts
//! non-MD formats via a configurable external subprocess, and produces
//! `Chunk`s that kern-vector indexes. Never processes the entire corpus —
//! only the file that changed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pulldown_cmark::{Event as MdEvent, Options, Parser, Tag};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("failed to read file {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("conversion subprocess failed: {0}")]
    ConversionFailed(String),
    #[error("failed to watch folder {path}: {source}")]
    WatchFailed {
        path: PathBuf,
        #[source]
        source: notify::Error,
    },
}

/// Indexed unit — the atomic piece of a file that gets embedded and
/// searched independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub id: Uuid,
    pub file_path: PathBuf,
    pub content: String,
    /// blake3 of the actual content — distinguishes a real change from a
    /// touch with no new content.
    pub content_hash: String,
    /// Present only if the chunk came from a file converted from a non-MD
    /// format — mandatory provenance contract.
    pub source_original: Option<PathBuf>,
    pub source_page: Option<String>,
    pub source_section: Option<String>,
}

/// blake3 of the content — used both for the per-file hash (touch dedup)
/// and for each chunk's `content_hash`.
pub fn hash_content(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

/// Compares the file's current hash to the last one processed. `None` in
/// `known_hashes` (file never seen before) always counts as a real change.
/// Pure — no I/O — testable without touching disk. See the BDD scenario
/// "Touch with no new content does not trigger reprocessing".
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

/// Port: markdown-aware chunking — never cuts a header, code block, or
/// table in half. Pure/synchronous: it's CPU-bound, so the caller decides
/// whether to run it in `spawn_blocking` rather than blocking the async
/// runtime directly.
pub trait MarkdownChunker: Send + Sync {
    fn chunk(&self, file_path: &Path, content: &str) -> Vec<Chunk>;
}

/// Real implementation: splits the document at heading boundaries (`#`,
/// `##`, ...). Since only a heading triggers a new cut, code blocks and
/// tables — which are never heading events — never end up split in half.
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

/// Raw event from the watcher — what changed, without judging whether it's
/// a real reprocessing (that's `is_real_change`'s responsibility, cheaper
/// to test in isolation from the filesystem).
#[derive(Debug, Clone)]
pub struct FileChangedEvent {
    pub path: PathBuf,
}

/// Watcher for the observed folder — event-driven via `notify`, never
/// polling in a loop.
pub struct Watcher {
    pub root: PathBuf,
}

impl Watcher {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Starts the watcher and sends a `FileChangedEvent` for each detected
    /// write. TODO: today it reports every write — the real-change filter
    /// (`is_real_change`) runs in the channel's consumer, which keeps the
    /// map of known hashes (application state, not the watcher's own
    /// state).
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

// TODO: function/adapter that invokes the configurable conversion
// subprocess (MarkItDown/Pandoc) for non-MD files, populating
// source_original/source_page/source_section in the resulting Chunk.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_different_content() {
        assert_ne!(hash_content("a"), hash_content("b"));
        assert_eq!(hash_content("a"), hash_content("a"));
    }

    #[test]
    fn touch_without_real_change_does_not_reprocess() {
        let mut known = HashMap::new();
        let path = PathBuf::from("a.md");
        let h = hash_content("content");
        known.insert(path.clone(), h);

        assert_eq!(is_real_change(&path, "content", &known), None);
        assert!(is_real_change(&path, "new content", &known).is_some());
    }

    #[test]
    fn never_seen_file_counts_as_change() {
        let known = HashMap::new();
        let path = PathBuf::from("new.md");
        assert!(is_real_change(&path, "anything", &known).is_some());
    }

    #[test]
    fn chunking_never_splits_a_code_block_or_table() {
        let content = "\
# Title

Text before.

```rust
fn f() {
    // continues
}
```

## Second section

| a | b |
|---|---|
| 1 | 2 |
";
        let chunker = StructuralMarkdownChunker;
        let chunks = chunker.chunk(Path::new("doc.md"), content);

        assert!(chunks.len() >= 2, "expected at least 2 chunks (2 headings)");
        for c in &chunks {
            let opens = c.content.matches("```").count();
            assert_eq!(opens % 2, 0, "code block split in half: {:?}", c.content);
        }
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert!(joined.contains("| a | b |") && joined.contains("| 1 | 2 |"));
    }

    #[test]
    fn document_without_heading_becomes_a_single_chunk() {
        let content = "Just loose text, no heading at all.\n";
        let chunker = StructuralMarkdownChunker;
        let chunks = chunker.chunk(Path::new("doc.md"), content);
        assert_eq!(chunks.len(), 1);
    }
}
