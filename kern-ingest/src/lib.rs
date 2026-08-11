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

/// Estimates how many tokens a piece of text would consume against a real
/// model. Deliberately a port, not a single hardcoded approach (see
/// docs/adr/0008) — `HeuristicTokenCounter` is the only implementation
/// shipped today, but the boundary is what lets a real per-model tokenizer
/// be added later without the chunker's control flow changing.
pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;
}

/// chars/4 — a documented approximation, not real tokenization. Always
/// available, zero dependencies. Callers apply their own safety margin
/// against the real budget (see `BudgetAwareMarkdownChunker`) to absorb
/// this heuristic's imprecision, rather than this counter over- or
/// under-estimating on their behalf.
pub struct HeuristicTokenCounter;

impl TokenCounter for HeuristicTokenCounter {
    fn count(&self, text: &str) -> usize {
        text.chars().count().div_ceil(4)
    }
}

/// A byte range in a chunk's content that must never be split — a code
/// block or table. Detected independently per chunk (re-parsing just that
/// chunk's own content), same technique `StructuralMarkdownChunker` already
/// uses for heading boundaries, just tracking `Start`..`End` event pairs
/// instead of single offsets.
fn protected_ranges(content: &str) -> Vec<std::ops::Range<usize>> {
    let parser = Parser::new_ext(content, Options::ENABLE_TABLES).into_offset_iter();
    let mut ranges = Vec::new();
    let mut open_start: Option<usize> = None;

    for (event, range) in parser {
        match event {
            MdEvent::Start(Tag::CodeBlock(_)) | MdEvent::Start(Tag::Table(_)) => {
                open_start = Some(range.start);
            }
            MdEvent::End(pulldown_cmark::TagEnd::CodeBlock)
            | MdEvent::End(pulldown_cmark::TagEnd::Table) => {
                if let Some(start) = open_start.take() {
                    ranges.push(start..range.end);
                }
            }
            _ => {}
        }
    }
    ranges
}

/// True if `[start, end)` overlaps a protected range at all — fully,
/// partially, or the reverse. Any overlap at all means this span isn't
/// safe to sub-split any further without risking cutting into a code
/// block or table, so the caller keeps it whole rather than recursing.
fn touches_protected_range(ranges: &[std::ops::Range<usize>], start: usize, end: usize) -> bool {
    ranges.iter().any(|r| r.start < end && start < r.end)
}

/// Splits `content` on `boundaries` (a sorted list of byte offsets, always
/// including `0` and `content.len()`), skipping any boundary that would
/// land strictly inside a protected range — used at both the paragraph and
/// sentence cascade levels below.
fn split_at_boundaries_outside_protected(
    content: &str,
    mut boundaries: Vec<usize>,
    protected: &[std::ops::Range<usize>],
) -> Vec<std::ops::Range<usize>> {
    boundaries.retain(|&b| {
        b == 0 || b == content.len() || !protected.iter().any(|r| r.start < b && b < r.end)
    });
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries.windows(2).map(|w| w[0]..w[1]).collect()
}

/// Budget-aware decorator over any `MarkdownChunker` — sub-splits any chunk
/// that exceeds the token budget, without touching the inner chunker's own
/// heading-boundary logic (its existing invariants, including "never split
/// a code block or table", keep holding for chunks that never exceed
/// budget in the first place).
///
/// Cascade for an over-budget chunk: paragraph boundaries (blank lines)
/// outside any protected range, then sentence boundaries within a
/// still-over-budget paragraph, then a hard UTF-8-safe cut as the absolute
/// last resort. A protected range (code block/table) that alone exceeds
/// budget is kept whole and logged — the "never split a code block"
/// invariant wins over the budget; a real downstream rejection at that
/// point is a correctly-surfaced failure, not a chunker bug.
pub struct BudgetAwareMarkdownChunker<C: MarkdownChunker> {
    inner: C,
    counter: Box<dyn TokenCounter>,
    budget_tokens: usize,
}

/// Absorbs `HeuristicTokenCounter`'s imprecision — a chunk estimated right
/// at the real budget could, with real tokenization, land slightly over it.
const BUDGET_SAFETY_MARGIN: f32 = 0.75;

impl<C: MarkdownChunker> BudgetAwareMarkdownChunker<C> {
    pub fn new(inner: C, counter: Box<dyn TokenCounter>, budget_tokens: usize) -> Self {
        Self {
            inner,
            counter,
            budget_tokens,
        }
    }

    fn effective_budget(&self) -> usize {
        ((self.budget_tokens as f32) * BUDGET_SAFETY_MARGIN) as usize
    }

    /// Re-splits one oversized chunk into one or more chunks, none
    /// exceeding `effective_budget` except where a protected range makes
    /// that impossible.
    fn sub_split(&self, chunk: Chunk, effective_budget: usize) -> Vec<Chunk> {
        if self.counter.count(&chunk.content) <= effective_budget {
            return vec![chunk];
        }

        let protected = protected_ranges(&chunk.content);
        let paragraph_boundaries = paragraph_boundaries(&chunk.content);
        let spans =
            split_at_boundaries_outside_protected(&chunk.content, paragraph_boundaries, &protected);

        spans
            .into_iter()
            .flat_map(|span| {
                let piece = &chunk.content[span.clone()];
                if piece.trim().is_empty() {
                    return vec![];
                }
                if self.counter.count(piece) <= effective_budget {
                    return vec![self.make_chunk(&chunk, piece)];
                }
                // Still over budget: a single paragraph (or the only span,
                // if the chunk has no paragraph breaks at all) that alone
                // exceeds the budget. If it touches a protected range at
                // all, sentence-splitting isn't safe (it has no idea where
                // the protected range's boundaries are) — keep it whole
                // and log instead.
                if touches_protected_range(&protected, span.start, span.end) {
                    tracing::warn!(
                        file = %chunk.file_path.display(),
                        tokens = self.counter.count(piece),
                        budget = effective_budget,
                        "a code block or table exceeds the token budget on its own — \
                         keeping it whole rather than splitting it; the embedding \
                         backend may still reject it"
                    );
                    return vec![self.make_chunk(&chunk, piece)];
                }
                self.sub_split_by_sentence(&chunk, piece, effective_budget)
            })
            .collect()
    }

    fn sub_split_by_sentence(
        &self,
        original: &Chunk,
        piece: &str,
        effective_budget: usize,
    ) -> Vec<Chunk> {
        let boundaries = sentence_boundaries(piece);
        let spans = split_at_boundaries_outside_protected(piece, boundaries, &[]);

        spans
            .into_iter()
            .flat_map(|span| {
                let sentence_piece = &piece[span];
                if sentence_piece.trim().is_empty() {
                    return vec![];
                }
                if self.counter.count(sentence_piece) <= effective_budget {
                    vec![self.make_chunk(original, sentence_piece)]
                } else {
                    // Absolute last resort: no paragraph or sentence break
                    // narrow enough on its own (e.g. one giant unbroken
                    // run of text) — hard-cut at a UTF-8-safe boundary
                    // nearest the target budget.
                    hard_cut(sentence_piece, effective_budget, self.counter.as_ref())
                        .into_iter()
                        .map(|p| self.make_chunk(original, p))
                        .collect()
                }
            })
            .collect()
    }

    fn make_chunk(&self, original: &Chunk, content: &str) -> Chunk {
        Chunk {
            id: Uuid::new_v4(),
            file_path: original.file_path.clone(),
            content: content.to_string(),
            content_hash: hash_content(content),
            source_original: original.source_original.clone(),
            source_page: original.source_page.clone(),
            source_section: original.source_section.clone(),
        }
    }
}

impl<C: MarkdownChunker> MarkdownChunker for BudgetAwareMarkdownChunker<C> {
    fn chunk(&self, file_path: &Path, content: &str) -> Vec<Chunk> {
        let effective_budget = self.effective_budget();
        self.inner
            .chunk(file_path, content)
            .into_iter()
            .flat_map(|c| self.sub_split(c, effective_budget))
            .collect()
    }
}

/// Blank-line-separated paragraph boundaries, as byte offsets (always
/// including `0` and `content.len()`).
fn paragraph_boundaries(content: &str) -> Vec<usize> {
    let mut boundaries = vec![0];
    let bytes = content.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
            boundaries.push(i + 2);
        }
        i += 1;
    }
    boundaries.push(content.len());
    boundaries
}

/// `. `/`! `/`? ` followed by whitespace, as byte offsets — a heuristic,
/// not real sentence segmentation (no abbreviation/acronym handling), only
/// used as a last-resort sub-split within an already-oversized paragraph.
fn sentence_boundaries(content: &str) -> Vec<usize> {
    let mut boundaries = vec![0];
    let bytes = content.as_bytes();
    for i in 0..bytes.len() {
        if (bytes[i] == b'.' || bytes[i] == b'!' || bytes[i] == b'?')
            && bytes.get(i + 1).is_some_and(|b| b.is_ascii_whitespace())
        {
            boundaries.push(i + 1);
        }
    }
    boundaries.push(content.len());
    boundaries
}

/// Absolute last resort: no paragraph/sentence break narrow enough on its
/// own — cuts at a UTF-8 char boundary nearest the token budget, walking
/// backward from the byte-length estimate the heuristic counter implies.
fn hard_cut<'a>(
    text: &'a str,
    effective_budget: usize,
    counter: &dyn TokenCounter,
) -> Vec<&'a str> {
    if counter.count(text) <= effective_budget || text.is_empty() {
        return vec![text];
    }

    // HeuristicTokenCounter is chars/4 — invert it to a target char count,
    // then find the nearest valid UTF-8 boundary at or before that many
    // bytes (chars are usually 1 byte in the corpora this targets, but
    // never assume — always re-validate with is_char_boundary).
    let target_chars = effective_budget * 4;
    let mut cut = text
        .char_indices()
        .nth(target_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    if cut == 0 {
        // Pathological: budget too small for even one char at this
        // estimate — take at least something rather than loop forever.
        cut = text.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
    }

    let (head, tail) = text.split_at(cut);
    let mut result = vec![head];
    result.extend(hard_cut(tail, effective_budget, counter));
    result
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

    #[test]
    fn heuristic_token_counter_estimates_roughly_chars_over_four() {
        let counter = HeuristicTokenCounter;
        assert_eq!(counter.count(""), 0);
        assert_eq!(counter.count("abcd"), 1);
        assert_eq!(counter.count("abcde"), 2); // ceil(5/4)
    }

    fn budget_chunker(
        budget_tokens: usize,
    ) -> BudgetAwareMarkdownChunker<StructuralMarkdownChunker> {
        BudgetAwareMarkdownChunker::new(
            StructuralMarkdownChunker,
            Box::new(HeuristicTokenCounter),
            budget_tokens,
        )
    }

    #[test]
    fn chunk_under_budget_is_returned_unchanged() {
        let content = "# Title\n\nShort paragraph, well under any real budget.\n";
        let chunker = budget_chunker(1000);
        let chunks = chunker.chunk(Path::new("doc.md"), content);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("Short paragraph"));
    }

    /// This is the direct empirical closure of the originally-reported
    /// bug: a document with one section big enough to exceed a small
    /// model's real context window (confirmed against real local Ollama's
    /// all-minilm deployment — see kern-model's capability tests) must
    /// come out as multiple sub-budget chunks, not one giant one.
    #[test]
    fn oversized_chunk_is_split_at_paragraph_boundaries_and_stays_under_budget() {
        let paragraph = "This paragraph repeats itself to blow past a tiny budget. ".repeat(20);
        let content = format!("# Title\n\n{paragraph}\n\nSecond paragraph, also long enough to matter on its own here.\n");
        let budget = 50; // small budget, real safety margin still applies
        let chunker = budget_chunker(budget);
        let chunks = chunker.chunk(Path::new("doc.md"), &content);

        assert!(
            chunks.len() > 1,
            "expected the oversized chunk to be sub-split"
        );
        let counter = HeuristicTokenCounter;
        let effective = (budget as f32 * 0.75) as usize;
        for c in &chunks {
            assert!(
                counter.count(&c.content) <= effective,
                "chunk exceeds the effective budget ({} tokens, budget {effective}): {:?}",
                counter.count(&c.content),
                c.content
            );
        }
        // Content survives the split, just spread across more chunks.
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert!(joined.contains("Second paragraph"));
    }

    #[test]
    fn oversized_paragraph_with_no_blank_lines_falls_back_to_sentence_split() {
        // One long "paragraph" (no blank line anywhere) made of many short
        // sentences — must fall back past the paragraph level to sentences.
        let content = format!("# T\n\n{}", "A short sentence here. ".repeat(30));
        let budget = 40;
        let chunker = budget_chunker(budget);
        let chunks = chunker.chunk(Path::new("doc.md"), &content);

        assert!(chunks.len() > 1, "expected sentence-level sub-splitting");
        let counter = HeuristicTokenCounter;
        let effective = (budget as f32 * 0.75) as usize;
        for c in &chunks {
            assert!(counter.count(&c.content) <= effective, "{:?}", c.content);
        }
    }

    #[test]
    fn text_with_no_paragraph_or_sentence_breaks_falls_back_to_hard_cut() {
        // No blank lines, no sentence-ending punctuation at all.
        let content = format!("# T\n\n{}", "x".repeat(500));
        let budget = 20;
        let chunker = budget_chunker(budget);
        let chunks = chunker.chunk(Path::new("doc.md"), &content);

        assert!(
            chunks.len() > 1,
            "expected a hard cut to still sub-split the run"
        );
        let counter = HeuristicTokenCounter;
        let effective = (budget as f32 * 0.75) as usize;
        for c in &chunks {
            assert!(counter.count(&c.content) <= effective, "{:?}", c.content);
        }
        // No data lost: every 'x' still present across the pieces.
        let total_x: usize = chunks.iter().map(|c| c.content.matches('x').count()).sum();
        assert_eq!(total_x, 500);
    }

    /// The invariant `chunking_never_splits_a_code_block_or_table` already
    /// covers `StructuralMarkdownChunker` on its own — this proves it keeps
    /// holding once budget pressure is added on top, with a budget small
    /// enough that everything else around the code block/table gets
    /// sub-split.
    #[test]
    fn budget_aware_chunker_still_never_splits_a_code_block_or_table() {
        let content = "\
# Title

Text before, repeated enough to matter for the budget in this test case here.

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
        let chunker = budget_chunker(15);
        let chunks = chunker.chunk(Path::new("doc.md"), content);

        for c in &chunks {
            let opens = c.content.matches("```").count();
            assert_eq!(opens % 2, 0, "code block split in half: {:?}", c.content);
        }
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert!(joined.contains("| a | b |") && joined.contains("| 1 | 2 |"));
        assert!(joined.contains("fn f()"));
    }

    /// A code block that alone exceeds the budget is kept whole rather
    /// than split — the invariant wins over the budget. It's allowed to
    /// still be over budget in the output; that's a correctly-surfaced
    /// downstream concern, not something the chunker silently hides by
    /// corrupting the code block.
    #[test]
    fn protected_range_alone_exceeding_budget_is_kept_whole() {
        let big_code = "line_of_code();\n".repeat(50);
        let content = format!("# T\n\n```rust\n{big_code}```\n");
        let chunker = budget_chunker(10);
        let chunks = chunker.chunk(Path::new("doc.md"), &content);

        let code_chunk = chunks
            .iter()
            .find(|c| c.content.contains("```"))
            .expect("the code block should appear whole in some chunk");
        assert_eq!(code_chunk.content.matches("```").count(), 2);
        assert!(code_chunk.content.contains("line_of_code()"));
    }
}
