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
            if let Ok(neighbors) = self.db.graph_get_neighbors(&node.id).await {
                if !neighbors.is_empty() {
                    output.push_str(&format!("  {} connections\n", neighbors.len()));
                }
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

        let max_depth = max_depth.max(1).min(3); // Limit depth to prevent explosion

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
            ToolDefinition {
                name: "graph_upsert_node".to_string(),
                description: "Add or update an entity in the Knowledge Graph. Use consistent IDs (e.g. lowercase names).".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Unique ID (e.g., 'robin_decker', 'project_apollo')"},
                        "label": {"type": "string", "description": "Display name (e.g., 'Robin Decker')"},
                        "type": {"type": "string", "description": "Category (Person, Project, Company, etc.)"},
                        "attributes": {
                            "type": "array",
                            "description": "List of attribute entries (use empty array for none)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "key": {"type": "string", "description": "Attribute name"},
                                    "value": {"type": "string", "description": "Attribute value"}
                                },
                                "required": ["key", "value"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["id", "label", "type", "attributes"],
                    "additionalProperties": false
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
                        "attributes": {
                            "type": "array",
                            "description": "List of edge attribute entries (use empty array for none)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "key": {"type": "string", "description": "Attribute name"},
                                    "value": {"type": "string", "description": "Attribute value"}
                                },
                                "required": ["key", "value"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["source_id", "target_id", "relation", "weight", "attributes"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "graph_search".to_string(),
                description: "Search the Knowledge Graph for entities and their relationships. Use this proactively to understand context before answering questions.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Name or partial name of the entity to look up"}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "graph_list_by_type".to_string(),
                description: "List all entities of a specific type (e.g., 'Person', 'Project', 'Company'). Great for getting an overview of all entities in a category.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "entity_type": {"type": "string", "description": "The type/category of entities to list (e.g., 'Person', 'Project', 'Company', 'Event')"}
                    },
                    "required": ["entity_type"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "graph_explore_connections".to_string(),
                description: "Discover how multiple entities are connected through relationships. Finds paths between entities to understand complex connections.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "entities": {
                            "type": "array",
                            "description": "List of entity names to explore connections between (minimum 2)",
                            "items": {"type": "string"}
                        },
                        "max_depth": {
                            "type": "integer",
                            "description": "Maximum relationship hops to traverse (1-3, default 2)",
                            "default": 2
                        }
                    },
                    "required": ["entities"],
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: &str) -> anyhow::Result<String> {
        tracing::debug!("graph.execute: {tool_name}");
        match tool_name {
            "graph_upsert_node" => {
                #[derive(Deserialize)]
                struct Args {
                    id: String,
                    label: String,
                    #[serde(rename = "type")]
                    node_type: String,
                    #[serde(default, alias = "properties")]
                    attributes: serde_json::Value,
                }
                let args: Args = serde_json::from_str(arguments)?;
                let attributes = normalize_attributes(args.attributes);
                self.add_node(&args.id, &args.label, &args.node_type, attributes)
                    .await
            }
            "graph_add_relation" => {
                #[derive(Deserialize)]
                struct Args {
                    source_id: String,
                    target_id: String,
                    relation: String,
                    #[serde(default = "default_weight")]
                    weight: f64,
                    #[serde(default, alias = "properties")]
                    attributes: serde_json::Value,
                }
                fn default_weight() -> f64 {
                    1.0
                }
                let args: Args = serde_json::from_str(arguments)?;
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
            "graph_search" => {
                #[derive(Deserialize)]
                struct Args {
                    query: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.query_graph(&args.query).await
            }
            "graph_list_by_type" => {
                #[derive(Deserialize)]
                struct Args {
                    entity_type: String,
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.list_by_type(&args.entity_type).await
            }
            "graph_explore_connections" => {
                #[derive(Deserialize)]
                struct Args {
                    entities: Vec<String>,
                    #[serde(default = "default_max_depth")]
                    max_depth: usize,
                }
                fn default_max_depth() -> usize {
                    2
                }
                let args: Args = serde_json::from_str(arguments)?;
                self.explore_connections(args.entities, args.max_depth)
                    .await
            }
            _ => anyhow::bail!("Unknown graph tool: {tool_name}"),
        }
    }
}
