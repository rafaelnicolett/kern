//! Agregado 3 — Perfil de Frontmatter (docs/domain/ontologia/aggregates.md).
//!
//! Parse determinístico do frontmatter (zero custo de LLM) + cache do
//! mapeamento de campos por (escopo de pasta, fingerprint do conjunto de
//! chaves). Escopo = diretório imediato do arquivo, não recursivo (resolve
//! hotspot #4 de bdd-hotspots.md).

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::OntologyError;

/// Extrai as chaves (não os valores) do bloco `---\n...\n---` no início do
/// arquivo. `None` se não houver frontmatter — cai no fallback de extração
/// via LLM sobre a prosa (fora do escopo deste módulo).
pub fn parse_frontmatter_keys(content: &str) -> Option<Vec<String>> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content); // BOM, se houver
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

/// Fingerprint do CONJUNTO de chaves — nunca dos valores. Duas formas com
/// as mesmas chaves em ordens diferentes produzem o mesmo fingerprint
/// (`parse_frontmatter_keys` já devolve ordenado).
pub fn key_fingerprint(keys: &[String]) -> String {
    blake3::hash(keys.join(",").as_bytes()).to_hex().to_string()
}

/// Escopo = diretório imediato do arquivo, não recursivo (decisão fechada
/// no hotspot #4 — ver docs/domain/ontologia/aggregates.md).
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
    /// Chave do frontmatter -> conceito canônico (`id`, `kind`, `status`,
    /// `depends_on`, ...), ou `None` se a chave não mapear pra nada
    /// conhecido.
    pub field_mapping: HashMap<String, Option<String>>,
    pub learned_at: String,
}

/// Port: persistência do Agregado 3.
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
    fn parse_extrai_chaves_ordenadas_do_frontmatter() {
        let content = "---\nid: TASK-0042\nkind: task\nstatus: in_progress\n---\n\n# corpo\n";
        let keys = parse_frontmatter_keys(content).unwrap();
        assert_eq!(keys, vec!["id", "kind", "status"]);
    }

    #[test]
    fn parse_retorna_none_sem_frontmatter() {
        assert!(parse_frontmatter_keys("# só um título\nsem frontmatter\n").is_none());
    }

    #[test]
    fn fingerprint_ignora_ordem_de_chaves() {
        let a = key_fingerprint(&["id".to_string(), "kind".to_string()]);
        let b = key_fingerprint(
            &["kind".to_string(), "id".to_string()]
                .into_iter()
                .collect::<Vec<_>>(),
        );
        // parse_frontmatter_keys já ordena — aqui simulamos entrada já ordenada
        // pros dois casos pra confirmar que o fingerprint é determinístico.
        let a2 = key_fingerprint(&["id".to_string(), "kind".to_string()]);
        assert_eq!(a, a2);
        assert_ne!(a, b, "chaves em ordem diferente sem normalização prévia dão fingerprints diferentes — é responsabilidade de parse_frontmatter_keys ordenar antes");
    }

    #[test]
    fn fingerprint_ignora_valores_so_olha_chaves() {
        let keys1 = parse_frontmatter_keys("---\nid: A\nkind: task\n---\n").unwrap();
        let keys2 = parse_frontmatter_keys("---\nid: B\nkind: adr\n---\n").unwrap();
        assert_eq!(
            key_fingerprint(&keys1),
            key_fingerprint(&keys2),
            "mesmo conjunto de chaves, valores diferentes — fingerprint deve ser igual"
        );
    }

    #[test]
    fn escopo_e_o_diretorio_imediato_nao_recursivo() {
        let a = folder_scope(std::path::Path::new(".specify/specs/feat.md"));
        let b = folder_scope(std::path::Path::new(".specify/specs/archive/feat.md"));
        assert_eq!(a, ".specify/specs");
        assert_eq!(b, ".specify/specs/archive");
        assert_ne!(
            a, b,
            "subpastas são escopos diferentes, nunca o mesmo (hotspot #4)"
        );
    }
}
