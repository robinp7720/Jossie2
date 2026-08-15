use jossie_core::integration::{Integration, ToolDefinition};
use jossie_db::Database;
use serde::Deserialize;
use std::sync::Arc;

pub struct GraphIntegration {
    db: Arc<Database>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphUpsertNodeArgs {
    id: String,
    label: String,
    #[serde(rename = "type")]
    node_type: String,
    #[serde(alias = "properties")]
    #[schemars(required)]
    attributes: serde_json::Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphAddRelationArgs {
    source_id: String,
    target_id: String,
    relation: String,
    #[schemars(required)]
    weight: f64,
    #[serde(alias = "properties")]
    #[schemars(required)]
    attributes: serde_json::Value,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphNodeIdArgs {
    id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphRelationArgs {
    source_id: String,
    target_id: String,
    relation: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphSearchArgs {
    query: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphListTypeArgs {
    entity_type: String,
}

fn default_graph_depth() -> usize {
    2
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphExploreArgs {
    entities: Vec<String>,
    #[serde(default = "default_graph_depth")]
    max_depth: usize,
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

    async fn delete_node(&self, id: &str) -> anyhow::Result<String> {
        if self.db.graph_delete_node(id).await? {
            Ok(format!(
                "Deleted graph node '{}' and all of its connected relations.",
                id
            ))
        } else {
            anyhow::bail!("No graph node found with ID '{}'", id)
        }
    }

    async fn delete_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relation: &str,
    ) -> anyhow::Result<String> {
        if self
            .db
            .graph_delete_edge(source_id, target_id, relation)
            .await?
        {
            Ok(format!(
                "Deleted relation {} --[{}]--> {}.",
                source_id, relation, target_id
            ))
        } else {
            anyhow::bail!(
                "No relation found from '{}' to '{}' with type '{}'",
                source_id,
                target_id,
                relation
            )
        }
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
            if let Some(obj) = node.properties.as_object()
                && !obj.is_empty()
            {
                output.push_str(&format!("  Properties: {:?}\n", obj));
            }

            let neighbors = self.db.graph_get_neighbors(&node.id).await?;
            if neighbors.is_empty() {
                output.push_str("  No relations recorded.\n");
            } else {
                output.push_str("  Relations:\n");
                for n in neighbors {
                    let arrow = if n.direction == "outgoing" {
                        "-->"
                    } else {
                        "<--"
                    };
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

    async fn list_by_type(&self, entity_type: &str) -> anyhow::Result<String> {
        let nodes = self.db.graph_list_nodes_by_type(entity_type).await?;
        if nodes.is_empty() {
            return Ok(format!("No {} entities found in the graph.", entity_type));
        }

        let mut output = format!("Found {} {} entities:\n\n", nodes.len(), entity_type);
        for node in nodes {
            output.push_str(&format!("- {} (ID: {})\n", node.label, node.id));

            // Show connection count
            if let Ok(neighbors) = self.db.graph_get_neighbors(&node.id).await
                && !neighbors.is_empty()
            {
                output.push_str(&format!("  {} connections\n", neighbors.len()));
            }
        }

        Ok(output)
    }

    async fn explore_connections(
        &self,
        entities: Vec<String>,
        max_depth: usize,
    ) -> anyhow::Result<String> {
        use std::collections::{HashMap, HashSet, VecDeque};

        let max_depth = max_depth.clamp(1, 3); // Limit depth to prevent explosion

        // Find all entity nodes first
        let mut entity_nodes = Vec::new();
        for entity_name in &entities {
            let nodes = self.db.graph_find_nodes(entity_name).await?;
            if let Some(node) = nodes.into_iter().next() {
                entity_nodes.push(node);
            }
        }

        if entity_nodes.len() < 2 {
            return Ok(format!(
                "Need at least 2 entities to explore connections. Found: {}",
                entity_nodes
                    .iter()
                    .map(|n| n.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let mut output = format!(
            "Exploring connections between: {}\n\n",
            entity_nodes
                .iter()
                .map(|n| n.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );

        // BFS to find paths between entities
        let start_id = &entity_nodes[0].id;
        let target_ids: HashSet<String> = entity_nodes[1..].iter().map(|n| n.id.clone()).collect();

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut paths: HashMap<String, Vec<String>> = HashMap::new();

        queue.push_back((start_id.clone(), 0, vec![entity_nodes[0].label.clone()]));
        visited.insert(start_id.clone());

        while let Some((current_id, depth, path)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            if let Ok(neighbors) = self.db.graph_get_neighbors(&current_id).await {
                for neighbor in neighbors {
                    let neighbor_id = neighbor.node.id.clone();

                    // If we found a target, record the path
                    if target_ids.contains(&neighbor_id) {
                        let mut full_path = path.clone();
                        full_path.push(format!("--[{}]-->", neighbor.relation));
                        full_path.push(neighbor.node.label.clone());
                        paths.insert(neighbor_id.clone(), full_path);
                    }

                    if !visited.contains(&neighbor_id) {
                        visited.insert(neighbor_id.clone());
                        let mut new_path = path.clone();
                        new_path.push(format!("--[{}]-->", neighbor.relation));
                        new_path.push(neighbor.node.label.clone());
                        queue.push_back((neighbor_id, depth + 1, new_path));
                    }
                }
            }
        }

        if paths.is_empty() {
            output.push_str("No connections found within the specified depth.\n");
        } else {
            output.push_str(&format!("Found {} connection path(s):\n\n", paths.len()));
            for (target_id, path) in paths {
                if let Some(target_node) = entity_nodes.iter().find(|n| n.id == target_id) {
                    output.push_str(&format!("→ {}:\n  ", target_node.label));
                    output.push_str(&path.join(" "));
                    output.push_str("\n\n");
                }
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    async fn test_graph() -> GraphIntegration {
        let db = Database::new("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        GraphIntegration::new(Arc::new(db))
    }

    #[tokio::test]
    async fn deletes_nodes_and_connected_relations() {
        let graph = test_graph().await;
        graph
            .execute(
                "graph_upsert_node",
                r#"{"id":"robin","label":"Robin","type":"Person","attributes":[]}"#,
            )
            .await
            .unwrap();
        graph
            .execute(
                "graph_upsert_node",
                r#"{"id":"apollo","label":"Apollo","type":"Project","attributes":[]}"#,
            )
            .await
            .unwrap();
        graph
            .execute(
                "graph_add_relation",
                r#"{"source_id":"robin","target_id":"apollo","relation":"WORKS_ON","weight":1,"attributes":[]}"#,
            )
            .await
            .unwrap();

        let result = graph
            .execute("graph_delete_node", r#"{"id":"robin"}"#)
            .await
            .unwrap();
        assert!(result.contains("Deleted graph node"));
        assert_eq!(graph.db.graph_list_edges(10).await.unwrap().len(), 0);
        assert!(
            graph
                .execute("graph_delete_node", r#"{"id":"robin"}"#)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn deletes_exact_relation() {
        let graph = test_graph().await;
        for (id, label) in [("robin", "Robin"), ("apollo", "Apollo")] {
            graph
                .add_node(id, label, "Person", serde_json::json!({}))
                .await
                .unwrap();
        }
        graph
            .add_edge("robin", "apollo", "WORKS_ON", 1.0, serde_json::json!({}))
            .await
            .unwrap();

        assert!(
            graph
                .execute(
                    "graph_delete_relation",
                    r#"{"source_id":"robin","target_id":"apollo","relation":"WORKS_ON"}"#,
                )
                .await
                .unwrap()
                .contains("Deleted relation")
        );
        assert!(
            graph
                .execute(
                    "graph_delete_relation",
                    r#"{"source_id":"robin","target_id":"apollo","relation":"WORKS_ON"}"#,
                )
                .await
                .is_err()
        );
    }
}

fn normalize_attributes(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            let mut map = serde_json::Map::new();
            for item in items {
                if let serde_json::Value::Object(obj) = item {
                    let key = obj
                        .get("key")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if key.is_empty() {
                        continue;
                    }
                    let value = obj
                        .get("value")
                        .cloned()
                        .unwrap_or_else(|| serde_json::Value::String(String::new()));
                    map.insert(key, value);
                }
            }
            serde_json::Value::Object(map)
        }
        serde_json::Value::Object(obj) => serde_json::Value::Object(obj),
        serde_json::Value::Null => serde_json::Value::Object(serde_json::Map::new()),
        other => other,
    }
}

#[async_trait::async_trait]
impl Integration for GraphIntegration {
    fn name(&self) -> &str {
        "knowledge_graph"
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::for_args::<GraphUpsertNodeArgs>(
                "graph_upsert_node",
                "Add or update an entity in the Knowledge Graph. Use consistent IDs (e.g. lowercase names).",
            ),
            ToolDefinition::for_args::<GraphAddRelationArgs>(
                "graph_add_relation",
                "Connect two entities in the Knowledge Graph.",
            ),
            ToolDefinition::for_args::<GraphNodeIdArgs>(
                "graph_delete_node",
                "Permanently delete a Knowledge Graph entity by its exact ID, along with every relation connected to it. Use only when the user explicitly asks to forget it.",
            ),
            ToolDefinition::for_args::<GraphRelationArgs>(
                "graph_delete_relation",
                "Permanently delete one exact Knowledge Graph relation. Use only when the user explicitly asks to remove it.",
            ),
            ToolDefinition::for_args::<GraphSearchArgs>(
                "graph_search",
                "Search the Knowledge Graph for entities and their relationships. Use this proactively to understand context before answering questions.",
            ),
            ToolDefinition::for_args::<GraphListTypeArgs>(
                "graph_list_by_type",
                "List all entities of a specific type (e.g., 'Person', 'Project', 'Company'). Great for getting an overview of all entities in a category.",
            ),
            ToolDefinition::for_args::<GraphExploreArgs>(
                "graph_explore_connections",
                "Discover how multiple entities are connected through relationships. Finds paths between entities to understand complex connections.",
            ),
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        tracing::debug!("graph.execute: {tool_name}");
        match tool_name {
            "graph_upsert_node" => {
                let args: GraphUpsertNodeArgs = serde_json::from_str(arguments)?;
                let attributes = normalize_attributes(args.attributes);
                self.add_node(&args.id, &args.label, &args.node_type, attributes)
                    .await
            }
            "graph_add_relation" => {
                let args: GraphAddRelationArgs = serde_json::from_str(arguments)?;
                let attributes = normalize_attributes(args.attributes);
                self.add_edge(
                    &args.source_id,
                    &args.target_id,
                    &args.relation,
                    args.weight,
                    attributes,
                )
                .await
            }
            "graph_delete_node" => {
                let args: GraphNodeIdArgs = serde_json::from_str(arguments)?;
                self.delete_node(&args.id).await
            }
            "graph_delete_relation" => {
                let args: GraphRelationArgs = serde_json::from_str(arguments)?;
                self.delete_edge(&args.source_id, &args.target_id, &args.relation)
                    .await
            }
            "graph_search" => {
                let args: GraphSearchArgs = serde_json::from_str(arguments)?;
                self.query_graph(&args.query).await
            }
            "graph_list_by_type" => {
                let args: GraphListTypeArgs = serde_json::from_str(arguments)?;
                self.list_by_type(&args.entity_type).await
            }
            "graph_explore_connections" => {
                let args: GraphExploreArgs = serde_json::from_str(arguments)?;
                self.explore_connections(args.entities, args.max_depth)
                    .await
            }
            _ => anyhow::bail!("Unknown graph tool: {tool_name}"),
        }
    }
}
