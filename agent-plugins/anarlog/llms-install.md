# Set up Anarlog agent access

1. Honor the user's requested source. Otherwise, on the user's computer check whether `anarlog` is on `PATH` (`anarlog-cli` on Flatpak), then run `anarlog --json doctor`. Prefer `anarlog --json meetings --source local ...` when ready. Local access requires no OAuth, Pro subscription, or completed sync.
2. If the agent supports local MCP, configure a server named `anarlog-local` with command `anarlog` and arguments `["mcp"]`. Do not start it in a remote environment without the user's local database.
3. For remote access or an absent local CLI/database, use Cloud MCP if authorized. Confirm Anarlog Pro and **Settings → Developers → Cloud API & Connectors** are enabled, then connect an HTTP MCP server at `https://api.anarlog.so/mcp` through the host's OAuth flow. Installing a skill alone does not connect an account. Do not paste a cloud API key unless the host cannot complete MCP OAuth.
4. Confirm the selected MCP server lists `list_meetings`, `get_meeting`, `get_meeting_transcript`, `get_recurring_meeting_history`, and `export_meeting`. Label the source and keep related reads on it. An empty search is not a missing database; report database errors instead of silently falling back. If neither source is available, explain local installation or Cloud connection setup. Do not install software or enable Cloud uploads without authorization.
5. Never query or modify Anarlog's SQLite database directly.

The hosted server is read-only. Staging a note or summary edit requires the local CLI or local MCP. Cloud snapshots are separate from encrypted Cloud Sync. Report freshness only when an interface provides it; a meeting's `updated_at` does not establish sync or upload completion.
