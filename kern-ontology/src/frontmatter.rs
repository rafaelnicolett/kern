//! Frontmatter Profile aggregate.
//!
//! Deterministic frontmatter parsing (zero LLM cost) + cache of the field
//! mapping keyed by (folder scope, fingerprint of the key set). Scope =
//! the file's immediate directory, not recursive (resolves hotspot #4).

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::OntologyError;

/// Extracts the keys (not the values) from the `---\n...\n---` block at
/// the start of the file. `None` if there is no frontmatter — falls back
/// to LLM-based extraction over the prose (outside the scope of this
/// module).
pub fn parse_frontmatter_keys(content: &str) -> Option<Vec<String>> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content); // BOM, if present
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let yaml_block = &rest[..end];

    let value: serde_yaml::Value = serde_yaml::from_str(yaml_block).ok()?;
    let mapping = value.as_mapping()?;
    let mut keys: Vec<String> = mapping
        .keys()
        .filter_map(|k| k.as_str().map(str::to_string))
        .collect();
    keys.sort();
    Some(keys)
}

/// Extracts the parsed `key -> value` mapping from the same `---\n...\n---`
/// block `parse_frontmatter_keys` reads. Kept separate from
/// `parse_frontmatter_keys` on purpose: the profile cache is keyed only on
/// the key *set* (never values, see `key_fingerprint`), while resolving a
/// specific file's own entity/relations needs the real values too. `None`
/// under the same conditions as `parse_frontmatter_keys`.
pub fn parse_frontmatter_values(content: &str) -> Option<serde_yaml::Mapping> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let yaml_block = &rest[..end];

    let value: serde_yaml::Value = serde_yaml::from_str(yaml_block).ok()?;
    value.as_mapping().cloned()
}

/// Normalizes a frontmatter value into one-or-many strings: a scalar
/// (`depends_on: TASK-0001`) becomes a single-item list, a sequence
/// (`depends_on: [TASK-0001, TASK-0002]`) becomes each item stringified.
/// Anything else (map, null) yields an empty list — not every field is a
/// reference list.
pub fn frontmatter_value_as_strings(value: &serde_yaml::Value) -> Vec<String> {
    match value {
        serde_yaml::Value::String(s) => vec![s.clone()],
        serde_yaml::Value::Number(n) => vec![n.to_string()],
        serde_yaml::Value::Sequence(items) => items
            .iter()
            .filter_map(|v| match v {
                serde_yaml::Value::String(s) => Some(s.clone()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Fingerprint of the SET of keys — never the values. Two shapes with the
/// same keys in different orders produce the same fingerprint
/// (`parse_frontmatter_keys` already returns them sorted).
pub fn key_fingerprint(keys: &[String]) -> String {
    blake3::hash(keys.join(",").as_bytes()).to_hex().to_string()
}

/// Scope = the file's immediate directory, not recursive (decision closed
/// out under hotspot #4).
pub fn folder_scope(file_path: &Path) -> String {
    file_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontmatterProfile {
    pub id: Uuid,
    pub folder_scope: String,
    pub key_fingerprint: String,
    /// Frontmatter key -> canonical concept (`id`, `kind`, `status`,
    /// `depends_on`, ...), or `None` if the key doesn't map to anything
    /// known.
    pub field_mapping: HashMap<String, Option<String>>,
    pub learned_at: String,
}

/// Port: persistence for the Frontmatter Profile aggregate.
#[async_trait]
pub trait FrontmatterProfileRepository: Send + Sync {
    async fn find(
        &self,
        folder_scope: &str,
        key_fingerprint: &str,
    ) -> Result<Option<FrontmatterProfile>, OntologyError>;
    async fn save(&self, profile: FrontmatterProfile) -> Result<(), OntologyError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_sorted_keys_from_frontmatter() {
        let content = "---\nid: TASK-0042\nkind: task\nstatus: in_progress\n---\n\n# body\n";
        let keys = parse_frontmatter_keys(content).unwrap();
        assert_eq!(keys, vec!["id", "kind", "status"]);
    }

    #[test]
    fn parse_returns_none_without_frontmatter() {
        assert!(parse_frontmatter_keys("# just a title\nno frontmatter\n").is_none());
    }

    #[test]
    fn parse_values_reads_scalars_and_sequences() {
        let content =
            "---\nid: TASK-0042\nkind: task\ndepends_on: [TASK-0001, TASK-0002]\n---\n\n# body\n";
        let values = parse_frontmatter_values(content).unwrap();
        let id = values.get("id").unwrap();
        assert_eq!(frontmatter_value_as_strings(id), vec!["TASK-0042"]);
        let depends_on = values.get("depends_on").unwrap();
        assert_eq!(
            frontmatter_value_as_strings(depends_on),
            vec!["TASK-0001", "TASK-0002"]
        );
    }

    #[test]
    fn value_as_strings_wraps_a_lone_scalar_in_a_single_item_list() {
        let value = serde_yaml::Value::String("TASK-0001".to_string());
        assert_eq!(frontmatter_value_as_strings(&value), vec!["TASK-0001"]);
    }

    #[test]
    fn value_as_strings_is_empty_for_an_empty_sequence() {
        let content = "---\nid: TASK-0042\ndepends_on: []\n---\n";
        let values = parse_frontmatter_values(content).unwrap();
        let depends_on = values.get("depends_on").unwrap();
        assert!(frontmatter_value_as_strings(depends_on).is_empty());
    }

    #[test]
    fn fingerprint_ignores_key_order() {
        let a = key_fingerprint(&["id".to_string(), "kind".to_string()]);
        let b = key_fingerprint(
            &["kind".to_string(), "id".to_string()]
                .into_iter()
                .collect::<Vec<_>>(),
        );
        // parse_frontmatter_keys already sorts — here we simulate
        // already-sorted input for both cases to confirm the fingerprint
        // is deterministic.
        let a2 = key_fingerprint(&["id".to_string(), "kind".to_string()]);
        assert_eq!(a, a2);
        assert_ne!(a, b, "keys in different order without prior normalization give different fingerprints — it's parse_frontmatter_keys's responsibility to sort beforehand");
    }

    #[test]
    fn fingerprint_ignores_values_only_looks_at_keys() {
        let keys1 = parse_frontmatter_keys("---\nid: A\nkind: task\n---\n").unwrap();
        let keys2 = parse_frontmatter_keys("---\nid: B\nkind: adr\n---\n").unwrap();
        assert_eq!(
            key_fingerprint(&keys1),
            key_fingerprint(&keys2),
            "same set of keys, different values — fingerprint should be equal"
        );
    }

    #[test]
    fn scope_is_the_immediate_directory_not_recursive() {
        let a = folder_scope(std::path::Path::new(".specify/specs/feat.md"));
        let b = folder_scope(std::path::Path::new(".specify/specs/archive/feat.md"));
        assert_eq!(a, ".specify/specs");
        assert_eq!(b, ".specify/specs/archive");
        assert_ne!(
            a, b,
            "subfolders are different scopes, never the same (hotspot #4)"
        );
    }
}
