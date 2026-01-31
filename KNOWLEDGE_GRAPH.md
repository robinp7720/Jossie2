# Jossie Knowledge Graph

This document outlines the design and implementation of a local Knowledge Graph (KG) for Jossie.

## 1. Goal
To enable Jossie to understand and reason about relationships between entities (people, projects, concepts, places) mentioned in conversations, surpassing the limitations of simple vector or keyword search.

**Example Query:** "How is Robin related to the 'Apollo' project?"
*   **Without KG:** Searches for messages containing "Robin" and "Apollo", potentially missing the link.
*   **With KG:** Traverses `Robin --(created)--> Project X --(sub-project-of)--> Apollo`.

## 2. Architecture

### 2.1 Storage (SQLite)
We will extend `jossie-db` with a lightweight graph schema consisting of Nodes and Edges.

**Table: `graph_nodes`**
*   `id` (TEXT, PK): Unique identifier (UUID or normalized name).
*   `label` (TEXT): Entity name (e.g., "Robin", "Apollo", "Rust").
*   `type` (TEXT): Category (e.g., "Person", "Project", "Technology").
*   `properties` (TEXT): JSON blob for extra attributes (e.g., email, status).
*   `created_at` (TEXT)
*   `updated_at` (TEXT)

**Table: `graph_edges`**
*   `id` (TEXT, PK): UUID.
*   `source_id` (TEXT, FK): `graph_nodes.id`.
*   `target_id` (TEXT, FK): `graph_nodes.id`.
*   `relation` (TEXT): The relationship type (e.g., "WORKS_ON", "CREATED", "FRIEND_OF").
*   `weight` (REAL): Confidence score or importance (0.0 - 1.0).
*   `properties` (TEXT): JSON blob.
*   `created_at` (TEXT)

### 2.2 Integration (The "Brain")
The KG is not just a database; it requires active maintenance by the agent.

1.  **Extraction (Write):**
    *   **Trigger:** After each user message (or periodically via a background job).
    *   **Process:** The LLM analyzes the conversation buffer.
    *   **Prompt:** "Identify key entities and relationships in the last message. Return JSON: `[{source: 'Robin', relation: 'likes', target: 'Pizza', type: 'Preference'}]`."
    *   **Action:** `db.graph_upsert(...)`

2.  **Retrieval (Read / RAG):**
    *   **Trigger:** Before generating a response.
    *   **Process:**
        1.  Identify entities in the user's current prompt (e.g., "Apollo").
        2.  Query the KG for neighbors of "Apollo" (1-2 hops).
        3.  Summarize these relationships into the System Prompt.
    *   **Prompt Injection:**
        ```text
        ## Context Graph
        - Apollo is a Project.
        - Robin works on Apollo.
        - Apollo uses Rust.
        ```

## 3. Implementation Plan

### Phase 1: Database Support
*   [ ] Add `graph_nodes` and `graph_edges` to `migrations.sql`.
*   [ ] Implement `GraphNode` and `GraphEdge` structs in `jossie-db`.
*   [ ] Add methods: `upsert_node`, `upsert_edge`, `get_neighbors`.

### Phase 2: Graph Tools
*   [ ] Create a new crate `jossie-integration-graph` (or add to `jossie-core`).
*   [ ] Expose tools for the Agent:
    *   `graph_add_relation(source, relation, target)`
    *   `graph_query(entity_name)`

### Phase 3: Automatic Extraction
*   [ ] Update `jossie-server/src/agent.rs` to include a "Graph Extraction" step.
*   [ ] This can be a separate LLM call (cheaper model) running in parallel or after the main response.

### Phase 4: Visualization (Optional)
*   [ ] Add a `/graph` endpoint to the Web API to visualize the nodes using D3.js or Cytoscape.

## 4. Potential Challenges
*   **Entity Resolution:** Does "Robin" refer to "Robin Decker" or "Robin (Bird)"? The LLM needs context to merge nodes correctly.
*   **Graph Explosion:** Avoiding storing trivial info ("Robin is typing"). Need filtering rules.
*   **Latency:** Adding an extra LLM call for extraction increases latency. Best done asynchronously.
