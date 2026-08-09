#[derive(Deserialize)]
struct ExtractionResult {
    #[serde(default)]
    nodes: Vec<ExtractedNode>,
    #[serde(default)]
    edges: Vec<ExtractedEdge>,
}

#[derive(Deserialize)]
struct ExtractedNode {
    id: String,
    label: String,
    #[serde(rename = "type")]
    node_type: String,
}

#[derive(Deserialize)]
struct ExtractedEdge {
    source: String,
    target: String,
    relation: String,
}
