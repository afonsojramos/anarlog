use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anlg_listener_core::LiveTranscriptDelta;
use anlg_transcript::{FinalizedWord, WordState};
use desktop_runtime::{CancellationToken, Profile, RuntimeHandle, ServiceError, SessionId};
use serde_json::{Value, json};

use super::ai::{AiServices, Message, Part, ProviderAdapter, ProviderEvent, Request, Role};
use super::model::{Interval, MAX_TEXT, Retention, Word, recovered_additions, validate_delta};
use super::persistence::{Persistence, coalesce};
use super::retention::Activities;
use super::store::{SpeakerAssignment, SpeakerScope, TranscriptStore};
use futures::{StreamExt, future::BoxFuture, stream::BoxStream};

struct Fixture {
    runtime: RuntimeHandle,
    store: TranscriptStore,
    session: SessionId,
    transcript: Arc<str>,
    directory: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("anarlog-meeting-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let (runtime, ready) = RuntimeHandle::start(Profile {
            database: directory.join("db.sqlite"),
        })
        .unwrap();
        ready.receive().await.unwrap();
        let session = runtime
            .create_note("Meeting fixture".into())
            .unwrap()
            .receive()
            .await
            .unwrap()
            .summary
            .id;
        let transcript: Arc<str> = uuid::Uuid::new_v4().to_string().into();
        let session_copy = session.clone();
        let id = transcript.clone();
        runtime.submit(move |services| async move {
            services.executor.execute("INSERT INTO transcripts (id, session_id, started_at_ms) VALUES (?, ?, 1000)".into(),
                vec![json!(id), json!(session_copy)]).await.map_err(super::model::failure)?;
            Ok(())
        }).unwrap().receive().await.unwrap();
        Self {
            store: TranscriptStore(runtime.clone()),
            runtime,
            session,
            transcript,
            directory,
        }
    }

    async fn delta(&self, id: &str, delta: LiveTranscriptDelta) {
        self.store
            .journal(self.transcript.clone(), id.to_owned().into(), delta)
            .unwrap()
            .receive()
            .await
            .unwrap();
    }

    async fn sql(&self, sql: &str, params: Vec<Value>) -> Vec<Value> {
        let sql = sql.to_owned();
        self.runtime
            .submit(move |services| async move {
                services
                    .executor
                    .execute(sql, params)
                    .await
                    .map_err(super::model::failure)
            })
            .unwrap()
            .receive()
            .await
            .unwrap()
    }

    async fn close(self) {
        self.runtime.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}

fn delta(id: &str, text: &str, start_ms: i64) -> LiveTranscriptDelta {
    LiveTranscriptDelta {
        new_words: vec![FinalizedWord {
            id: id.into(),
            text: text.into(),
            start_ms,
            end_ms: start_ms + 100,
            channel: 0,
            state: WordState::Final,
            speaker_index: Some(2),
        }],
        replaced_ids: Vec::new(),
        partials: Vec::new(),
    }
}

fn word(id: &str, start_ms: i64) -> Word {
    Word {
        id: id.into(),
        text: "word".into(),
        start_ms,
        end_ms: start_ms + 100,
        channel: 0,
        extra: BTreeMap::new(),
    }
}

#[tokio::test]
async fn journal_is_idempotent_and_fold_replays_corrections() {
    let fixture = Fixture::new().await;
    fixture.delta("op1", delta("a", "first ", 0)).await;
    fixture.delta("op1", delta("a", "first ", 0)).await;
    let mut replacement = delta("b", "corrected ", 0);
    replacement.replaced_ids.push("a".into());
    fixture.delta("op2", replacement).await;
    let snapshot = fixture
        .store
        .snapshot(fixture.transcript.clone())
        .unwrap()
        .receive()
        .await
        .unwrap();
    assert_eq!(snapshot.sequence, 2);
    assert_eq!(snapshot.words.len(), 1);
    assert_eq!(snapshot.words[0].text, "corrected ");
    fixture
        .store
        .fold(fixture.transcript.clone())
        .unwrap()
        .receive()
        .await
        .unwrap();
    let snapshot = fixture
        .store
        .snapshot(fixture.transcript.clone())
        .unwrap()
        .receive()
        .await
        .unwrap();
    assert_eq!(snapshot.sequence, 0);
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.words[0].id, "b");
    assert!(
        fixture
            .sql("SELECT * FROM transcript_live_deltas", vec![])
            .await
            .is_empty()
    );
    fixture.close().await;
}

#[tokio::test]
async fn concurrent_edits_use_word_cas_and_recovery_never_resurrects_deletions() {
    let fixture = Fixture::new().await;
    fixture
        .delta("op", delta("original", "original", 100))
        .await;
    let snapshot = fixture
        .store
        .snapshot(fixture.transcript.clone())
        .unwrap()
        .receive()
        .await
        .unwrap();
    fixture
        .store
        .edit_word(fixture.transcript.clone(), snapshot.words[0].clone(), None)
        .unwrap()
        .receive()
        .await
        .unwrap();
    let stale = fixture
        .store
        .edit_word(
            fixture.transcript.clone(),
            snapshot.words[0].clone(),
            Some("stale".into()),
        )
        .unwrap()
        .receive()
        .await;
    assert!(matches!(stale, Err(ServiceError::Conflict)));
    fixture
        .store
        .repair(
            fixture.transcript.clone(),
            snapshot.words,
            vec![word("recovered-deleted", 100), word("gap", 2000)],
            Vec::new(),
            vec![Interval {
                start: 0,
                end: 3000,
            }],
        )
        .unwrap()
        .receive()
        .await
        .unwrap();
    let snapshot = fixture
        .store
        .snapshot(fixture.transcript.clone())
        .unwrap()
        .receive()
        .await
        .unwrap();
    assert_eq!(snapshot.words.len(), 1);
    assert_eq!(snapshot.words[0].id, "gap");
    fixture.close().await;
}

#[tokio::test]
async fn worker_flushes_corrections_and_runtime_shutdown_barrier() {
    let fixture = Fixture::new().await;
    let persistence = Persistence::install(&fixture.runtime, fixture.transcript.clone())
        .await
        .unwrap();
    persistence.push(delta("a", "pending", 0)).await.unwrap();
    let mut correction = delta("b", "final", 0);
    correction.replaced_ids.push("a".into());
    persistence.push(correction).await.unwrap();
    persistence.flush().await.unwrap();
    let snapshot = fixture
        .store
        .snapshot(fixture.transcript.clone())
        .unwrap()
        .receive()
        .await
        .unwrap();
    assert_eq!(snapshot.words.len(), 1);
    assert_eq!(snapshot.words[0].text, "final");
    assert_eq!(snapshot.sequence, 0);
    persistence
        .push(delta("c", "shutdown", 1000))
        .await
        .unwrap();
    fixture.runtime.shutdown().await.unwrap();
    let (runtime, ready) = RuntimeHandle::start(Profile {
        database: fixture.directory.join("db.sqlite"),
    })
    .unwrap();
    ready.receive().await.unwrap();
    let snapshot = TranscriptStore(runtime.clone())
        .snapshot(fixture.transcript)
        .unwrap()
        .receive()
        .await
        .unwrap();
    assert_eq!(snapshot.words.len(), 2);
    runtime.shutdown().await.unwrap();
    std::fs::remove_dir_all(fixture.directory).unwrap();
}

#[tokio::test]
async fn locked_or_deleted_session_rejects_authoritative_transcript_writes() {
    let fixture = Fixture::new().await;
    fixture
        .sql(
            "UPDATE sessions SET locked = 1 WHERE id = ?",
            vec![json!(fixture.session)],
        )
        .await;
    let result = fixture
        .store
        .journal(
            fixture.transcript.clone(),
            "locked".into(),
            delta("a", "locked", 0),
        )
        .unwrap()
        .receive()
        .await;
    assert!(result.is_err());
    assert!(
        fixture
            .sql("SELECT * FROM transcript_live_deltas", vec![])
            .await
            .is_empty()
    );
    fixture
        .sql(
            "UPDATE sessions SET locked = 0, deleted_at = 'deleted' WHERE id = ?",
            vec![json!(fixture.session)],
        )
        .await;
    assert!(
        fixture
            .store
            .fold(fixture.transcript.clone())
            .unwrap()
            .receive()
            .await
            .is_err()
    );
    fixture.close().await;
}

#[tokio::test]
async fn speaker_assignment_is_anchor_scoped_and_survives_provider_correction() {
    let fixture = Fixture::new().await;
    fixture
        .sql(
            "INSERT INTO humans (id, name) VALUES ('human', 'Alice')",
            vec![],
        )
        .await;
    fixture.delta("op", delta("a", "hello ", 0)).await;
    fixture
        .store
        .assign(SpeakerAssignment {
            transcript_id: fixture.transcript.clone(),
            anchor: "a".into(),
            human_id: "human".into(),
            scope: SpeakerScope::Segment {
                word_ids: vec!["a".into()],
            },
        })
        .unwrap()
        .receive()
        .await
        .unwrap();
    let mut correction = delta("b", "Hello ", 0);
    correction.replaced_ids.push("a".into());
    fixture.delta("correction", correction).await;
    let rendered = fixture
        .store
        .load(fixture.session.clone(), CancellationToken::new())
        .unwrap()
        .receive()
        .await
        .unwrap();
    assert_eq!(rendered.segments[0].speaker_label, "Alice");
    assert_eq!(rendered.segments[0].words[0].id.as_deref(), Some("b"));
    let invalid = fixture
        .store
        .assign(SpeakerAssignment {
            transcript_id: fixture.transcript.clone(),
            anchor: "b".into(),
            human_id: "human".into(),
            scope: SpeakerScope::Segment {
                word_ids: vec!["missing".into()],
            },
        })
        .unwrap()
        .receive()
        .await;
    assert!(invalid.is_err());
    fixture.close().await;
}

#[test]
fn recovery_filters_old_current_and_out_of_gap_words() {
    let before = vec![word("deleted", 0)];
    let current = vec![word("edited", 1000)];
    let recovered = vec![
        word("new-old", 0),
        word("new-current", 1000),
        word("new-gap", 2000),
        word("outside", 4000),
    ];
    let result = recovered_additions(
        recovered,
        &before,
        &current,
        &[Interval {
            start: 0,
            end: 3000,
        }],
    );
    assert_eq!(
        result.into_iter().map(|word| word.id).collect::<Vec<_>>(),
        vec!["new-gap"]
    );
}

#[test]
fn retention_normalizes_all_shipping_and_legacy_values() {
    for (value, retention) in [
        (json!(false), Retention::Never),
        (json!(true), Retention::Forever),
        (json!("none"), Retention::Never),
        (json!("forever"), Retention::Forever),
        (json!("oneDay"), Retention::Days(1)),
        (json!("threeDays"), Retention::Days(3)),
        (json!("oneWeek"), Retention::Days(7)),
        (json!("oneMonth"), Retention::Days(30)),
    ] {
        assert_eq!(Retention::from_setting(&value).unwrap(), retention);
    }
    assert!(Retention::from_setting(&json!("unexpected")).is_err());
}

#[test]
fn bounds_reject_oversized_authoritative_words_and_bad_timestamps() {
    assert!(validate_delta(&delta("a", &"x".repeat(MAX_TEXT), 0)).is_err());
    let mut invalid = delta("a", "ok", 1);
    invalid.new_words[0].end_ms = 0;
    assert!(validate_delta(&invalid).is_err());
}

#[test]
fn correction_chain_coalesces_without_losing_replacements() {
    let first = delta("a", "one", 0);
    let mut second = delta("b", "two", 0);
    second.replaced_ids = vec!["a".into()];
    let mut third = delta("c", "three", 0);
    third.replaced_ids = vec!["b".into()];
    let result = coalesce([first, second, third]);
    assert_eq!(result.new_words.len(), 1);
    assert_eq!(result.new_words[0].id, "c");
    assert_eq!(result.replaced_ids, ["a", "b"]);
}

#[tokio::test]
async fn retention_honors_activity_and_preserves_non_audio_data() {
    let fixture = Fixture::new().await;
    let directory =
        super::retention::session_directory(&fixture.directory, &fixture.session).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("audio.wav"), b"audio").unwrap();
    std::fs::write(directory.join("memo.json"), b"preserve").unwrap();
    fixture.sql("INSERT INTO session_attachments (id, session_id, source_type, source_id) VALUES ('audio', ?, 'session_audio', 'primary')", vec![json!(fixture.session)]).await;
    let activities = Activities::default();
    let lease = activities.acquire(fixture.session.clone()).unwrap();
    assert!(activities.startup().is_err());
    assert!(
        super::retention::cleanup(
            &fixture.runtime,
            fixture.directory.clone(),
            activities.clone(),
            Retention::Never,
            i64::MAX
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert!(directory.join("audio.wav").exists());
    drop(lease);
    assert_eq!(
        super::retention::cleanup(
            &fixture.runtime,
            fixture.directory.clone(),
            activities,
            Retention::Never,
            i64::MAX
        )
        .await
        .unwrap(),
        vec![fixture.session.clone()]
    );
    assert!(!directory.join("audio.wav").exists());
    assert_eq!(
        std::fs::read(directory.join("memo.json")).unwrap(),
        b"preserve"
    );
    assert_eq!(
        fixture
            .sql(
                "SELECT availability FROM attachment_local_state WHERE attachment_id = 'audio'",
                vec![]
            )
            .await[0]["availability"],
        "absent"
    );
    assert!(
        fixture
            .store
            .snapshot(fixture.transcript.clone())
            .unwrap()
            .receive()
            .await
            .is_ok()
    );
    fixture.close().await;
}

#[tokio::test]
async fn startup_never_deletes_audio_but_preserves_failed_finalization_marker() {
    let fixture = Fixture::new().await;
    let directory =
        super::retention::session_directory(&fixture.directory, &fixture.session).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("audio.wav"), b"audio").unwrap();
    std::fs::write(directory.join(".delete-audio-on-stop"), b"").unwrap();
    let marker = json!({"version":1, "sessionId":fixture.session, "transcriptId":fixture.transcript, "retainAudio":false, "startedAt":1});
    fixture
        .sql(
            "INSERT INTO app_settings (id, value_json) VALUES (?, ?)",
            vec![
                json!(format!("capture_lifecycle_pending:{}", fixture.session.0)),
                json!(marker.to_string()),
            ],
        )
        .await;
    let reports = super::recovery::recover_startup(
        &fixture.runtime,
        fixture.directory.clone(),
        Activities::default(),
        Arc::new(|_, _| {
            Box::pin(async {
                Err(super::model::failure(
                    "Never must not invoke batch provider",
                ))
            })
        }),
    )
    .await
    .unwrap();
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0]
            .result
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("incomplete")
    );
    assert!(!directory.join("audio.wav").exists());
    assert_eq!(
        fixture
            .sql(
                "SELECT * FROM app_settings WHERE id GLOB 'capture_lifecycle_pending:*'",
                vec![]
            )
            .await
            .len(),
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn cleanup_preserves_audio_owned_by_durable_finalization_marker() {
    let fixture = Fixture::new().await;
    let directory =
        super::retention::session_directory(&fixture.directory, &fixture.session).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("audio.wav"), b"unfinished").unwrap();
    fixture
        .sql(
            "INSERT INTO app_settings (id, value_json) VALUES (?, '{}')",
            vec![json!(format!(
                "capture_lifecycle_pending:{}",
                fixture.session.0
            ))],
        )
        .await;
    assert!(
        super::retention::cleanup(
            &fixture.runtime,
            fixture.directory.clone(),
            Activities::default(),
            Retention::Never,
            i64::MAX
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert!(directory.join("audio.wav").exists());
    fixture.close().await;
}

#[test]
fn activity_exclusion_and_path_provenance() {
    let activities = Activities::default();
    let startup = activities.startup().unwrap();
    assert!(activities.acquire(SessionId("session".into())).is_err());
    drop(startup);
    assert!(activities.acquire(SessionId("session".into())).is_ok());
    for unsafe_id in ["", "..", "../another", "/outside", "x\\y"] {
        assert!(
            super::retention::session_directory(
                std::path::Path::new("/vault"),
                &SessionId(unsafe_id.into())
            )
            .is_err()
        );
    }
}

struct FixtureProvider(Arc<AtomicBool>);

impl ProviderAdapter for FixtureProvider {
    fn stream(
        &self,
        _: Request,
    ) -> BoxFuture<
        'static,
        desktop_runtime::Result<BoxStream<'static, desktop_runtime::Result<ProviderEvent>>>,
    > {
        let fail = self.0.load(Ordering::Acquire);
        Box::pin(async move {
            if fail {
                return Err(super::model::failure("Fixture provider failure"));
            }
            Ok(futures::stream::iter([
                Ok(ProviderEvent::Text("answer ".into())),
                Ok(ProviderEvent::Text("completed".into())),
            ])
            .boxed())
        })
    }
}

#[tokio::test]
async fn chat_repairs_user_row_rejects_orphans_and_preserves_failed_regeneration() {
    let fixture = Fixture::new().await;
    fixture
        .sql("INSERT INTO chat_groups (id) VALUES ('group')", vec![])
        .await;
    let fail = Arc::new(AtomicBool::new(false));
    let provider: Arc<dyn ProviderAdapter> = Arc::new(FixtureProvider(fail.clone()));
    let ai = Arc::new(
        AiServices::new(
            fixture.runtime.clone(),
            Arc::new(move |_| {
                let provider = provider.clone();
                Box::pin(async move { Ok(provider) })
            }),
            Arc::new(|_, _| Box::pin(async { Err(super::model::failure("No fixture tools")) })),
            Arc::new(|_| false),
        )
        .unwrap(),
    );
    let chat = ai.chat(fixture.session.clone(), "group".into()).unwrap();
    assert!(Arc::ptr_eq(
        &chat,
        &ai.chat(fixture.session.clone(), "group".into()).unwrap()
    ));
    let user = Message {
        id: "user".into(),
        role: Role::User,
        parts: vec![Part::Text {
            text: "question".into(),
        }],
    };
    let assistant = chat
        .queue_send(user.clone(), Vec::new(), None, Arc::new(|_| {}))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let rows = fixture.sql("SELECT role, content FROM chat_messages WHERE deleted_at IS NULL ORDER BY created_at, id", vec![]).await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row["role"] == "user"));
    assert!(rows.iter().any(|row| row["content"] == "answer completed"));
    fail.store(true, Ordering::Release);
    assert!(
        chat.queue_send(
            user.clone(),
            Vec::new(),
            Some(assistant.id.clone()),
            Arc::new(|_| {})
        )
        .unwrap()
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(
        fixture
            .sql(
                "SELECT id FROM chat_messages WHERE id = ? AND deleted_at IS NULL",
                vec![json!(assistant.id)]
            )
            .await
            .len(),
        1
    );
    fixture
        .sql(
            "UPDATE chat_groups SET deleted_at = 'deleted' WHERE id = 'group'",
            vec![],
        )
        .await;
    assert!(
        chat.queue_send(user, Vec::new(), None, Arc::new(|_| {}))
            .unwrap()
            .await
            .unwrap()
            .is_err()
    );
    chat.flush().await;
    fixture.close().await;
}

#[test]
fn history_window_retains_the_user_that_started_the_tool_chain() {
    let history = (0..30)
        .map(|index| Message {
            id: index.to_string(),
            role: if index % 3 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            parts: vec![],
        })
        .collect();
    let window = super::ai::window_history(history);
    assert_eq!(window[0].id, "9");
    assert_eq!(window.last().unwrap().id, "29");
}

#[tokio::test]
async fn provider_corrections_preserve_unknown_word_metadata() {
    let fixture = Fixture::new().await;
    let mut original = word("word", 0);
    original
        .extra
        .insert("future_metadata".into(), json!({"nested": [1, 2]}));
    fixture
        .sql(
            "UPDATE transcripts SET words_json = ? WHERE id = ?",
            vec![
                json!(serde_json::to_string(&vec![original]).unwrap()),
                json!(fixture.transcript),
            ],
        )
        .await;
    fixture
        .delta("correction", delta("word", "corrected", 0))
        .await;
    fixture
        .store
        .fold(fixture.transcript.clone())
        .unwrap()
        .receive()
        .await
        .unwrap();
    let snapshot = fixture
        .store
        .snapshot(fixture.transcript.clone())
        .unwrap()
        .receive()
        .await
        .unwrap();
    assert_eq!(snapshot.words[0].text, "corrected");
    assert_eq!(
        snapshot.words[0].extra["future_metadata"],
        json!({"nested": [1, 2]})
    );
    fixture.close().await;
}

fn summary_context() -> super::ai::SummaryContext {
    super::ai::SummaryContext {
        system: anlg_template_app::EnhanceSystem {
            language: Some("en".into()),
            format_override: "Use headings and preserve decisions.".into(),
        },
        user: anlg_template_app::EnhanceUser {
            session: anlg_template_app::Session {
                title: Some("Fixture".into()),
                started_at: None,
                ended_at: None,
                event: None,
            },
            participants: Vec::new(),
            template: None,
            transcripts: Vec::new(),
            pre_meeting_memo: "Initial note".into(),
            post_meeting_memo: String::new(),
        },
    }
}

#[tokio::test]
async fn retired_capture_workers_do_not_exhaust_runtime_flush_slots() {
    let fixture = Fixture::new().await;
    let group = super::persistence::PersistenceGroup::install(&fixture.runtime)
        .await
        .unwrap();
    for _ in 0..70 {
        let worker = group
            .transcript(&fixture.runtime, fixture.transcript.clone())
            .await
            .unwrap();
        worker.retire().await.unwrap();
        assert!(matches!(
            worker.push(delta("late", "late", 0)).await,
            Err(ServiceError::Closed)
        ));
    }
    fixture.close().await;
}

#[tokio::test]
async fn summaries_reject_notes_active_capture_and_stale_targets() {
    let fixture = Fixture::new().await;
    let base = fixture
        .runtime
        .open_session(fixture.session.clone(), CancellationToken::new())
        .unwrap()
        .receive()
        .await
        .unwrap()
        .note
        .unwrap();
    let active = Arc::new(AtomicBool::new(false));
    let recording = active.clone();
    let provider: Arc<dyn ProviderAdapter> =
        Arc::new(FixtureProvider(Arc::new(AtomicBool::new(false))));
    let services = AiServices::new(
        fixture.runtime.clone(),
        Arc::new(move |_| {
            let provider = provider.clone();
            Box::pin(async move { Ok(provider) })
        }),
        Arc::new(|_, _| Box::pin(async { Ok(json!({})) })),
        Arc::new(move |_| recording.load(Ordering::Acquire)),
    )
    .unwrap();
    assert!(matches!(
        services
            .summarize(
                base.clone(),
                summary_context(),
                CancellationToken::new(),
                Arc::new(|_| {})
            )
            .await,
        Err(ServiceError::Conflict)
    ));
    fixture
        .sql(
            "UPDATE session_documents SET kind = 'summary' WHERE id = ?",
            vec![json!(base.id)],
        )
        .await;
    active.store(true, Ordering::Release);
    assert!(matches!(
        services
            .summarize(
                base.clone(),
                summary_context(),
                CancellationToken::new(),
                Arc::new(|_| {})
            )
            .await,
        Err(ServiceError::Busy)
    ));
    active.store(false, Ordering::Release);
    let saved = services
        .summarize(
            base.clone(),
            summary_context(),
            CancellationToken::new(),
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    assert!(saved.body.contains("answer completed"));
    assert!(matches!(
        services
            .summarize(
                base,
                summary_context(),
                CancellationToken::new(),
                Arc::new(|_| {})
            )
            .await,
        Err(ServiceError::Conflict)
    ));
    fixture.close().await;
}

struct ToolProvider;
impl ProviderAdapter for ToolProvider {
    fn stream(
        &self,
        request: Request,
    ) -> BoxFuture<
        'static,
        desktop_runtime::Result<BoxStream<'static, desktop_runtime::Result<ProviderEvent>>>,
    > {
        Box::pin(async move {
            let event = if request.tools_allowed {
                ProviderEvent::Tool {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    name: "meeting".into(),
                    input: json!({"session_id": request.session.0}),
                }
            } else {
                ProviderEvent::Text("Final report".into())
            };
            Ok(futures::stream::iter([Ok(event)]).boxed())
        })
    }
}

#[tokio::test]
async fn tool_loop_executes_five_typed_calls_then_a_final_report() {
    let fixture = Fixture::new().await;
    fixture
        .sql("INSERT INTO chat_groups (id) VALUES ('tools')", vec![])
        .await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let ai = Arc::new(
        AiServices::new(
            fixture.runtime.clone(),
            Arc::new(|_| {
                Box::pin(async { Ok(Arc::new(ToolProvider) as Arc<dyn ProviderAdapter>) })
            }),
            Arc::new(move |tool, _| {
                assert!(matches!(tool, super::ai::MeetingTool::Meeting { .. }));
                observed.fetch_add(1, Ordering::AcqRel);
                Box::pin(async { Ok(json!({"title": "Meeting fixture"})) })
            }),
            Arc::new(|_| false),
        )
        .unwrap(),
    );
    let chat = ai.chat(fixture.session.clone(), "tools".into()).unwrap();
    let answer = chat
        .queue_send(
            Message {
                id: "question".into(),
                role: Role::User,
                parts: vec![Part::Text {
                    text: "Find this meeting".into(),
                }],
            },
            Vec::new(),
            None,
            Arc::new(|_| {}),
        )
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::Acquire), 5);
    assert!(matches!(answer.parts.last(), Some(Part::Text { text }) if text == "Final report"));
    assert_eq!(
        answer
            .parts
            .iter()
            .filter(|part| matches!(part, Part::Tool { .. }))
            .count(),
        5
    );
    fixture.close().await;
}

#[tokio::test]
async fn cancelling_queued_chat_prevents_provider_calls_and_persistence() {
    let fixture = Fixture::new().await;
    fixture
        .sql("INSERT INTO chat_groups (id) VALUES ('cancelled')", vec![])
        .await;
    let (release, blocked) = tokio::sync::oneshot::channel();
    let ai = Arc::new(
        AiServices::new(
            fixture.runtime.clone(),
            Arc::new(|_| {
                Box::pin(async { panic!("Cancelled queued work must never request a provider") })
            }),
            Arc::new(|_, _| Box::pin(async { Ok(json!({})) })),
            Arc::new(|_| false),
        )
        .unwrap(),
    );
    let blocker = ai
        .enqueue(Box::pin(async move {
            blocked.await.map_err(super::model::failure)
        }))
        .unwrap();
    let chat = ai
        .chat(fixture.session.clone(), "cancelled".into())
        .unwrap();
    let answer = chat
        .queue_send(
            Message {
                id: "cancelled".into(),
                role: Role::User,
                parts: vec![Part::Text {
                    text: "queued".into(),
                }],
            },
            Vec::new(),
            None,
            Arc::new(|_| {}),
        )
        .unwrap();
    chat.cancel().unwrap();
    release.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    assert!(matches!(
        answer.await.unwrap(),
        Err(ServiceError::Cancelled)
    ));
    assert!(
        fixture
            .sql("SELECT * FROM chat_messages", vec![])
            .await
            .is_empty()
    );
    ai.flush().await.unwrap();
    fixture.close().await;
}

#[test]
fn stereo_waveform_is_normalized_and_bounded_for_long_input() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stereo.wav");
    let frames = 16_000 * 45;
    let data_length = frames * 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36_u32 + data_length).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&64_000_u32.to_le_bytes());
    bytes.extend_from_slice(&4_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_length.to_le_bytes());
    for _ in 0..frames {
        bytes.extend_from_slice(&8192_i16.to_le_bytes());
        bytes.extend_from_slice(&16384_i16.to_le_bytes());
    }
    std::fs::write(&path, bytes).unwrap();
    let (peaks, duration) = super::playback::decode_waveform(&path).unwrap();
    assert!(peaks.len() <= 4096);
    assert_eq!(duration.as_secs(), 45);
    assert!(peaks.iter().all(|peak| peak[0] == 0.5 && peak[1] == 1.0));
}

struct FixtureStorage(PathBuf);

#[test]
fn recovery_chunks_align_resumed_capture_and_overlap_offsets() {
    let chunk = anlg_listener_core::actors::recorder::RecoveryAudioChunk {
        id: "chunk".into(),
        path: "unused.mp3".into(),
        capture_started_at: 105_000,
        start_ms: 60_000,
        audio_start_ms: 58_000,
        end_ms: 120_000,
    };
    assert_eq!(
        super::recovery::chunk_interval(&chunk, 100_000),
        Interval {
            start: 65_000,
            end: 125_000
        }
    );
    assert_eq!(
        chunk.capture_started_at as i64 - 100_000 + chunk.audio_start_ms as i64,
        63_000
    );
}

impl anlg_storage::StorageRuntime for FixtureStorage {
    fn global_base(&self) -> std::result::Result<PathBuf, anlg_storage::Error> {
        Ok(self.0.clone())
    }
    fn vault_base(&self) -> std::result::Result<PathBuf, anlg_storage::Error> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn native_capture_uses_fixture_audio_and_keeps_unresolved_batch_marker() {
    let fixture = Fixture::new().await;
    let capture = super::capture::CaptureService::spawn(
        fixture.runtime.clone(),
        Arc::new(anlg_audio_mock::MockAudio::new(1)),
        Arc::new(FixtureStorage(fixture.directory.clone())),
        Arc::new(|session| {
            Box::pin(async move {
                Ok(super::capture::CaptureConfig {
                    params: anlg_listener_core::actors::SessionParams {
                        session_id: session.0.to_string(),
                        retain_audio: Some(true),
                        languages: Vec::new(),
                        onboarding: false,
                        transcription_mode: anlg_listener_core::TranscriptionMode::Batch,
                        model: "fixture-batch".into(),
                        base_url: "http://127.0.0.1:1".into(),
                        api_key: String::new(),
                        keywords: Vec::new(),
                        mic_device: None,
                        participant_human_ids: Vec::new(),
                        self_human_id: None,
                        speaker_assignments: Vec::new(),
                    },
                    provider: "fixture".into(),
                    retention: Retention::Forever,
                    memo: String::new(),
                    recovery: None,
                })
            })
        }),
    )
    .unwrap();
    let devices = capture.devices().unwrap().await.unwrap().unwrap();
    assert_eq!(devices.default, "mock-mic");
    assert!(devices.microphones.contains(&"mock-mic".to_owned()));
    capture.microphone(None).unwrap().await.unwrap().unwrap();
    capture
        .start(fixture.session.clone())
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while capture.take_update(0).phase != super::capture::Phase::Listening {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let marker = fixture
        .sql(
            "SELECT value_json FROM app_settings WHERE id = ?",
            vec![json!(format!(
                "capture_lifecycle_pending:{}",
                fixture.session.0
            ))],
        )
        .await;
    assert_eq!(marker.len(), 1);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    capture.stop().unwrap().await.unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while capture.take_update(0).phase != super::capture::Phase::Failed {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        capture
            .take_update(0)
            .error
            .unwrap()
            .to_string()
            .contains("incomplete")
    );
    assert_eq!(
        fixture
            .sql(
                "SELECT value_json FROM app_settings WHERE id = ?",
                vec![json!(format!(
                    "capture_lifecycle_pending:{}",
                    fixture.session.0
                ))]
            )
            .await
            .len(),
        1
    );
    drop(capture);
    fixture.close().await;
}

#[cfg(unix)]
#[test]
fn retention_refuses_symlinked_session_directories() {
    let vault = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let sessions = vault.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    std::os::unix::fs::symlink(outside.path(), sessions.join("meeting")).unwrap();
    assert!(
        super::retention::session_directory(vault.path(), &SessionId("meeting".into())).is_err()
    );
}

#[test]
fn playback_centers_both_recording_channels_without_clipping() {
    let source = rodio::buffer::SamplesBuffer::new(
        std::num::NonZeroU16::new(2).unwrap(),
        std::num::NonZeroU32::new(16_000).unwrap(),
        vec![1., 0., 0., 1., 0.8, 0.2, 1., 1.],
    );
    let centered = super::playback::Centered(source);
    assert_eq!(anlg_audio_utils::Source::channels(&centered).get(), 1);
    assert_eq!(centered.collect::<Vec<_>>(), vec![0.5, 0.5, 0.5, 1.]);
}
