use super::*;

impl Database {
    pub async fn graph_counts(&self) -> anyhow::Result<(i64, i64)> {
        let nodes = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM graph_nodes")
            .fetch_one(&self.pool)
            .await?;
        let edges = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM graph_edges")
            .fetch_one(&self.pool)
            .await?;
        Ok((nodes, edges))
    }

    pub async fn memory_prompt_context(
        &self,
        scope: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryPromptEntry>> {
        let scope = normalize_prompt_scope(scope);
        if scope == "none" {
            return Ok(Vec::new());
        }

        let rows = sqlx::query_as::<_, MemoryPromptRow>(
            "SELECT m.key, m.content, m.tags, mm.prompt_scope, mm.importance, mm.updated_at
             FROM memory m
             INNER JOIN memory_metadata mm ON m.key = mm.key
             WHERE mm.prompt_scope IN (?, 'both')
             ORDER BY mm.importance DESC, mm.updated_at DESC
             LIMIT ?",
        )
        .bind(scope)
        .bind(limit.max(1).min(50) as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| MemoryPromptEntry {
                key: r.key,
                content: r.content,
                tags: r.tags,
                prompt_scope: r.prompt_scope,
                importance: r.importance,
                updated_at: r.updated_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
            })
            .collect())
    }

    pub async fn memory_prompt_search(
        &self,
        scope: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryPromptEntry>> {
        let scope = normalize_prompt_scope(scope);
        if scope == "none" || query.trim().len() < 2 {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        let mut seen_keys = HashSet::new();

        for match_query in build_memory_search_queries(query) {
            let rows = sqlx::query_as::<_, MemoryPromptRow>(
                "SELECT memory.key, memory.content, memory.tags, mm.prompt_scope, mm.importance, mm.updated_at
                 FROM memory
                 INNER JOIN memory_metadata mm ON memory.key = mm.key
                 WHERE memory MATCH ? AND mm.prompt_scope IN (?, 'both')
                 ORDER BY bm25(memory, 8.0, 1.0, 3.0), mm.importance DESC, memory.rowid DESC
                 LIMIT ?",
            )
            .bind(&match_query)
            .bind(&scope)
            .bind(limit.max(1).min(50) as i64)
            .fetch_all(&self.pool)
            .await;

            let rows = match rows {
                Ok(rows) => rows,
                Err(err) => {
                    tracing::warn!(
                        "Prompt memory search strategy failed for query {:?}: {err}",
                        match_query
                    );
                    continue;
                }
            };

            for row in rows {
                if seen_keys.insert(row.key.clone()) {
                    results.push(MemoryPromptEntry {
                        key: row.key,
                        content: row.content,
                        tags: row.tags,
                        prompt_scope: row.prompt_scope,
                        importance: row.importance,
                        updated_at: row.updated_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
                    });
                    if results.len() >= limit {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    pub(super) async fn memory_search_match(
        &self,
        match_query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryRow>> {
        let rows = sqlx::query_as::<_, MemoryRow>(
            "SELECT key, content, tags
             FROM memory
             WHERE memory MATCH ?
             ORDER BY bm25(memory, 8.0, 1.0, 3.0), rowid DESC
             LIMIT ?",
        )
        .bind(match_query)
        .bind(limit.max(1).min(100) as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
