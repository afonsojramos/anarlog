use std::{collections::VecDeque, path::Path, sync::Arc};

use anlg_db_app::ListSessions;
use anlg_db_execute::TransactionStatement;
use serde_json::json;
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use uuid::Uuid;

use crate::{
    CancellationToken, DocumentSnapshot, LibraryPage, LibraryQuery, MAX_DOCUMENT_BYTES,
    OpenSession, RenameSession, Reply, Result, RuntimeHandle, SaveDocument, ServiceError, Services,
    SessionId, SessionSummary, types::failure,
};

const PAGE_STRIDE: u32 = 256;
const CACHE_ROWS: u32 = PAGE_STRIDE + 200;
const CACHE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct Cache {
    observer: SqliteConnection,
    version: i64,
    pages: VecDeque<(Arc<str>, LibraryPage)>,
    bytes: usize,
}

impl Cache {
    pub(crate) async fn new(path: &Path) -> Result<Self> {
        let observer = SqliteConnection::connect_with(
            &SqliteConnectOptions::new().filename(path).read_only(true),
        )
        .await
        .map_err(failure)?;
        Ok(Self {
            observer,
            version: -1,
            pages: VecDeque::new(),
            bytes: 0,
        })
    }

    async fn version(&mut self) -> Result<i64> {
        sqlx::query_scalar("PRAGMA data_version")
            .fetch_one(&mut self.observer)
            .await
            .map_err(failure)
    }

    fn clear(&mut self, version: i64) {
        self.pages.clear();
        self.bytes = 0;
        self.version = version;
    }
}

fn page_slice(page: &LibraryPage, offset: u32, limit: u32) -> LibraryPage {
    let start = (offset - page.offset) as usize;
    let end = (start + limit as usize).min(page.items.len());
    LibraryPage {
        items: page
            .items
            .get(start..end)
            .unwrap_or_default()
            .to_vec()
            .into(),
        offset,
        has_more: end < page.items.len() || page.has_more,
    }
}

impl RuntimeHandle {
    pub fn library(
        &self,
        query: LibraryQuery,
        cancel: CancellationToken,
    ) -> Result<Reply<LibraryPage>> {
        self.read(cancel, move |services| async move {
            let limit = query.limit.clamp(1, 200);
            let offset = query.offset / PAGE_STRIDE * PAGE_STRIDE;
            let mut cache = services.library.lock().await;
            let version = cache.version().await?;
            if version != cache.version {
                cache.clear(version);
            }
            if let Some(index) = cache.pages.iter().position(|(search, page)| {
                search.as_ref() == query.search.as_ref() && page.offset == offset
            }) {
                let entry = cache.pages.remove(index).expect("cache index exists");
                let result = page_slice(&entry.1, query.offset, limit);
                cache.pages.push_front(entry);
                return Ok(result);
            }
            let mut rows = anlg_db_app::list_sessions(
                services.db.pool(),
                ListSessions {
                    query: Some(&query.search),
                    series_id: None,
                    limit: CACHE_ROWS + 1,
                    offset,
                },
            )
            .await
            .map_err(failure)?;
            let has_more = rows.len() > CACHE_ROWS as usize;
            rows.truncate(CACHE_ROWS as usize);
            let page = LibraryPage {
                items: rows
                    .into_iter()
                    .map(|row| SessionSummary {
                        id: row.id.into(),
                        title: row.title.into(),
                        updated_at: row.updated_at.into(),
                        created_at: row.created_at.into(),
                    })
                    .collect(),
                offset,
                has_more,
            };
            let result = page_slice(&page, query.offset, limit);
            if cache.version().await? == version {
                let bytes = query.search.len()
                    + page
                        .items
                        .iter()
                        .map(|row| {
                            row.id.0.len()
                                + row.title.len()
                                + row.updated_at.len()
                                + row.created_at.len()
                                + std::mem::size_of::<SessionSummary>()
                        })
                        .sum::<usize>();
                if cache.pages.len() >= 8 || cache.bytes.saturating_add(bytes) > CACHE_BYTES {
                    cache.clear(version);
                }
                if bytes <= CACHE_BYTES {
                    cache.bytes += bytes;
                    cache.pages.push_front((query.search, page));
                }
            }
            Ok(result)
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
                            json!(id),
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
