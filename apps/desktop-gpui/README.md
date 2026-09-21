# Native GPUI preview (ANLG-320)

This is an opt-in, standalone native package. The existing desktop app remains the
shipping application and the behavior reference. This package starts from
`6adf6f0d1ccdf72a5ceeb2313466a5a98cd0eeb8`; no implementation from PR #7683 or its
branch was transplanted.

## Run

Use the repository's Rust toolchain and run from the repository root:

```sh
cargo run --locked -p desktop-gpui
# Or select a disposable profile / backup copy:
cargo run --locked -p desktop-gpui -- --profile /absolute/path/to/sandbox
```

The default is `$HOME/.anarlog-gpui-sandbox/library.sqlite` (Windows:
`%USERPROFILE%\.anarlog-gpui-sandbox\library.sqlite`). There is no automatic
discovery or migration of the shipping application's live profile. `--profile`
is a directory, not a database filename. Only use an isolated directory or a
consistent SQLite backup; do not copy a live WAL database's main file alone.
The canonical schema is prepared with `db-app`; CloudSync is disabled.

The integrated preview mounts the native workspace, TipTap JSON editor,
transcript/audio pane, and product settings/services. The editor autosaves through
a background journal; save conflicts retain drafts. The note header opens
transcript/audio, share, and export surfaces. Product services without concrete
adapters display their unavailable state. Recording is deliberately unavailable
until a validated provider, secure credentials, and model configuration are
connected. No credentials or provider defaults are fabricated.

Pinned tabs are restored and saved with CAS through `app_settings.gpui_pinned_tabs`
inside the isolated profile. Malformed persisted pin data disables pin writes.
This does not import the shipping application's file-based pinned-tab store.
Title/editor/product drafts gate navigation and quit. Window close, the native
Quit control, and the application quit keyboard shortcut flush the editor, stop
and finalize capture, and await runtime drain. OS-forced termination and native
application-menu quit are not yet intercepted reliably.

Published `gpui = "=0.2.2"` is used without a fork or patch. On Linux, GPUI needs
X11 or Wayland, a working Vulkan device/driver, xkbcommon, fontconfig, and FreeType.
Build-only verification does not establish that a particular GPU/desktop works.
macOS and Windows require their native toolchains and platform validation.
Use Ubuntu 24.04 or a matching recent PipeWire SDK: Ubuntu 22.04's 0.3.48 SPA
headers cannot compile the repository's `libspa 0.9.2` dependency. The native
Linux CI uses Ubuntu 24.04 and never launches a display or audio device.

The shared lockfile also changes existing resolutions: GPUI pins macOS
`core-foundation` to `0.10.0`; its `cbindgen` requires `toml >=0.8.8`, resolving to
`0.8.23`/`toml_datetime 0.6.11`; `proc-macro-crate 2.0.2` pins an incompatible
datetime version, so the resolver selects `2.0.0`. The GPUI utilities' `nix`
requires `libc >=0.2.186`, resolving to `0.2.189`. Attempts to retain the old
versions were rejected by Cargo. Verify the existing desktop's native platform
builds against these shared resolutions before integration.

## Architecture

- `main.rs` owns CLI/profile selection and the native window.
- `application.rs` owns the pane entities, event routing, one shared capture and
  playback owner, startup recovery, persisted pins, and the close/drain flow.
  Real audio/storage ports are installed; missing provider/product services remain
  explicit unavailable boundaries.
- `ui/` provides source-derived light/dark HSL tokens, the system font policy,
  embedded existing logo and Hugeicons, focus restoration, and a bounded native
  single-line input with UTF-16/IME entry points and clipboard operations.
- `workspace/` has separate shell, virtual list and note entities. A list refresh
  does not reparse or replace the selected document. List and note reads use
  cancellation plus monotonically increasing request generations; stale results
  cannot replace the current view. A previous page remains visible while loading.
- `desktop-runtime` owns a two-thread Tokio executor on a separate coordinator
  thread, a 64-item FIFO admission queue and canonical DB services. SQL, disk I/O,
  parsing for document saves and transaction execution happen there, never in
  rendering or text replacement. Accepted writes survive dropped receivers.
  Queue saturation is an explicit `Busy` result; it never silently drops a write.
- Data reads reuse `db-app`. Atomic writes/CAS use `db-execute`. Reactive queries
  use `db-reactive` rather than recreating its invalidation rules.
  Dependencies are table/view/FTS-level, not row-level. The foundation watches
  `sessions` only, then refreshes the bounded current page. No global settings,
  templates, contacts or document reload is attached to this signal.
- `QueryWatch` coalesces replaceable snapshots in one watch slot. Errors use an
  eight-item reliable queue; overflow records a terminal error and terminates
  that subscription. At most 32 watches can be live per runtime. Callers must
  inspect both channels and `terminal_error()`.
  `unsubscribe().await` is the canonical hard delivery barrier.
- `shutdown().await` rejects new admission, drains all accepted work, runs
  registered flush participants and closes the pool before acknowledging.
  Flush participants run on the runtime and must not enqueue back into the
  closed queue. Normal window close awaits this acknowledgement. OS-forced
  termination and GPUI's time-limited application-quit path cannot guarantee it.

`DocumentSnapshot.body` preserves exact stored bytes and unknown fields.
The isolated native runtime uses one pooled SQLite connection. SQLite's commit
hook can notify a reactive reader before a large WAL write becomes visible to
another connection; serializing acquisition makes refreshes wait for the writer.
Watches also suppress identical snapshots. Increasing the native pool size needs
a verified post-commit delivery barrier first.

`save_document` is an explicit JSON-document API, not a lossless editor
implementation: it verifies the root and CASes the original body, format and
timestamp, but cannot prove that a caller retained every unknown node. The editor
lane must preserve unknown data and refuse unsafe transformations. CAS failures
must retain the dirty draft and expose reload/merge recovery.

## Module contracts

All modules are registered in `lib.rs` and mounted by `application.rs`.
The following contracts and ownership boundaries were used by the five fresh
implementation lanes. Explicit unavailable services are still integration
boundaries; they do not count as feature delivery.

### Path ownership

| Owner | Exclusive paths |
| --- | --- |
| Workspace | `apps/desktop-gpui/src/workspace/**` |
| Editor | `apps/desktop-gpui/src/editor/**` |
| Meeting | `apps/desktop-gpui/src/meeting/**` |
| Product | `apps/desktop-gpui/src/product/**` |
| Runtime | `crates/desktop-runtime/src/**`, `crates/desktop-runtime/tests/**`, `apps/desktop-gpui/src/runtime_bridge.rs`, `apps/desktop-gpui/src/platform/**` |
| Final integrator | Root/shared manifests and lockfiles, all CI, `apps/desktop-gpui/{Cargo.toml,build.rs,README.md,scripts/**,assets/**}`, `src/{main.rs,lib.rs,contracts.rs,ui/**}`, `crates/desktop-runtime/Cargo.toml` |

Workspace can create `workspace/theme.rs` for application-specific layout or
theme state, but must consume common `ui::theme::Theme`, not duplicate token
values. Existing domain crates and existing desktop/web/mobile code are outside
every lane's write ownership. Needed changes/extractions there, including
transport-neutral Tantivy, go to the integrator with the exact proposed patch.
No lane changes schema, credentials, release defaults or security policies.

### Shared symbols

Imports below are public from the `desktop_gpui` and `desktop_runtime` libraries:

```rust,ignore
use desktop_gpui::contracts::{
    LaneContext, EditorInit, EditorEvent, MeetingIntent, MeetingEvent,
    ProductRoute, ProductEvent, WorkspaceEvent,
};
use desktop_runtime::{
    RuntimeHandle, Profile, Reply, Services, CancellationToken, Generation,
    SessionId, DocumentId, HumanId, AttachmentId,
    SessionSummary, LibraryQuery, LibraryPage, OpenSession, DocumentSnapshot,
    RenameSession, SaveDocument, LoadState, ServiceError, QueryWatch,
};
```

`LaneContext { runtime: RuntimeHandle }` is cheap to clone. Immutable snapshots
use `Arc`; resource IDs are typed strings so imported IDs are not forced into a
UUID format. `LoadState<T>` distinguishes pending/ready/error/unsupported and
retains previous snapshots. `Reply<T>::receive().await -> Result<T>` can be
awaited by a GPUI task; it does not require a Tokio runtime on the UI thread.

`RuntimeHandle` APIs:

```rust,ignore
start(Profile) -> std::io::Result<(RuntimeHandle, Reply<()>)>
library(LibraryQuery, CancellationToken) -> Result<Reply<LibraryPage>>
open_session(SessionId, CancellationToken) -> Result<Reply<OpenSession>>
create_note(Arc<str>) -> Result<Reply<OpenSession>>
rename_session(RenameSession) -> Result<Reply<OpenSession>>
save_document(SaveDocument) -> Result<Reply<DocumentSnapshot>>
watch_library() -> Result<Reply<QueryWatch>>
watch_query(String, Vec<serde_json::Value>) -> Result<Reply<QueryWatch>>
submit(FnOnce(Services) -> SendFuture<Result<T>>) -> Result<Reply<T>>
read(CancellationToken, FnOnce(Services) -> SendFuture<Result<T>>) -> Result<Reply<T>>
register_flush(FlushParticipant) -> Result<Reply<()>>
shutdown().await -> Result<()>
```

`submit`/`read` are worker entry points, not UI SQL APIs. Domain-specific
typed operations belong in runtime submodules. The runtime lane should add
bounded independent service queues for long-running audio/network work rather
than monopolizing the foundation's single serialized DB coordinator.
`register_flush` must be acknowledged before starting a durable producer.
Cancellation is for superseded reads, never an accepted authoritative write.

### Constructors and event routing

Each constructor below returns `Self`, called through `cx.new(|cx| ...)`.
Views implement GPUI `Render` and the indicated `EventEmitter<Event>`.
Subscribe with `cx.subscribe` and retain the `Subscription`.
Workspace also implements `Focusable`; use its focus handle for
`EditorInit::return_focus`.

| Lane | Public constructor |
| --- | --- |
| Workspace | `workspace::WorkspaceView::new(LaneContext, Reply<()>, &mut Window, &mut Context<Self>)` |
| Editor | `editor::EditorPane::new(LaneContext, EditorInit, &mut Window, &mut Context<Self>)` |
| Meeting | `meeting::MeetingPane::new(LaneContext, MeetingIntent, &mut Window, &mut Context<Self>)` |
| Product | `product::ProductPane::new(LaneContext, ProductRoute, &mut Window, &mut Context<Self>)` |
| Runtime | `runtime_bridge::start(Profile) -> std::io::Result<(RuntimeHandle, Reply<()>)>` |

The integrator starts the runtime once, gives the readiness reply only to
workspace, and clones `LaneContext` into every other pane.

- Workspace emits `WorkspaceEvent::OpenEditor { session_id, document }`,
  `Meeting(MeetingIntent)` and `Product(ProductRoute)`. The final integrator
  creates/focuses the corresponding pane. Workspace owns session/folder
  selection, tabs, navigation history, search, contacts, calendar, templates and
  automations. It does not own document JSON or recorder internals.
  Only `OpenEditor` is emitted by the foundation; meeting and product routes
  become available when their workspace navigation is implemented.
- Editor receives `EditorInit { session_id, document, return_focus }`. It emits
  `Dirty { session_id, dirty }`, `Saved(DocumentSnapshot)`,
  `SaveFailed { session_id, error }`, `OpenLink(Arc<str>)`,
  `OpenAttachment(AttachmentId)` and `MentionHuman(HumanId)`.
  Dirty/save failure is authoritative and must not be coalesced away.
  The integrator handles external navigation and close guards. Native selection,
  rich text, UTF-16 mapping, IME/CJK, history, menus, clipboard, attachment
  insertion, lossless unknown nodes, concurrent sync and autosave belong here.
  The shared single-line input is not the rich-text editor.
- Meeting receives `MeetingIntent::{Open(SessionId), Start { session_id }, Stop}`.
  It emits `Recording { session_id, active }`, `OpenSession(SessionId)` and
  `Failed(ServiceError)`. The intent enum is a request, never proof that capture
  started. Runtime/audio workers must acknowledge start/stop and durable final
  transcript events. Meeting owns devices/capture, live partial/final words,
  speakers, retention including Never, recovery, playback/waveform, summaries and
  local/hosted AI execution. Keep transcript delivery reliable and partial UI
  snapshots coalesced independently.
- Product receives `ProductRoute::{Onboarding, Permissions, Settings, Account,
  Billing, CloudSync, Share(SessionId), Import, Export(SessionId), Models,
  Integrations}`. It emits `NavigateWorkspace`, `OpenSession(SessionId)` and
  `Failed(ServiceError)`. Auth/E2EE/settings orchestration, imports/exports and
  product integration state belong here. Never represent an unavailable
  provider, permission, license or secret as successful.
- Runtime owns paths/profile policy, native platform adapters, reactive
  subscription lifecycle, cancellation, startup/drain barriers, localization and
  accessibility foundations, and release-build instrumentation/fixtures.
  `platform::WindowRole::{Main, Settings, MeetingOverlay}` and
  `PlatformEvent::{DeepLink(String), Shortcut(String), OpenMainWindow,
  Failed(ServiceError)}` reserve the initial native boundary. Tray, deep links,
  shortcuts, dialogs, updater, notifications and alternate windows still need
  implementations; shared additions must be reported to the integrator.

### Assets and theme

`build.rs` derives all HSL colors from
`packages/design-system/src/tokens.css`. The shared radius is 8px. The CSS RGBA
keyboard-shadow tokens are not yet native shadow primitives.
`ui::theme::SYSTEM_FONT` selects `.SystemUIFont`, `Segoe UI` or Linux
`sans-serif`; font metrics still require platform comparison.
The existing logo is embedded directly. Hugeicons SVGs are generated from the
repository-pinned `@hugeicons/core-free-icons` package:

```sh
node apps/desktop-gpui/scripts/export-icons.mjs
```

Keep generated SVGs checked in so Rust builds do not require Node. Ask the
integrator to extend the generator/asset map for additional icons. Do not add
an unrelated icon family or replace the repository font policy.

## Verification and remaining gates

```sh
cargo check --locked -p desktop-gpui
cargo build --locked -p desktop-gpui
cargo test --locked -p desktop-runtime -p desktop-gpui
cargo clippy --locked -p desktop-runtime -p desktop-gpui --all-targets --no-deps -- -D warnings
find apps/desktop-gpui crates/desktop-runtime -name '*.rs' -print0 | xargs -0 rustfmt --edition 2024 --check
cargo tree --locked -p desktop-gpui --edges normal,build --target all
```

The runtime tests use temporary databases only, covering durable reopen,
opaque JSON preservation, CAS conflict and deleted-document rejection,
queue saturation/drain, flush barriers, cancellation and reactive unsubscribe.
Input model tests cover UTF-16, grapheme deletion and composition-relative ranges.
These do not validate native input methods or accessibility. Integration coverage
also exercises rich-document save/reopen through `SaveJournal`, preservation of
opaque/table/task content, competing writes, explicit conflict resolution, and
pin CAS/corrupt-storage behavior.

`pnpm dev:desktop-gpui --profile /absolute/path/to/sandbox` runs the native binary.
`pnpm check:desktop-gpui` runs the native check, tests, and Clippy. The focused
`desktop_gpui_ci.yaml` additionally builds the binary and checks Rust formatting.

This preview has not demonstrated appearance/interaction parity and makes no
performance claim. No GUI test or recording is performed in this child handoff.
macOS/Windows builds, native accessibility trees, screen readers, IME/CJK across
platforms, real audio, auth/providers, i18n, tray/updater/integration behavior,
packaging and signing remain unvalidated. The workspace's title/search input is
separate from the rich editor and still lacks full undo, drag selection, word
navigation and horizontal scrolling. Selected documents currently refresh only
when reopened; the editor/runtime typed document watch must still be connected
to the editor's remote-revision API without replacing dirty drafts.
Shutdown flush errors currently leave a visible failure on a closed runtime;
recovery/retry policy and OS session-end handling belong to runtime.

Standalone note windows, folder/contact/template mutations, automation execution,
provider calendar reconciliation, attachment preview, native search extraction,
secure credential/provider/model selection, meeting AI services,
account/CloudSync/billing adapters, and system permissions remain incomplete or
unavailable.

Equivalent release-build old/new profiling with reproducible large-library,
large-document and streaming fixtures is required before claiming any speed,
memory or responsiveness improvement. A successful Linux build is only a
build-viability result.
