use std::sync::Arc;

use desktop_runtime::{CancellationToken, Profile, RuntimeHandle, ServiceError};
use futures::executor::block_on;
use serde_json::{Value, json};

use super::{
    calendar::{CalendarRequest, load_calendar},
    picker::PICKER_QUERY,
    ports::{Catalog, CatalogQuery, catalog_detail, catalog_page, decode_catalog},
};

#[test]
fn contacts_prefer_authenticated_self_and_keep_self_visible_during_search() {
    block_on(async {
        let (_directory, runtime) = start().await;
        execute(&runtime,"INSERT INTO humans(id,name) VALUES ('signed-in','Signed in self'),('00000000-0000-0000-0000-000000000000','Local self'),('other','Another person')",vec![]).await;
        let query = CatalogQuery {
            catalog: Catalog::Contacts,
            search: "does not match".into(),
            offset: 0,
        };
        let (sql, params) = query.sql_for_viewer(Some("signed-in"));
        let page = decode_catalog(&execute(&runtime, &sql, params).await).unwrap();
        assert_eq!(page.rows[0].id.as_ref(), "signed-in");
        assert!(page.rows[0].self_contact);
        assert_eq!(
            page.rows[1].id.as_ref(),
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(page.rows.len(), 2);
        let (sql, params) = query.sql();
        let page = decode_catalog(&execute(&runtime, &sql, params).await).unwrap();
        assert_eq!(page.rows.len(), 1);
        assert!(page.rows[0].self_contact);
        runtime.shutdown().await.unwrap();
    });
}

#[test]
fn picker_scopes_shared_cache_to_viewer_and_deduplicates_managed_local_notes() {
    block_on(async {
        let (_directory, runtime) = start().await;
        execute(&runtime,"INSERT INTO sessions(id,title,created_at) VALUES ('local','Local','2024-01-01'),('recent','Recent','2023-01-01')",vec![]).await;
        for (share, viewer, session, managed) in [
            ("duplicate", "viewer", "local", 1),
            ("shared", "viewer", "remote", 0),
            ("other-account", "other", "remote2", 0),
        ] {
            execute(&runtime,"INSERT INTO shared_session_cache(share_id,viewer_user_id,workspace_id,session_id,content_revision,access_version,published_at,title,manage_access) VALUES (?,?,'workspace',?,1,1,'2025-01-01',?,?)",
                vec![json!(share),json!(viewer),json!(session),json!(share),json!(managed)]).await;
        }
        let rows = execute(
            &runtime,
            PICKER_QUERY,
            vec![json!("%"), json!("viewer"), json!("[\"recent\"]")],
        )
        .await;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], "recent");
        assert_eq!(rows[1]["kind"], "shared");
        assert_eq!(rows[1]["id"], "shared");
        assert_eq!(rows[2]["id"], "local");
        let signed_out = execute(
            &runtime,
            PICKER_QUERY,
            vec![json!("%"), Value::Null, json!("[]")],
        )
        .await;
        assert_eq!(signed_out.len(), 2);
        assert!(signed_out.iter().all(|row| row["kind"] == "session"));
        execute(
            &runtime,
            "UPDATE sessions SET deleted_at='deleted' WHERE id='local'",
            vec![],
        )
        .await;
        let rows = execute(
            &runtime,
            PICKER_QUERY,
            vec![json!("%"), json!("viewer"), json!("[]")],
        )
        .await;
        assert!(rows.iter().any(|row| row["id"] == "duplicate"));
        runtime.shutdown().await.unwrap();
    });
}

#[test]
fn malformed_workflow_storage_is_an_error_and_remains_unchanged() {
    block_on(async {
        let (_directory, runtime) = start().await;
        execute(
            &runtime,
            "INSERT INTO app_settings(id,value_json) VALUES ('automation_workflows','{invalid')",
            vec![],
        )
        .await;
        let query = CatalogQuery {
            catalog: Catalog::Automations,
            search: "".into(),
            offset: 0,
        };
        assert!(
            catalog_page(&runtime, query, CancellationToken::new())
                .unwrap()
                .receive()
                .await
                .is_err()
        );
        let rows = execute(
            &runtime,
            "SELECT value_json FROM app_settings WHERE id='automation_workflows'",
            vec![],
        )
        .await;
        assert_eq!(rows[0]["value_json"], "{invalid");
        runtime.shutdown().await.unwrap();
    });
}

async fn execute(runtime: &RuntimeHandle, sql: &str, params: Vec<Value>) -> Vec<Value> {
    let sql = sql.to_owned();
    runtime
        .submit(move |services| async move {
            services
                .executor
                .execute(sql, params)
                .await
                .map_err(|error| ServiceError::Failed(error.to_string().into()))
        })
        .unwrap()
        .receive()
        .await
        .unwrap()
}

async fn start() -> (tempfile::TempDir, RuntimeHandle) {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, ready) = RuntimeHandle::start(Profile {
        database: directory.path().join("workspace.sqlite"),
    })
    .unwrap();
    ready.receive().await.unwrap();
    (directory, runtime)
}

#[test]
fn catalogs_read_canonical_schema_and_preserve_malformed_template() {
    block_on(async {
        let (_directory, runtime) = start().await;
        execute(&runtime,"INSERT INTO humans(id,name,email) VALUES ('human','Person','p@example.com'),('00000000-0000-0000-0000-000000000000','Me','')",vec![]).await;
        execute(
            &runtime,
            "INSERT INTO organizations(id,name,pinned,pin_order) VALUES ('org','Company',1,0)",
            vec![],
        )
        .await;
        execute(&runtime,"INSERT INTO folders(id,path,instructions) VALUES ('folder','Clients/100%_complete','Original instructions')",vec![]).await;
        execute(&runtime,"INSERT INTO templates(id,title,sections_json,targets_json) VALUES ('template','Damaged template','{broken','\"Team\"')",vec![]).await;
        execute(&runtime,"INSERT INTO app_settings(id,value_json) VALUES ('automation_workflows',?)",vec![json!(serde_json::to_string(&json!([{
            "id":"workflow","title":"Export","enabled":false,"trigger":"meeting_completed","steps":[{"type":"markdown_export","directory":"/fixture/export","options":{"include_memo":false}}],
            "lastRun":null,"processedSessionIds":[],"chatGroupId":"chat"
        }])).unwrap())]).await;
        for catalog in [
            Catalog::Contacts,
            Catalog::Folders,
            Catalog::Templates,
            Catalog::Automations,
        ] {
            let page = catalog_page(
                &runtime,
                CatalogQuery {
                    catalog,
                    search: "".into(),
                    offset: 0,
                },
                CancellationToken::new(),
            )
            .unwrap()
            .receive()
            .await
            .unwrap();
            assert!(!page.rows.is_empty(), "{catalog:?}");
            for row in page.rows.iter() {
                let detail = catalog_detail(&runtime, row.clone(), CancellationToken::new())
                    .unwrap()
                    .receive()
                    .await
                    .unwrap();
                assert!(!detail.title.is_empty(), "{catalog:?}");
                if row.id.as_ref() == "template" {
                    assert!(!detail.warnings.is_empty());
                }
            }
            if catalog == Catalog::Contacts {
                assert!(page.rows[0].self_contact);
                assert_eq!(page.rows[1].id.as_ref(), "org");
            }
        }
        let page = catalog_page(
            &runtime,
            CatalogQuery {
                catalog: Catalog::Folders,
                search: "100%_".into(),
                offset: 0,
            },
            CancellationToken::new(),
        )
        .unwrap()
        .receive()
        .await
        .unwrap();
        assert_eq!(page.rows.len(), 1);
        let rows = execute(
            &runtime,
            "SELECT sections_json FROM templates WHERE id='template'",
            vec![],
        )
        .await;
        assert_eq!(rows[0]["sections_json"], "{broken");
        execute(
            &runtime,
            "UPDATE humans SET deleted_at = 'removed' WHERE id='human'",
            vec![],
        )
        .await;
        let page = catalog_page(
            &runtime,
            CatalogQuery {
                catalog: Catalog::Contacts,
                search: "".into(),
                offset: 0,
            },
            CancellationToken::new(),
        )
        .unwrap()
        .receive()
        .await
        .unwrap();
        assert!(!page.rows.iter().any(|row| row.id.as_ref() == "human"));
        runtime.shutdown().await.unwrap();
    });
}

#[test]
fn calendar_uses_enabled_stored_events_and_exclusive_all_day_end() {
    block_on(async {
        let (_directory, runtime) = start().await;
        execute(
            &runtime,
            "INSERT INTO calendars(id,name,enabled) VALUES ('on','Enabled',1),('off','Disabled',0)",
            vec![],
        )
        .await;
        for (id, calendar, tracking, start, end, all_day) in [
            ("all-day", "on", "all", "2024-02-28", "2024-03-01", 1),
            ("hidden", "off", "off", "2024-02-29", "2024-03-01", 1),
            ("ignored", "on", "ignored", "2024-02-29", "2024-03-01", 1),
            (
                "meeting",
                "on",
                "timed",
                "2024-02-29T12:00:00Z",
                "2024-02-29T13:00:00Z",
                0,
            ),
        ] {
            execute(&runtime,"INSERT INTO events(id,title,calendar_id,tracking_id_event,started_at,ended_at,is_all_day) VALUES (?,?,?,?,?,?,?)",
                vec![json!(id),json!(id),json!(calendar),json!(tracking),json!(start),json!(end),json!(all_day)]).await;
        }
        execute(
            &runtime,
            "INSERT INTO sessions(id,title,event_id) VALUES ('linked','Meeting','meeting')",
            vec![],
        )
        .await;
        execute(
            &runtime,
            "INSERT INTO app_settings(id,value_json) VALUES ('ignored_events',?)",
            vec![json!(
                "[{\"tracking_id\":\"ignored\",\"last_seen\":\"2024-02-29\"}]"
            )],
        )
        .await;
        let request = CalendarRequest {
            anchor: "2024-02-29".into(),
            columns: 7,
            week_start: 1,
            step: 0,
        };
        let page = load_calendar(&runtime, request.clone(), CancellationToken::new())
            .unwrap()
            .receive()
            .await
            .unwrap();
        assert_eq!(page.days.first().unwrap().date.as_ref(), "2024-01-29");
        assert_eq!(page.days.last().unwrap().date.as_ref(), "2024-03-03");
        assert_eq!(page.days.len(), 35);
        let day = page
            .days
            .iter()
            .find(|day| day.date.as_ref() == "2024-02-29")
            .unwrap();
        assert_eq!(day.items.len(), 2);
        assert!(day.items.iter().any(|item| {
            item.session
                .as_ref()
                .is_some_and(|id| id.0.as_ref() == "linked")
        }));
        assert!(
            !page
                .days
                .iter()
                .find(|day| day.date.as_ref() == "2024-03-01")
                .unwrap()
                .items
                .iter()
                .any(|item| item.id.as_ref() == "all-day")
        );
        let next = load_calendar(
            &runtime,
            CalendarRequest {
                step: 1,
                ..request.clone()
            },
            CancellationToken::new(),
        )
        .unwrap()
        .receive()
        .await
        .unwrap();
        assert_eq!(next.anchor.as_ref(), "2024-03-01");
        let compact = load_calendar(
            &runtime,
            CalendarRequest {
                columns: 4,
                ..request
            },
            CancellationToken::new(),
        )
        .unwrap()
        .receive()
        .await
        .unwrap();
        assert_eq!(compact.days.len(), 85);
        assert_eq!(compact.days[42].date, Arc::<str>::from("2024-02-29"));
        runtime.shutdown().await.unwrap();
    });
}

#[test]
fn catalog_page_is_bounded_and_cancelled_reads_cannot_succeed() {
    block_on(async {
        let (_directory, runtime) = start().await;
        execute(&runtime,"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<150) INSERT INTO folders(id,path) SELECT 'id-'||x, printf('Folder-%03d',x) FROM n",vec![]).await;
        let query = CatalogQuery {
            catalog: Catalog::Folders,
            search: "".into(),
            offset: 0,
        };
        let page = catalog_page(&runtime, query.clone(), CancellationToken::new())
            .unwrap()
            .receive()
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 100);
        assert!(page.has_more);
        let next = catalog_page(
            &runtime,
            CatalogQuery {
                offset: 100,
                ..query.clone()
            },
            CancellationToken::new(),
        )
        .unwrap()
        .receive()
        .await
        .unwrap();
        assert_eq!(next.rows.len(), 50);
        assert!(!next.has_more);
        assert_ne!(page.rows[99].id, next.rows[0].id);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            catalog_page(&runtime, query, cancel)
                .unwrap()
                .receive()
                .await,
            Err(ServiceError::Cancelled)
        ));
        runtime.shutdown().await.unwrap();
    });
}
