# Jossie Knowledge Graph

This document describes the **current** Knowledge Graph (KG) implementation in Jossie, along with the visualization endpoint.

## 1. Goal
Enable Jossie to reason about relationships between entities (people, projects, concepts, places) beyond keyword or vector search.

## 2. Current Architecture (Implemented)

### 2.1 Storage (SQLite)
Implemented in `crates/jossie-db/migrations.sql` and `crates/jossie-db/src/lib.rs`.

**Table: `graph_nodes`**
- `id` (TEXT, PK): Stable identifier (typically normalized label).
- `label` (TEXT): Display name.
- `type` (TEXT): Category (Person, Project, Technology, etc.).
- `properties` (TEXT): JSON blob for attributes.
- `created_at`, `updated_at` (TEXT, RFC3339).

**Table: `graph_edges`**
- `id` (TEXT, PK): UUID.
- `source_id`, `target_id` (TEXT, FK → `graph_nodes.id`).
- `relation` (TEXT): Relationship type (WORKS_ON, CREATED, etc.).
- `weight` (REAL): Confidence/strength.
- `properties` (TEXT): JSON blob.
- `created_at`, `updated_at` (TEXT, RFC3339).

**Database APIs (implemented)**
- `graph_upsert_node`, `graph_upsert_edge`
- `graph_find_nodes` (label LIKE)
- `graph_get_neighbors`
- `graph_list_nodes`, `graph_list_edges`

### 2.2 Integration Tools (Implemented)
A dedicated crate exists: `crates/jossie-integration-graph` and is registered in `src/main.rs`.

**Tools exposed to the agent:**
- `graph_upsert_node`
- `graph_add_relation`
- `graph_search`

### 2.3 Automatic Extraction (Implemented)
In `crates/jossie-server/src/agent.rs`:

1. After the assistant responds, a **background LLM call** extracts nodes/edges from the latest user + assistant turn.
2. Parsed nodes/edges are upserted into the KG.

Extraction prompt expects JSON:
```json
{
  "nodes": [{"id": "...", "label": "...", "type": "..."}],
  "edges": [{"source": "...", "target": "...", "relation": "..."}]
}
```

### 2.4 Graph Context Injection (Implemented)
Before generating a response, the agent builds a **Context Graph** block:

- Heuristically extracts candidate entities from the user message (quoted phrases + token runs).
- Looks up matching nodes and neighbors.
- Injects the relationships into the System Prompt.

This gives the LLM structured graph context during response generation.

## 3. Visualization (Implemented)
A graph visualization page is available:

- **UI:** `GET /graph` (public HTML page)
- **Data:** `GET /api/graph` (auth-protected)

The `/graph` page uses a D3 force-directed layout and pulls data from `/api/graph` with the same Bearer token used by the chat UI. It supports:
- Live reloads
- Filtering by node name/type
- Zoom and pan

**Response shape from `/api/graph`:**
```json
{
  "nodes": [
    {"id":"robin","label":"Robin","node_type":"Person","properties":{}}
  ],
  "edges": [
    {"id":"...","source_id":"robin","target_id":"apollo","relation":"WORKS_ON","weight":1.0,"properties":{}}
  ]
}
```

## 4. Known Gaps / Next Steps
- Entity resolution is naive (IDs are caller-supplied; no merge strategy).
- Extraction runs in the background with the main model; no cheaper extractor model or queue.
- Graph growth controls (pruning / TTL) are not implemented.
- Visualization is read-only; no editing UI yet.
