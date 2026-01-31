use jossie_core::integration::{Integration, ToolDefinition};
use jossie_db::Database;
use serde::Deserialize;
use std::sync::Arc;

pub struct GraphIntegration {
    db: Arc<Database>,
}

impl GraphIntegration {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    async fn add_node(
        &self,
        id: &str,
        label: &str,
        node_type: &str,
        properties: serde_json::Value,
    ) -> anyhow::Result<String> {
        self.db
            .graph_upsert_node(id, label, node_type, &properties)
            .await?;
        Ok(format!("Node '{}' ({}) upserted successfully.", label, id))
    }

    async fn add_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relation: &str,
        weight: f64,
        properties: serde_json::Value,
    ) -> anyhow::Result<String> {
        // Ensure nodes exist first? Ideally yes, but upsert_edge requires foreign keys.
        // The agent should ensure nodes exist. Or we can auto-create placeholders.
        // For now, assume strictness (agent must create nodes).

        self.db
            .graph_upsert_edge(source_id, target_id, relation, weight, &properties)
            .await?;
        Ok(format!(
            "Edge {} --[{}]--> {} created.",
            source_id, relation, target_id
        ))
    }

    async fn query_graph(&self, query: &str) -> anyhow::Result<String> {
        // Simple strategy: find nodes matching query, then get their neighbors.
        let nodes = self.db.graph_find_nodes(query).await?;
        if nodes.is_empty() {
            return Ok("No matching entities found in the graph.".to_string());
        }

        let mut output = String::new();
        for node in nodes {
            output.push_str(&format!(
                "Entity: {} [{}] (ID: {})\n",
                node.label, node.node_type, node.id
            ));
            
            // Print properties if not empty object
            if let Some(obj) = node.properties.as_object() {
                 if !obj.is_empty() {
                     output.push_str(&format!("  Properties: {:?}\n", obj));
                 }
            }

            let neighbors = self.db.graph_get_neighbors(&node.id).await?;
            if neighbors.is_empty() {
                output.push_str("  No relations recorded.\n");
            } else {
                output.push_str("  Relations:\n");
                for n in neighbors {
                    let arrow = if n.direction == "outgoing" { "-->" } else { "<--" };
                    output.push_str(&format!(
                        "    {} [{}] {} ({})\n",
                        arrow, n.relation, n.node.label, n.node.node_type
                    ));
                }
            }
            output.push('\n');
        }

        Ok(output)
    }
}

#[async_trait::async_trait]
impl Integration for GraphIntegration {
    fn name(&self) -> &str {
        "knowledge_graph"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "graph_upsert_node".to_string(),
                description: "Add or update an entity in the Knowledge Graph. Use consistent IDs (e.g. lowercase names).".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Unique ID (e.g., 'robin_decker', 'project_apollo')"},
                        "label": {"type": "string", "description": "Display name (e.g., 'Robin Decker')"},
                        "type": {"type": "string", "description": "Category (Person, Project, Company, etc.)"},
                        "properties": {"type": "object", "description": "Arbitrary JSON attributes"}
                    },
                    "required": ["id", "label", "type"]
                }),
            },
            ToolDefinition {
                name: "graph_add_relation".to_string(),
                description: "Connect two entities in the Knowledge Graph.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "source_id": {"type": "string", "description": "ID of source entity"},
                        "target_id": {"type": "string", "description": "ID of target entity"},
                        "relation": {"type": "string", "description": "Relationship type (WORKS_ON, KNOWS, LOCATED_IN, etc.)"},
                        "weight": {"type": "number", "description": "Confidence/Strength (0.0 - 1.0), default 1.0"},
                        "properties": {"type": "object", "description": "Edge attributes"}
                    },
                    "required": ["source_id", "target_id", "relation"]
                }),
            },
            ToolDefinition {
                name: "graph_search".to_string(),
                description: "Search the Knowledge Graph for entities and their relationships.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Name or partial name of the entity to look up"}
                    },
                    "required": ["query"]
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        tracing::debug!("graph.execute: {tool_name}");
        match tool_name {
            "graph_upsert_node" => {
                #[derive(Deserialize)]
                struct Args { id: String, label: String, #[serde(rename = "type")] node_type: String, #[serde(default)] properties: serde_json::Value }
                let args: Args = serde_json::from_str(arguments)?;
                self.add_node(&args.id, &args.label, &args.node_type, args.properties).await
            }
            "graph_add_relation" => {
                #[derive(Deserialize)]
                struct Args { source_id: String, target_id: String, relation: String, #[serde(default = "default_weight")] weight: f64, #[serde(default)] properties: serde_json::Value }
                fn default_weight() -> f64 { 1.0 }
                let args: Args = serde_json::from_str(arguments)?;
                self.add_edge(&args.source_id, &args.target_id, &args.relation, args.weight, args.properties).await
            }
            "graph_search" => {
                #[derive(Deserialize)]
                struct Args { query: String }
                let args: Args = serde_json::from_str(arguments)?;
                self.query_graph(&args.query).await
            }
            _ => anyhow::bail!("Unknown graph tool: {tool_name}"),
        }
    }
}
