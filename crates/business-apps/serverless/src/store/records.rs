//! Record CRUD + read paths (list/query/filter/search/aggregate/patch/
//! bulk/delete-filter). Writes run computed + validate + schema prep;
//! public reads mask `redact` paths (raw variants for internal consumers).

use serde_json::Value as Json;
use turso::params::Params;
use turso::Value as SqlValue;

use serverless_core::{BoardStore, ServerlessError, SrvRecord};

use super::{NO_TENANT, TursoStore, escape_like, highlight, json_to_sql, quote_sql_path};

impl TursoStore {
    pub async fn record_patch(
        &self,
        board_id: &str,
        seq: i64,
        patch: &Json,
        writer: Option<&str>,
    ) -> Result<Json, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare("SELECT payload FROM srv_records WHERE seq = ?")
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![SqlValue::Integer(seq)]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? else {
            return Err(ServerlessError::NotFound("record not found".to_string()));
        };
        let current = row
            .get_value(0).ok()
            .and_then(|v| v.as_text().cloned())
            .unwrap_or_default();
        let current: Json = serde_json::from_str(&current)
            .map_err(|e| ServerlessError::Db(format!("stored payload: {e}")))?;
        let mut updated = current;
        crate::dsl::apply_patch(&mut updated, patch)
            .map_err(ServerlessError::BadPayload)?;
        if !updated.is_object() {
            return Err(ServerlessError::BadPayload("patched payload must be a JSON object".to_string()));
        }
        let text = serde_json::to_string(&updated)
            .map_err(|e| ServerlessError::BadPayload(e.to_string()))?;
        conns
            .conn
            .execute(
                "UPDATE srv_records SET payload = ?, writer = ? WHERE seq = ?",
                Params::Positional(vec![
                    SqlValue::Text(text.clone()),
                    SqlValue::Text(writer.unwrap_or("").to_string()),
                    SqlValue::Integer(seq),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        let _ = Self::audit_append_locked(&conns.conn, board_id, "record.updated", seq, Some(&text)).await;
        let hooks = Self::hooks_locked(&conns.conn).await?;
        let _ = Self::enqueue_hooks(&conns.conn, board_id, "record.updated", seq, &updated, hooks).await;
        drop(_guard);
        let _ = self.event_append(board_id, "record.updated", seq, &text).await;
        self.publish_event(board_id, "record.updated", seq, &text);
        Ok(updated)
    }

    /// Query a JSON path across records (`records query <path>`), returning
    /// rows whose extracted value equals `eq` (when given).
    pub async fn record_query(
        &self,
        board_id: &str,
        path: &str,
        eq: Option<&Json>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SrvRecord>, ServerlessError> {
        let sql_path = crate::dsl::sql_json_path(path).unwrap_or_else(|| "$".to_string());
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut params = vec![SqlValue::Text(sql_path.clone())];
        let mut where_clause = String::new();
        if let Some(v) = eq {
            params.push(json_to_sql(v));
            where_clause = format!(" AND json_extract(payload, ?1) = ?{}", params.len());
        }
        let sql = format!(
            "SELECT seq, payload, created_at, writer FROM srv_records \
             WHERE json_extract(payload, ?1) IS NOT NULL {where_clause} {ttl} \
             ORDER BY created_at DESC LIMIT ?{lim} OFFSET ?{off}",
            ttl = Self::ttl_hide_sql(board_id),
            lim = params.len() + 1,
            off = params.len() + 2,
        );
        let mut stmt = conns
            .conn
            .prepare(&sql)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        params.push(SqlValue::Integer(limit));
        params.push(SqlValue::Integer(offset));
        let mut rows = stmt
            .query(Params::Positional(params))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        Ok(self.redact_records(board_id, Self::rows_to_records(&mut rows).await?))
    }

    /// Filter + sort records by `conds`/`orders` (JSON-path where clauses).
    pub async fn record_filter(
        &self,
        board_id: &str,
        conds: &[crate::dsl::Cond],
        orders: &[(String, bool)],
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SrvRecord>, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let (where_clause, params) = Self::filter_sql(conds);
        let mut order_clause = String::from("ORDER BY created_at DESC");
        if !orders.is_empty() {
            let parts: Vec<String> = orders
                .iter()
                .map(|(f, asc)| {
                    let path = crate::dsl::sql_json_path(f).unwrap_or_else(|| "$".to_string());
                    format!("json_extract(payload, {}) {}", quote_sql_path(&path), if *asc { "ASC" } else { "DESC" })
                })
                .collect();
            order_clause = format!("ORDER BY {}", parts.join(", "));
        }
        let sql = format!(
            "SELECT seq, payload, created_at, writer FROM srv_records {where_clause} {ttl} {order_clause} LIMIT ?{lim} OFFSET ?{off}",
            ttl = Self::ttl_hide_sql(board_id),
            lim = params.len() + 1,
            off = params.len() + 2,
        );
        let mut stmt = conns
            .conn
            .prepare(&sql)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut all = params;
        all.push(SqlValue::Integer(limit));
        all.push(SqlValue::Integer(offset));
        let mut rows = stmt
            .query(Params::Positional(all))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        Ok(self.redact_records(board_id, Self::rows_to_records(&mut rows).await?))
    }

    /// Build the `WHERE` clause + params for a filter (empty → all records).
    pub(crate) fn filter_sql(conds: &[crate::dsl::Cond]) -> (String, Vec<SqlValue>) {
        if conds.is_empty() {
            return (String::new(), Vec::new());
        }
        let mut params: Vec<SqlValue> = Vec::new();
        let mut parts = Vec::new();
        for cond in conds {
            let field = crate::dsl::sql_json_path(&cond.path).unwrap_or_else(|| "$".to_string());
            params.push(SqlValue::Text(field.clone()));
            let fidx = params.len();
            let clause = match &cond.op {
                crate::dsl::Op::Eq(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("lower(json_extract(payload, ?{fidx})) = lower(?{vidx})")
                }
                crate::dsl::Op::Neq(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("lower(json_extract(payload, ?{fidx})) != lower(?{vidx})")
                }
                crate::dsl::Op::Gt(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("json_extract(payload, ?{fidx}) > ?{vidx}")
                }
                crate::dsl::Op::Gte(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("json_extract(payload, ?{fidx}) >= ?{vidx}")
                }
                crate::dsl::Op::Lt(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("json_extract(payload, ?{fidx}) < ?{vidx}")
                }
                crate::dsl::Op::Lte(v) => {
                    params.push(json_to_sql(v));
                    let vidx = params.len();
                    format!("json_extract(payload, ?{fidx}) <= ?{vidx}")
                }
                crate::dsl::Op::Contains(s) => {
                    params.push(SqlValue::Text(format!("%{}%", escape_like(s))));
                    let vidx = params.len();
                    format!("lower(json_extract(payload, ?{fidx})) LIKE ?{vidx} ESCAPE '\\'")
                }
                crate::dsl::Op::NotContains(s) => {
                    params.push(SqlValue::Text(format!("%{}%", escape_like(s))));
                    let vidx = params.len();
                    format!("lower(json_extract(payload, ?{fidx})) NOT LIKE ?{vidx} ESCAPE '\\'")
                }
                crate::dsl::Op::In(vs) => {
                    let placeholders: Vec<String> = vs
                        .iter()
                        .map(|v| {
                            params.push(json_to_sql(v));
                            format!("?{}", params.len())
                        })
                        .collect();
                    format!("lower(json_extract(payload, ?{fidx})) IN ({})", placeholders.join(","))
                }
            };
            parts.push(clause);
        }
        (format!("WHERE {}", parts.join(" AND ")), params)
    }

    /// Full-text-ish substring search with optional `<mark>` highlight.
    pub async fn record_search(
        &self,
        board_id: &str,
        q: &str,
        hl: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SrvRecord>, ServerlessError> {
        let needle = format!("%{}%", escape_like(q.to_lowercase().as_str()));
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let mut stmt = conns
            .conn
            .prepare(
                &format!("SELECT seq, payload, created_at, writer FROM srv_records \
                 WHERE lower(payload) LIKE ?1 ESCAPE '\\' {} \
                 ORDER BY created_at DESC LIMIT ?2 OFFSET ?3", Self::ttl_hide_sql(board_id)),
            )
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(vec![
                SqlValue::Text(needle),
                SqlValue::Integer(limit),
                SqlValue::Integer(offset),
            ]))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = self.redact_records(board_id, Self::rows_to_records(&mut rows).await?);
        if hl && !q.is_empty() {
            for rec in &mut out {
                rec.payload = highlight(&rec.payload, q);
            }
        }
        Ok(out)
    }

    /// Aggregate over a JSON path: count/sum/avg/min/max, optional group_by.
    pub async fn record_aggregate(
        &self,
        board_id: &str,
        op: crate::dsl::Agg,
        field: &str,
        group_by: Option<&str>,
        conds: &[crate::dsl::Cond],
    ) -> Result<Json, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let (where_clause, params) = Self::filter_sql(conds);
        let field_path = crate::dsl::sql_json_path(field).unwrap_or_else(|| "$".to_string());
        let group_path = group_by.map(|g| crate::dsl::sql_json_path(g).unwrap_or_else(|| "$".to_string()));
        let op_sql = op.as_str().to_uppercase();
        let is_count = op == crate::dsl::Agg::Count;
        let agg_sql = |path: &str| {
            if is_count {
                "COUNT(*)".to_string()
            } else {
                format!("{op_sql}(json_extract(payload, {path}))", path = quote_sql_path(path))
            }
        };
        let (select, group_clause): (String, String) = match group_path {
            Some(g) => (
                format!(
                    "json_extract(payload, {}) AS grp, {} AS val",
                    quote_sql_path(&g),
                    agg_sql(&field_path)
                ),
                "GROUP BY grp".to_string(),
            ),
            None => (agg_sql(&field_path), String::new()),
        };
        let sql = format!(
            "SELECT {select} FROM srv_records {where_clause} {ttl} {group_clause}",
            ttl = Self::ttl_hide_sql(board_id),
        );
        let mut stmt = conns
            .conn
            .prepare(&sql)
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut rows = stmt
            .query(Params::Positional(params))
            .await
            .map_err(|e| ServerlessError::Db(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| ServerlessError::Db(e.to_string()))? {
            match group_by {
                Some(_) => {
                    let key = row.get_value(0).ok().and_then(|v| v.as_text().cloned()).unwrap_or_default();
                    let value = row.get_value(1).ok().map(|v| Self::sql_num(&v)).unwrap_or(0.0);
                    out.push(serde_json::json!({ "key": key, "value": value }));
                }
                None => {
                    let value = row.get_value(0).ok().map(|v| Self::sql_num(&v)).unwrap_or(0.0);
                    out.push(serde_json::json!({ "op": op.as_str(), "value": value }));
                }
            }
        }
        Ok(if group_by.is_some() {
            serde_json::json!({ "grouped_by": group_by, "rows": out })
        } else {
            serde_json::json!({ "field": field, "op": op.as_str(), "value": out.first().and_then(|v| v.get("value")).cloned().unwrap_or(Json::Null) })
        })
    }

    /// Insert many records (each gets its own seq + webhook + SSE event).
    pub async fn record_bulk_insert(
        &self,
        board_id: &str,
        payloads: Vec<Json>,
        writer: Option<&str>,
    ) -> Result<Vec<i64>, ServerlessError> {
        let mut seqs = Vec::with_capacity(payloads.len());
        for payload in payloads {
            seqs.push(self.record_insert(NO_TENANT, board_id, &payload, writer, false).await?);
        }
        Ok(seqs)
    }

    /// Delete every record matching the filter; returns how many were removed.
    pub async fn record_delete_filter(
        &self,
        board_id: &str,
        conds: &[crate::dsl::Cond],
    ) -> Result<i64, ServerlessError> {
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let (where_clause, params) = Self::filter_sql(conds);
        let sql = format!("DELETE FROM srv_records {where_clause}");
        let n = conns
            .conn
            .execute(&sql, Params::Positional(params))
            .await
            .map_err(Self::map_db)?;
        if n > 0 {
            let _ = self.event_append(board_id, "record.deleted", 0, "null").await;
            self.publish_event(board_id, "record.deleted", 0, "null");
        }
        Ok(n as i64)
    }

    // ---- hardening: prepare + raw write paths ----------------------------

    /// Materialize `computed` fields, run `validate` rules, then JSON-schema
    /// check — in that order, exactly like zerowrapper's `prepare_payload`.
    /// Returns the prepared payload ready to store.
    pub(crate) async fn prepare_payload(&self, board_id: &str, payload: &Json) -> Result<Json, ServerlessError> {
        let meta = Self::read_meta(board_id).ok().unwrap_or_default();
        let mut prepared = payload.clone();
        if let Some(c) = meta.get("computed_json").filter(|s| !s.is_empty()) {
            let computed: Json = serde_json::from_str(c)
                .map_err(|e| ServerlessError::BadPayload(format!("board {board_id} has invalid computed_json: {e}")))?;
            super::logic::apply_computed(&computed, &mut prepared)
                .map_err(ServerlessError::BadPayload)?;
        }
        if let Some(v) = meta.get("validate_json").filter(|s| !s.is_empty()) {
            let rules: Json = serde_json::from_str(v)
                .map_err(|e| ServerlessError::BadPayload(format!("board {board_id} has invalid validate_json: {e}")))?;
            if let Some(msg) = super::logic::validate_rules(&rules, &prepared).map_err(ServerlessError::BadPayload)? {
                return Err(ServerlessError::BadPayload(msg));
            }
        }
        self.validate_schema(board_id, &prepared).await?;
        Ok(prepared)
    }

    /// JSON-schema validation of a payload against the board's `schema_json`
    /// (no-op when the board has no schema). Formats are not checked.
    pub(crate) async fn validate_schema(&self, board_id: &str, payload: &Json) -> Result<(), ServerlessError> {
        let Some(schema_text) = Self::read_meta(board_id).ok().and_then(|m| m.get("schema_json").cloned()).filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let schema: Json = serde_json::from_str(&schema_text)
            .map_err(|e| ServerlessError::BadPayload(format!("board {board_id} has an invalid schema_json: {e}")))?;
        let compiled = jsonschema::options_for::<jsonschema::json::SerdeJson>()
            .build(&schema)
            .map_err(|e| ServerlessError::BadPayload(format!("board {board_id} has an invalid schema_json: {e}")))?;
        compiled
            .validate(payload)
            .map_err(|e| ServerlessError::BadPayload(format!("payload failed schema: {e}")))?;
        Ok(())
    }

    /// Mask `redact` paths on a payload for public reads (clone — leaves the
    /// stored payload untouched).
    pub(crate) fn redact_payload(&self, board_id: &str, payload: &Json) -> Json {
        let Some(r) = Self::read_meta(board_id).ok().and_then(|m| m.get("redact_json").cloned()).filter(|s| !s.is_empty()) else {
            return payload.clone();
        };
        let Ok(rules) = serde_json::from_str::<Json>(&r) else {
            return payload.clone();
        };
        let mut out = payload.clone();
        super::logic::apply_redact(&rules, &mut out);
        out
    }

    /// Redact every record payload (no-op when the board has no redact config).
    pub(crate) fn redact_records(&self, board_id: &str, records: Vec<SrvRecord>) -> Vec<SrvRecord> {
        let configured = Self::read_meta(board_id)
            .ok()
            .and_then(|m| m.get("redact_json").cloned())
            .is_some_and(|s| !s.is_empty());
        if !configured {
            return records;
        }
        records
            .into_iter()
            .map(|mut r| {
                r.payload = self.redact_payload(board_id, &r.payload);
                r
            })
            .collect()
    }

    /// The raw insert core: unique-key upsert replace or plain INSERT, no
    /// hooks/audit/publish/dispatch — used by the recipe engine (which would
    /// otherwise re-trigger the very recipes that wrote). Returns the seq and
    /// whether an existing row was replaced (upsert hit).
    pub(crate) async fn record_insert_raw(
        &self,
        board_id: &str,
        payload: &Json,
        writer: Option<&str>,
        upsert: bool,
    ) -> Result<(i64, bool), ServerlessError> {
        if !payload.is_object() {
            return Err(ServerlessError::BadPayload("payload must be a JSON object".to_string()));
        }
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let payload_text = serde_json::to_string(payload).map_err(|e| ServerlessError::BadPayload(e.to_string()))?;
        let writer_text = writer.unwrap_or("").to_string();

        if upsert {
            let ukey = Self::read_meta(board_id).ok().and_then(|m| m.get("unique_key").cloned()).filter(|s| !s.is_empty());
            if let Some(key) = ukey {
                let value = payload.get(&key).and_then(Json::as_str);
                let Some(value) = value else {
                    return Err(ServerlessError::BadPayload(format!("upsert: payload has no string property `{key}`")));
                };
                let mut existing = conns
                    .conn
                    .prepare("SELECT seq FROM srv_records WHERE json_extract(payload, ?) = ? LIMIT 1")
                    .await
                    .map_err(|e| ServerlessError::Db(e.to_string()))?;
                let mut rows = existing
                    .query(Params::Positional(vec![
                        SqlValue::Text(format!("$.{key}")),
                        SqlValue::Text(value.to_string()),
                    ]))
                    .await
                    .map_err(|e| ServerlessError::Db(e.to_string()))?;
                let found = rows
                    .next()
                    .await
                    .map_err(|e| ServerlessError::Db(e.to_string()))?
                    .and_then(|row| row.get_value(0).ok().and_then(|v| v.as_integer().copied()));
                if let Some(target) = found {
                    conns
                        .conn
                        .execute(
                            "UPDATE srv_records SET payload = ?, writer = ? WHERE seq = ?",
                            Params::Positional(vec![
                                SqlValue::Text(payload_text.clone()),
                                SqlValue::Text(writer_text.clone()),
                                SqlValue::Integer(target),
                            ]),
                        )
                        .await
                        .map_err(Self::map_db)?;
                    drop(_guard);
                    return Ok((target, true));
                }
            }
        }

        conns
            .conn
            .execute(
                "INSERT INTO srv_records (payload, created_at, writer) VALUES (?, ?, ?)",
                Params::Positional(vec![
                    SqlValue::Text(payload_text.clone()),
                    SqlValue::Text(Self::now_millis()),
                    SqlValue::Text(writer_text),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        let seq = conns.conn.last_insert_rowid();
        drop(_guard);
        Ok((seq, false))
    }

    /// The raw update core: UPDATE or INSERT-when-missing, no effects — the
    /// recipe engine's persist path (also used by `record_set`). Returns
    /// whether the row was created (vs updated).
    pub(crate) async fn record_set_raw(
        &self,
        board_id: &str,
        seq: i64,
        payload: &Json,
        writer: Option<&str>,
    ) -> Result<bool, ServerlessError> {
        if !payload.is_object() {
            return Err(ServerlessError::BadPayload("payload must be a JSON object".to_string()));
        }
        let conns = self.board_conn(board_id).await?;
        let _guard = conns.lock.lock().await;
        let payload_text = serde_json::to_string(payload).map_err(|e| ServerlessError::BadPayload(e.to_string()))?;
        let writer_text = writer.unwrap_or("").to_string();
        let changed = conns
            .conn
            .execute(
                "UPDATE srv_records SET payload = ?, writer = ? WHERE seq = ?",
                Params::Positional(vec![
                    SqlValue::Text(payload_text.clone()),
                    SqlValue::Text(writer_text.clone()),
                    SqlValue::Integer(seq),
                ]),
            )
            .await
            .map_err(Self::map_db)?;
        if changed == 0 {
            conns
                .conn
                .execute(
                    "INSERT INTO srv_records (seq, payload, created_at, writer) VALUES (?, ?, ?, ?)",
                    Params::Positional(vec![
                        SqlValue::Integer(seq),
                        SqlValue::Text(payload_text.clone()),
                        SqlValue::Text(Self::now_millis_i64().to_string()),
                        SqlValue::Text(writer_text),
                    ]),
                )
                .await
                .map_err(Self::map_db)?;
        }
        drop(_guard);
        Ok(changed == 0)
    }
}
