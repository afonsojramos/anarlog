use std::sync::Arc;

use anlg_db_app::ListSessions;
use anlg_db_execute::TransactionStatement;
use serde_json::json;
use uuid::Uuid;

use crate::{
    CancellationToken, DocumentSnapshot, LibraryPage, LibraryQuery, MAX_DOCUMENT_BYTES,
    OpenSession, RenameSession, Reply, Result, RuntimeHandle, SaveDocument, ServiceError, Services,
    SessionId, SessionSummary, types::failure,
};

impl RuntimeHandle {
    pub fn library(
        &self,
        query: LibraryQuery,
        cancel: CancellationToken,
    ) -> Result<Reply<LibraryPage>> {
        self.read(cancel, move |services| async move {
            let limit = query.limit.clamp(1, 200);
            let mut rows = anlg_db_app::list_sessions(
                services.db.pool(),
                ListSessions {
                    query: Some(&query.search),
                    series_id: None,
                    limit: limit + 1,
                    offset: query.offset,
                },
            )
            .await
            .map_err(failure)?;
            let has_more = rows.len() > limit as usize;
            rows.truncate(limit as usize);
            Ok(LibraryPage {
                items: rows
                    .into_iter()
                    .map(|row| SessionSummary {
                        id: row.id.into(),
                        title: row.title.into(),
                        updated_at: row.updated_at.into(),
                        created_at: row.created_at.into(),
                    })
                    .collect(),
                offset: query.offset,
                has_more,
            })
        })
    }

    pub fn open_session(
        &self,
        id: SessionId,
        cancel: CancellationToken,
    ) -> Result<Reply<OpenSession>> {
        self.read(cancel, move |services| async move {
            load_session(&services, id).await
        })
    }

    pub fn create_note(&self, title: Arc<str>) -> Result<Reply<OpenSession>> {
        self.submit(move |services| async move {
            let id = Uuid::new_v4().to_string();
            let document_id = Uuid::new_v4().to_string();
            services
                .executor
                .execute_transaction(vec![
                    TransactionStatement {
                        sql: "INSERT INTO sessions (id, title, kind) VALUES (?, ?, 'note')".into(),
                        params: vec![json!(id), json!(title)],
                        expected_rows_affected: Some(1),
                    },
                    TransactionStatement {
                        sql:
                            "INSERT INTO session_documents (id, session_id, body) VALUES (?, ?, ?)"
                                .into(),
                        params: vec![
                            json!(document_id),
                            json!(id),
                            json!({"type":"doc","content":[{"type":"paragraph"}]})
                                .to_string()
                                .into(),
                        ],
                        expected_rows_affected: Some(1),
                    },
                ])
                .await
                .map_err(failure)?;
            load_session(&services, id.into()).await
        })
    }

    pub fn rename_session(&self, input: RenameSession) -> Result<Reply<OpenSession>> {
        self.submit(move |services| async move {
            services.executor.execute_transaction(vec![TransactionStatement {
                sql: "UPDATE sessions SET title = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ? AND title = ? AND updated_at = ? AND deleted_at IS NULL AND locked = 0".into(),
                params: vec![json!(input.title), json!(input.base.id), json!(input.base.title), json!(input.base.updated_at)],
                expected_rows_affected: Some(1),
            }]).await.map_err(map_write_error)?;
            load_session(&services, input.base.id).await
        })
    }

    pub fn save_document(&self, input: SaveDocument) -> Result<Reply<DocumentSnapshot>> {
        if input.body.len() > MAX_DOCUMENT_BYTES {
            return Err(ServiceError::Unsupported(
                "Document exceeds the 16 MiB save limit; original preserved.".into(),
            ));
        }
        self.submit(move |services| async move {
            if input.base.body_format.as_ref() != "prosemirror_json" {
                return Err(ServiceError::Unsupported("Document format is read-only.".into()));
            }
            let body: serde_json::Value = serde_json::from_str(&input.body).map_err(failure)?;
            if body.get("type").and_then(serde_json::Value::as_str) != Some("doc") {
                return Err(ServiceError::Unsupported("Expected a ProseMirror doc root.".into()));
            }
            let row = services.executor.execute(
                "UPDATE session_documents SET body = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ? AND session_id = ? AND body = ? AND body_format = ? AND updated_at = ? AND deleted_at IS NULL AND EXISTS (SELECT 1 FROM sessions WHERE id = session_documents.session_id AND deleted_at IS NULL AND locked = 0) RETURNING id, session_id, body_format, body, updated_at".into(),
                vec![json!(input.body), json!(input.base.id), json!(input.base.session_id), json!(input.base.body), json!(input.base.body_format), json!(input.base.updated_at)],
            ).await.map_err(failure)?.into_iter().next().ok_or(ServiceError::Conflict)?;
            serde_json::from_value(row).map_err(failure)
        })
    }
}

fn map_write_error(error: anlg_db_execute::Error) -> ServiceError {
    match error {
        anlg_db_execute::Error::UnexpectedRowsAffected { .. } => ServiceError::Conflict,
        error => failure(error),
    }
}

async fn load_session(services: &Services, id: SessionId) -> Result<OpenSession> {
    let row = anlg_db_app::get_session(services.db.pool(), &id.0)
        .await
        .map_err(failure)?
        .ok_or(ServiceError::Conflict)?;
    let note = anlg_db_app::get_session_note(services.db.pool(), &id.0)
        .await
        .map_err(failure)?
        .map(|row| DocumentSnapshot {
            id: row.id.into(),
            session_id: row.session_id.into(),
            body_format: row.body_format.into(),
            body: row.body.into(),
            updated_at: row.updated_at.into(),
        });
    Ok(OpenSession {
        summary: SessionSummary {
            id,
            title: row.title.into(),
            updated_at: row.updated_at.into(),
            created_at: row.created_at.into(),
        },
        note,
    })
}
