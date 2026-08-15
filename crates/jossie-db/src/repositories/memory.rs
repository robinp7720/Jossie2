use super::*;

impl Database {
    pub async fn memory_save(&self, key: &str, content: &str, tags: &str) -> anyhow::Result<()> {
        self.memory_save_with_prompt_metadata(key, content, tags, None, None)
            .await
    }

    pub async fn memory_save_with_prompt_metadata(
        &self,
        key: &str,
        content: &str,
        tags: &str,
        prompt_scope: Option<&str>,
        importance: Option<i64>,
    ) -> anyhow::Result<()> {
        let existing = self.memory_prompt_metadata(key).await?;
        let prompt_scope = prompt_scope
            .map(normalize_prompt_scope)
            .unwrap_or_else(|| existing.0.unwrap_or_else(|| "none".to_string()));
        let importance = importance
            .map(normalize_memory_importance)
            .unwrap_or_else(|| existing.1.unwrap_or(0));

        // Delete existing entry if any
        sqlx::query("DELETE FROM memory WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await
            .ok();
        sqlx::query("INSERT INTO memory (key, content, tags) VALUES (?, ?, ?)")
            .bind(key)
            .bind(content)
            .bind(tags)
            .execute(&self.pool)
            .await?;
        let now_str = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR REPLACE INTO memory_metadata (key, created_at, updated_at, prompt_scope, importance) VALUES (?, COALESCE((SELECT created_at FROM memory_metadata WHERE key = ?), ?), ?, ?, ?)",
        )
        .bind(key)
        .bind(key)
        .bind(&now_str)
        .bind(&now_str)
        .bind(prompt_scope)
        .bind(importance)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete a memory entry and all metadata associated with its key.
    ///
    /// Returns `true` when a memory entry existed and was deleted.
    pub async fn memory_delete(&self, key: &str) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;

        let result = sqlx::query("DELETE FROM memory WHERE key = ?")
            .bind(key)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM memory_metadata WHERE key = ?")
            .bind(key)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    pub(super) async fn memory_prompt_metadata(
        &self,
        key: &str,
    ) -> anyhow::Result<(Option<String>, Option<i64>)> {
        #[derive(sqlx::FromRow)]
        struct Row {
            prompt_scope: Option<String>,
            importance: Option<i64>,
        }

        let row = sqlx::query_as::<_, Row>(
            "SELECT prompt_scope, importance FROM memory_metadata WHERE key = ?",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row
            .map(|r| (r.prompt_scope, r.importance))
            .unwrap_or((None, None)))
    }

    pub async fn memory_search(&self, query: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        const LIMIT: usize = 10;
        const FETCH_PER_STRATEGY: usize = 12;

        let mut results = Vec::new();
        let mut seen_keys = HashSet::new();

        for match_query in build_memory_search_queries(query) {
            let rows = match self
                .memory_search_match(&match_query, FETCH_PER_STRATEGY)
                .await
            {
                Ok(rows) => rows,
                Err(err) => {
                    tracing::warn!(
                        "Memory search strategy failed for query {:?}: {err}",
                        match_query
                    );
                    continue;
                }
            };

            for row in rows {
                if seen_keys.insert(row.key.clone()) {
                    results.push(MemoryEntry {
                        key: row.key,
                        content: row.content,
                        tags: row.tags,
                    });
                    if results.len() >= LIMIT {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    pub async fn get_memory(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let row =
            sqlx::query_as::<_, MemoryRow>("SELECT key, content, tags FROM memory WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| MemoryEntry {
            key: r.key,
            content: r.content,
            tags: r.tags,
        }))
    }

    pub async fn get_profile_memories(&self) -> anyhow::Result<HashMap<String, MemoryEntry>> {
        let rows = sqlx::query_as::<_, MemoryRow>(
            "SELECT key, content, tags FROM memory
             WHERE key IN ('agent_profile.soul', 'agent_profile', 'agent_profile.mood',
                           'user_profile', 'frequent_entities')",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let entry = MemoryEntry {
                    key: row.key,
                    content: row.content,
                    tags: row.tags,
                };
                (entry.key.clone(), entry)
            })
            .collect())
    }

    pub async fn memory_list_keys(&self) -> anyhow::Result<Vec<MemoryKeyInfo>> {
        let rows = sqlx::query_as::<_, MemoryKeyRow>(
            "SELECT m.key, mm.created_at, mm.updated_at 
             FROM memory m 
             LEFT JOIN memory_metadata mm ON m.key = mm.key 
             ORDER BY mm.updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| MemoryKeyInfo {
                key: r.key,
                created_at: r.created_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
                updated_at: r.updated_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
            })
            .collect())
    }

    pub async fn memory_list_all(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntryWithMetadata>> {
        let limit = limit.clamp(1, 500);
        let rows = sqlx::query_as::<_, MemoryEntryMetadataRow>(
            "SELECT m.key, m.content, m.tags, mm.created_at, mm.updated_at,
                    COALESCE(mm.prompt_scope, 'none') AS prompt_scope,
                    COALESCE(mm.importance, 0) AS importance
             FROM memory m 
             LEFT JOIN memory_metadata mm ON m.key = mm.key 
             ORDER BY mm.updated_at DESC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| MemoryEntryWithMetadata {
                key: r.key,
                content: r.content,
                tags: r.tags,
                created_at: r.created_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
                updated_at: r.updated_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
                prompt_scope: r.prompt_scope,
                importance: r.importance,
            })
            .collect())
    }

    pub async fn memory_list_for_dashboard(
        &self,
        query: Option<&str>,
        prompt_scope: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntryWithMetadata>> {
        let limit = limit.clamp(1, 100);
        let query = query.unwrap_or("").trim();
        let prompt_scope = prompt_scope.unwrap_or("").trim();
        let has_query = !query.is_empty();
        let has_scope = !prompt_scope.is_empty() && prompt_scope != "all";
        let search = format!("%{query}%");

        let rows = sqlx::query_as::<_, MemoryEntryMetadataRow>(
            "SELECT m.key, m.content, m.tags, mm.created_at, mm.updated_at,
                    COALESCE(mm.prompt_scope, 'none') AS prompt_scope,
                    COALESCE(mm.importance, 0) AS importance
             FROM memory m
             LEFT JOIN memory_metadata mm ON m.key = mm.key
             WHERE (? = 0 OR m.key LIKE ? OR m.content LIKE ? OR m.tags LIKE ?)
               AND (? = 0 OR COALESCE(mm.prompt_scope, 'none') = ?)
             ORDER BY mm.updated_at DESC
             LIMIT ?",
        )
        .bind(if has_query { 1 } else { 0 })
        .bind(&search)
        .bind(&search)
        .bind(&search)
        .bind(if has_scope { 1 } else { 0 })
        .bind(prompt_scope)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| MemoryEntryWithMetadata {
                key: r.key,
                content: r.content,
                tags: r.tags,
                created_at: r.created_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
                updated_at: r.updated_at.unwrap_or_else(|| Utc::now().to_rfc3339()),
                prompt_scope: r.prompt_scope,
                importance: r.importance,
            })
            .collect())
    }

    pub async fn memory_stats(&self) -> anyhow::Result<MemoryStats> {
        #[derive(sqlx::FromRow)]
        struct Row {
            total: i64,
            prompt_ready: i64,
        }

        let row = sqlx::query_as::<_, Row>(
            "SELECT COUNT(*) AS total,
                    COALESCE(SUM(CASE WHEN COALESCE(mm.prompt_scope, 'none') != 'none' THEN 1 ELSE 0 END), 0) AS prompt_ready
             FROM memory m
             LEFT JOIN memory_metadata mm ON m.key = mm.key",
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(MemoryStats {
            total: row.total,
            prompt_ready: row.prompt_ready,
        })
    }
}
