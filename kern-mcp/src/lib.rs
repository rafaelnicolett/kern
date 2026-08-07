//! kern-mcp — servidor MCP (stdio, via `rmcp`) + as 6 tools da superfície de
//! agente. Contrato: docs/architecture/mcp-tool-contract.md (workspace de
//! delivery) — substitui OpenAPI, kern não expõe REST no v0 (docs/adr/0007).

use std::sync::Arc;

use kern_model::EmbeddingProvider;
use kern_ontology::{InstanceRepository, TypeRepository};
use kern_vector::VectorStore;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_handler, tool_router, Json, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// ---------------------------------------------------------------------
// Schemas — espelham docs/architecture/mcp-tool-contract.md
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchHybridInput {
    pub query: String,
    pub top_k: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ScoredChunkDto {
    pub chunk_id: String,
    pub file_path: String,
    pub content: String,
    pub score: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchHybridOutput {
    pub chunks: Vec<ScoredChunkDto>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryByConceptInput {
    pub concept: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EntityDto {
    pub id: String,
    pub canonical_name: String,
    pub type_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RelationDto {
    #[serde(rename = "type")]
    pub type_id: String,
    pub source_entity_id: String,
    pub target_entity_id: String,
    pub evidence_chunk_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QueryByConceptOutput {
    pub entity: EntityDto,
    pub direct_relations: Vec<RelationDto>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetRelatedEntitiesInput {
    pub entity_id: String,
    pub depth: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SubgraphDto {
    pub entities: Vec<EntityDto>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetRelatedEntitiesOutput {
    pub subgraph: SubgraphDto,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EntityTypeDto {
    pub name: String,
    pub instance_count: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RelationTypeDto {
    pub name: String,
    pub status: String,
    pub instance_count: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetOntologySchemaOutput {
    pub entity_types: Vec<EntityTypeDto>,
    pub relation_types: Vec<RelationTypeDto>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExplainRelationInput {
    pub entity_id_a: String,
    pub entity_id_b: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PathStepDto {
    #[serde(rename = "type")]
    pub type_id: String,
    pub from: String,
    pub to: String,
    pub evidence_chunk_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExplainRelationOutput {
    pub path: Vec<PathStepDto>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryOntologicalInput {
    pub question: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EvidenceDto {
    pub chunk_id: String,
    pub excerpt: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QueryOntologicalOutput {
    pub mode: String,
    pub answer: String,
    pub evidence: Vec<EvidenceDto>,
}

// ---------------------------------------------------------------------
// Servidor
// ---------------------------------------------------------------------

#[derive(Clone)]
pub struct KernServer {
    types: Arc<dyn TypeRepository>,
    instances: Arc<dyn InstanceRepository>,
    vector_store: Arc<dyn VectorStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl KernServer {
    pub fn new(
        types: Arc<dyn TypeRepository>,
        instances: Arc<dyn InstanceRepository>,
        vector_store: Arc<dyn VectorStore>,
        embedder: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        Self {
            types,
            instances,
            vector_store,
            embedder,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Busca chunks por similaridade vetorial + keyword boost")]
    async fn search_hybrid(
        &self,
        params: Parameters<SearchHybridInput>,
    ) -> Result<Json<SearchHybridOutput>, String> {
        let input = params.0;
        let embedding = self
            .embedder
            .embed(&input.query)
            .await
            .map_err(|e| e.to_string())?;
        let top_k = input.top_k.unwrap_or(10) as usize;
        let results = self
            .vector_store
            .search_hybrid(&embedding, top_k)
            .await
            .map_err(|e| e.to_string())?;

        Ok(Json(SearchHybridOutput {
            chunks: results
                .into_iter()
                .map(|r| ScoredChunkDto {
                    chunk_id: r.chunk.id.to_string(),
                    file_path: r.chunk.file_path,
                    content: r.chunk.content,
                    score: r.score,
                })
                .collect(),
        }))
    }

    #[tool(description = "Busca entidade por nome/descrição (conceito) + suas relações diretas")]
    async fn query_by_concept(
        &self,
        params: Parameters<QueryByConceptInput>,
    ) -> Result<Json<QueryByConceptOutput>, String> {
        let concept = params.0.concept;
        let matches = self
            .instances
            .find_entities_by_name(&concept)
            .await
            .map_err(|e| e.to_string())?;
        let entity = matches.into_iter().next().ok_or_else(|| {
            format!("ONTOLOGIA.CONCEITO_NAO_ENCONTRADO: nenhuma entidade casa com '{concept}'")
        })?;

        let relations = self
            .instances
            .direct_relations(entity.id)
            .await
            .map_err(|e| e.to_string())?;

        Ok(Json(QueryByConceptOutput {
            entity: EntityDto {
                id: entity.id.to_string(),
                canonical_name: entity.canonical_name,
                type_id: entity.type_id.to_string(),
            },
            direct_relations: relations
                .into_iter()
                .map(|r| RelationDto {
                    type_id: r.type_id.to_string(),
                    source_entity_id: r.source_entity_id.to_string(),
                    target_entity_id: r.target_entity_id.to_string(),
                    evidence_chunk_id: r.evidence_chunk_id.to_string(),
                })
                .collect(),
        }))
    }

    #[tool(description = "Subgrafo local a partir de uma entidade, até a profundidade solicitada")]
    async fn get_related_entities(
        &self,
        params: Parameters<GetRelatedEntitiesInput>,
    ) -> Result<Json<GetRelatedEntitiesOutput>, String> {
        let input = params.0;
        let entity_id = Uuid::parse_str(&input.entity_id)
            .map_err(|_| format!("id de entidade inválido: {}", input.entity_id))?;
        let depth = input.depth.unwrap_or(1);

        let related = self
            .instances
            .related_entities(entity_id, depth)
            .await
            .map_err(|e| e.to_string())?;

        Ok(Json(GetRelatedEntitiesOutput {
            subgraph: SubgraphDto {
                entities: related
                    .into_iter()
                    .map(|e| EntityDto {
                        id: e.id.to_string(),
                        canonical_name: e.canonical_name,
                        type_id: e.type_id.to_string(),
                    })
                    .collect(),
            },
        }))
    }

    #[tool(description = "Lista os tipos de entidade e relação existentes, com contagens")]
    async fn get_ontology_schema(&self) -> Result<Json<GetOntologySchemaOutput>, String> {
        let entity_types = self
            .types
            .list_entity_types()
            .await
            .map_err(|e| e.to_string())?;
        let relation_types = self
            .types
            .list_relation_types()
            .await
            .map_err(|e| e.to_string())?;

        Ok(Json(GetOntologySchemaOutput {
            entity_types: entity_types
                .into_iter()
                .map(|t| EntityTypeDto {
                    name: t.name,
                    instance_count: t.instance_count,
                })
                .collect(),
            relation_types: relation_types
                .into_iter()
                .map(|t| RelationTypeDto {
                    name: t.name,
                    status: match t.status {
                        kern_ontology::RelationTypeStatus::Candidate => "candidate".to_string(),
                        kern_ontology::RelationTypeStatus::Canonical => "canonical".to_string(),
                    },
                    instance_count: t.instance_count,
                })
                .collect(),
        }))
    }

    #[tool(description = "Caminho de relações entre duas entidades, com evidência")]
    async fn explain_relation(
        &self,
        params: Parameters<ExplainRelationInput>,
    ) -> Result<Json<ExplainRelationOutput>, String> {
        let input = params.0;
        let a = Uuid::parse_str(&input.entity_id_a)
            .map_err(|_| format!("id de entidade inválido: {}", input.entity_id_a))?;
        let b = Uuid::parse_str(&input.entity_id_b)
            .map_err(|_| format!("id de entidade inválido: {}", input.entity_id_b))?;

        let path = self
            .instances
            .find_path(a, b, 6)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("ONTOLOGIA.CAMINHO_NAO_ENCONTRADO")?;

        Ok(Json(ExplainRelationOutput {
            path: path
                .into_iter()
                .map(|r| PathStepDto {
                    type_id: r.type_id.to_string(),
                    from: r.source_entity_id.to_string(),
                    to: r.target_entity_id.to_string(),
                    evidence_chunk_id: r.evidence_chunk_id.to_string(),
                })
                .collect(),
        }))
    }

    #[tool(
        description = "Pergunta em linguagem natural: roteia por tipo de relação com evidência, ou cai em busca vetorial"
    )]
    async fn query_ontological(
        &self,
        params: Parameters<QueryOntologicalInput>,
    ) -> Result<Json<QueryOntologicalOutput>, String> {
        let question = params.0.question;
        let question_embedding = self
            .embedder
            .embed(&question)
            .await
            .map_err(|e| e.to_string())?;

        let relation_types = self
            .types
            .list_relation_types()
            .await
            .map_err(|e| e.to_string())?;

        // Roteamento semântico: casa a pergunta contra as descrições
        // canônicas dos tipos de relação antes de cair em busca vetorial
        // (docs/architecture — "A ontologia participa da consulta").
        const ROUTING_THRESHOLD: f32 = 0.3;
        let mut best: Option<(kern_ontology::RelationTypeRecord, f32)> = None;
        for rt in relation_types {
            let desc_embedding = self
                .embedder
                .embed(&rt.description)
                .await
                .map_err(|e| e.to_string())?;
            let score = cosine_similarity(&question_embedding, &desc_embedding);
            if best.as_ref().map(|(_, s)| score > *s).unwrap_or(true) {
                best = Some((rt, score));
            }
        }

        // Tenta achar uma entidade mencionada na pergunta — heurística
        // simples de v0: procura cada palavra/token relevante como
        // substring de nome de entidade.
        let mentioned_entity = {
            let mut found = None;
            for word in question.split_whitespace() {
                let cleaned: String = word
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
                if cleaned.len() < 3 {
                    continue;
                }
                if let Ok(mut matches) = self.instances.find_entities_by_name(&cleaned).await {
                    if !matches.is_empty() {
                        found = Some(matches.remove(0));
                        break;
                    }
                }
            }
            found
        };

        if let (Some((relation_type, score)), Some(entity)) = (&best, &mentioned_entity) {
            if *score >= ROUTING_THRESHOLD {
                let relations = self
                    .instances
                    .direct_relations(entity.id)
                    .await
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|r| r.type_id == relation_type.id)
                    .collect::<Vec<_>>();

                if !relations.is_empty() {
                    let evidence: Vec<EvidenceDto> = relations
                        .iter()
                        .map(|r| EvidenceDto {
                            chunk_id: r.evidence_chunk_id.to_string(),
                            excerpt: "ver chunk de evidência via search_hybrid".to_string(),
                        })
                        .collect();
                    return Ok(Json(QueryOntologicalOutput {
                        mode: "graph_traversal".to_string(),
                        answer: format!(
                            "{} tem {} relação(ões) do tipo '{}'",
                            entity.canonical_name,
                            relations.len(),
                            relation_type.name
                        ),
                        evidence,
                    }));
                }
            }
        }

        // Fallback: nenhum tipo de relação bateu (ou não achou entidade
        // mencionada) — cai em busca vetorial.
        let chunks = self
            .vector_store
            .search_hybrid(&question_embedding, 3)
            .await
            .map_err(|e| e.to_string())?;

        Ok(Json(QueryOntologicalOutput {
            mode: "vector_fallback".to_string(),
            answer: chunks
                .first()
                .map(|c| c.chunk.content.clone())
                .unwrap_or_else(|| "nenhum resultado encontrado".to_string()),
            evidence: chunks
                .into_iter()
                .map(|c| EvidenceDto {
                    chunk_id: c.chunk.id.to_string(),
                    excerpt: c.chunk.content,
                })
                .collect(),
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KernServer {}

#[cfg(test)]
mod tests {
    use super::*;
    use kern_model::OllamaClient;
    use kern_ontology::{SqliteInstanceRepository, SqliteTypeRepository};
    use kern_vector::{ChunkRecord, LanceVectorStore};

    async fn server(dir: &std::path::Path) -> (KernServer, Arc<dyn EmbeddingProvider>) {
        let db_path = dir.join("registry.db");
        let types: Arc<dyn TypeRepository> =
            Arc::new(SqliteTypeRepository::open(&db_path).unwrap());
        let instances: Arc<dyn InstanceRepository> =
            Arc::new(SqliteInstanceRepository::open(&db_path).unwrap());
        let vector_store: Arc<dyn VectorStore> =
            Arc::new(LanceVectorStore::open(dir).await.unwrap());
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(OllamaClient::new("all-minilm"));
        (
            KernServer::new(types, instances, vector_store, embedder.clone()),
            embedder,
        )
    }

    #[tokio::test]
    async fn get_ontology_schema_lista_tipos_semeados() {
        let dir = tempfile::tempdir().unwrap();
        let (srv, _) = server(dir.path()).await;
        srv.types.seed_canonical_vocabulary().await.unwrap();

        let Json(output) = srv.get_ontology_schema().await.unwrap();
        assert_eq!(
            output.relation_types.len(),
            kern_ontology::SEED_RELATION_TYPES.len()
        );
        assert!(output
            .relation_types
            .iter()
            .all(|t| t.status == "canonical"));
    }

    #[tokio::test]
    async fn query_by_concept_encontra_entidade_e_relacoes_diretas() {
        let dir = tempfile::tempdir().unwrap();
        let (srv, _) = server(dir.path()).await;

        let entity_type = srv
            .types
            .find_or_create_entity_type("Crate", "um crate")
            .await
            .unwrap();
        let a = srv
            .instances
            .find_or_create_entity(entity_type.id, "kern-mcp", "docs/a.md")
            .await
            .unwrap();
        let b = srv
            .instances
            .find_or_create_entity(entity_type.id, "kern-ontology", "docs/b.md")
            .await
            .unwrap();
        srv.instances
            .record_relation(kern_ontology::RelationRecord {
                id: Uuid::new_v4(),
                type_id: entity_type.id,
                source_entity_id: a.id,
                target_entity_id: b.id,
                confidence: 0.9,
                evidence_chunk_id: Uuid::new_v4(),
            })
            .await
            .unwrap();

        let Json(output) = srv
            .query_by_concept(Parameters(QueryByConceptInput {
                concept: "kern-mcp".to_string(),
            }))
            .await
            .unwrap();

        assert_eq!(output.entity.canonical_name, "kern-mcp");
        assert_eq!(output.direct_relations.len(), 1);
    }

    #[tokio::test]
    async fn query_by_concept_retorna_erro_para_conceito_inexistente() {
        let dir = tempfile::tempdir().unwrap();
        let (srv, _) = server(dir.path()).await;

        let result = srv
            .query_by_concept(Parameters(QueryByConceptInput {
                concept: "nao-existe".to_string(),
            }))
            .await;

        match result {
            Err(msg) => assert!(msg.contains("CONCEITO_NAO_ENCONTRADO")),
            Ok(_) => panic!("esperava erro CONCEITO_NAO_ENCONTRADO"),
        }
    }

    #[tokio::test]
    async fn explain_relation_retorna_caminho_com_evidencia() {
        let dir = tempfile::tempdir().unwrap();
        let (srv, _) = server(dir.path()).await;

        let entity_type = srv
            .types
            .find_or_create_entity_type("Crate", "um crate")
            .await
            .unwrap();
        let a = srv
            .instances
            .find_or_create_entity(entity_type.id, "a", "a.md")
            .await
            .unwrap();
        let b = srv
            .instances
            .find_or_create_entity(entity_type.id, "b", "b.md")
            .await
            .unwrap();
        srv.instances
            .record_relation(kern_ontology::RelationRecord {
                id: Uuid::new_v4(),
                type_id: entity_type.id,
                source_entity_id: a.id,
                target_entity_id: b.id,
                confidence: 1.0,
                evidence_chunk_id: Uuid::new_v4(),
            })
            .await
            .unwrap();

        let Json(output) = srv
            .explain_relation(Parameters(ExplainRelationInput {
                entity_id_a: a.id.to_string(),
                entity_id_b: b.id.to_string(),
            }))
            .await
            .unwrap();

        assert_eq!(output.path.len(), 1);
    }

    /// Integração real: embed via Ollama (all-minilm) + índice LanceDB real.
    #[tokio::test]
    async fn search_hybrid_via_ollama_e_lancedb_reais() {
        let dir = tempfile::tempdir().unwrap();
        let (srv, embedder) = server(dir.path()).await;

        if !OllamaClient::new("all-minilm").probe().await {
            eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
            return;
        }

        let embedding = embedder
            .embed("motor de ontologia incremental")
            .await
            .unwrap();
        srv.vector_store
            .upsert(ChunkRecord {
                id: Uuid::new_v4(),
                file_path: "docs/ontologia.md".to_string(),
                content: "O motor de ontologia decide merge, novo tipo ou judge.".to_string(),
                embedding,
                content_hash: "hash-1".to_string(),
                updated_at: "2026-08-06T00:00:00Z".to_string(),
            })
            .await
            .unwrap();

        let Json(output) = srv
            .search_hybrid(Parameters(SearchHybridInput {
                query: "motor de ontologia".to_string(),
                top_k: Some(5),
            }))
            .await
            .unwrap();

        assert!(!output.chunks.is_empty());
        assert!(output.chunks[0].content.contains("motor de ontologia"));
    }

    #[tokio::test]
    async fn get_related_entities_respeita_profundidade() {
        let dir = tempfile::tempdir().unwrap();
        let (srv, _) = server(dir.path()).await;

        let entity_type = srv
            .types
            .find_or_create_entity_type("Crate", "um crate")
            .await
            .unwrap();
        let a = srv
            .instances
            .find_or_create_entity(entity_type.id, "a", "a.md")
            .await
            .unwrap();
        let b = srv
            .instances
            .find_or_create_entity(entity_type.id, "b", "b.md")
            .await
            .unwrap();
        let c = srv
            .instances
            .find_or_create_entity(entity_type.id, "c", "c.md")
            .await
            .unwrap();
        srv.instances
            .record_relation(kern_ontology::RelationRecord {
                id: Uuid::new_v4(),
                type_id: entity_type.id,
                source_entity_id: a.id,
                target_entity_id: b.id,
                confidence: 1.0,
                evidence_chunk_id: Uuid::new_v4(),
            })
            .await
            .unwrap();
        srv.instances
            .record_relation(kern_ontology::RelationRecord {
                id: Uuid::new_v4(),
                type_id: entity_type.id,
                source_entity_id: b.id,
                target_entity_id: c.id,
                confidence: 1.0,
                evidence_chunk_id: Uuid::new_v4(),
            })
            .await
            .unwrap();

        let Json(depth1) = srv
            .get_related_entities(Parameters(GetRelatedEntitiesInput {
                entity_id: a.id.to_string(),
                depth: Some(1),
            }))
            .await
            .unwrap();
        assert_eq!(depth1.subgraph.entities.len(), 1);
        assert_eq!(depth1.subgraph.entities[0].canonical_name, "b");

        let Json(depth2) = srv
            .get_related_entities(Parameters(GetRelatedEntitiesInput {
                entity_id: a.id.to_string(),
                depth: Some(2),
            }))
            .await
            .unwrap();
        assert_eq!(depth2.subgraph.entities.len(), 2);
    }

    /// Integração real contra Ollama+LanceDB: consulta que casa com um tipo
    /// de relação canônico e menciona uma entidade existente deveria
    /// rotear por travessia de grafo, não cair no fallback vetorial.
    #[tokio::test]
    async fn query_ontological_roteia_por_tipo_quando_ha_match_forte() {
        let dir = tempfile::tempdir().unwrap();
        let (srv, _) = server(dir.path()).await;

        if !OllamaClient::new("all-minilm").probe().await {
            eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
            return;
        }

        srv.types.seed_canonical_vocabulary().await.unwrap();
        let depends_on = srv
            .types
            .find_relation_type("depends_on")
            .await
            .unwrap()
            .unwrap();
        let entity_type = srv
            .types
            .find_or_create_entity_type("Crate", "um crate")
            .await
            .unwrap();
        let kernmcp = srv
            .instances
            .find_or_create_entity(entity_type.id, "kernmcpxyz", "a.md")
            .await
            .unwrap();
        let kernontology = srv
            .instances
            .find_or_create_entity(entity_type.id, "kernontologyxyz", "b.md")
            .await
            .unwrap();
        srv.instances
            .record_relation(kern_ontology::RelationRecord {
                id: Uuid::new_v4(),
                type_id: depends_on.id,
                source_entity_id: kernmcp.id,
                target_entity_id: kernontology.id,
                confidence: 1.0,
                evidence_chunk_id: Uuid::new_v4(),
            })
            .await
            .unwrap();

        let Json(output) = srv
            .query_ontological(Parameters(QueryOntologicalInput {
                question: "o que kernmcpxyz depends_on?".to_string(),
            }))
            .await
            .unwrap();

        assert_eq!(output.mode, "graph_traversal");
        assert!(!output.evidence.is_empty());
    }

    #[tokio::test]
    async fn query_ontological_cai_em_fallback_vetorial_sem_match() {
        let dir = tempfile::tempdir().unwrap();
        let (srv, embedder) = server(dir.path()).await;

        if !OllamaClient::new("all-minilm").probe().await {
            eprintln!("Ollama não está rodando em :11434 — pulando teste de integração");
            return;
        }

        let embedding = embedder
            .embed("conteúdo qualquer sobre o projeto")
            .await
            .unwrap();
        srv.vector_store
            .upsert(ChunkRecord {
                id: Uuid::new_v4(),
                file_path: "docs/x.md".to_string(),
                content: "conteúdo qualquer sobre o projeto".to_string(),
                embedding,
                content_hash: "hash-x".to_string(),
                updated_at: "2026-08-06T00:00:00Z".to_string(),
            })
            .await
            .unwrap();

        let Json(output) = srv
            .query_ontological(Parameters(QueryOntologicalInput {
                question: "pergunta sem nenhuma entidade conhecida mencionada".to_string(),
            }))
            .await
            .unwrap();

        assert_eq!(output.mode, "vector_fallback");
    }
}
