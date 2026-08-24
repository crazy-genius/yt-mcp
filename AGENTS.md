# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`CLAUDE.md` is a symlink to `AGENTS.md` — edit `AGENTS.md`.

## Commands

```sh
cargo build --workspace
cargo test --workspace                       # wiremock-based integration tests
cargo test -p yt-client --test service_articles   # one test file
cargo test -p yt-client search_articles_paginates # one test by name
make lint                                    # = cargo fmt-check + cargo lint
cargo lint                                   # alias: clippy --workspace --all-targets --all-features -D warnings
cargo fmt-check                              # alias: fmt --all -- --check
make run                                     # cargo run -p yt-mcp-server (needs YOUTRACK_URL + MCP_TOKEN)
cargo run -p yt-client --example readme --features examples  # needs YOUTRACK_HOST + YOUTRACK_TOKEN
```

Rust edition 2024, `max_width = 100`, `use_small_heuristics = "Max"` — clippy runs with `-D warnings`, so
warnings are build failures in CI-equivalent checks.

## Runtime surface

One HTTP endpoint: `POST /mcp`, rmcp's streamable-HTTP transport. Two independent authorizations,
both headers, both required for a tool call:

- `Authorization: Bearer <MCP_TOKEN>` — level 1, a static gate on the server itself. Enforced by an
  axum layer *in front of* `StreamableHttpService`, so an unauthorized caller never establishes an
  MCP session and gets a plain HTTP 401 with `WWW-Authenticate: Bearer`.
- `X-YouTrack-Token: <user token>` — level 2, the caller's own YouTrack token. There is no system
  YouTrack token: every YouTrack request is made as the user who sent the header. Checked once in
  `call_tool`, so `initialize`, `tools/list` and `resources/*` still work without it — otherwise a
  client could not tell the user what is missing.

Environment (`Config::from_env` in `main.rs`):

| Variable | Required | Meaning |
| --- | --- | --- |
| `YOUTRACK_URL` | yes | YouTrack base URL; parsed at startup so a bad value kills the process instead of the first tool call |
| `MCP_TOKEN` | yes | the level-1 bearer secret; an empty value is refused at startup, because `""` would match `""` in the gate and let everyone in |
| `MCP_BIND` | no | listen address, default `127.0.0.1:8080` |
| `MCP_ALLOWED_HOSTS` | no | comma-separated `Host` allowlist for the transport; rmcp's default is loopback only, and reverse proxies forward the original `Host`, so without it a proxied deployment gets `403 Forbidden: Host header is not allowed` on every request |

Both tokens ride in headers, so **TLS termination at a reverse proxy is mandatory**: never expose
this server over plain HTTP.

## Architecture

Three crates, strictly layered bottom-up:

**`yt-client`** — the only place that talks to YouTrack. Wraps the external `yt-rs`
crate (pinned by git rev in the workspace `Cargo.toml`).
- `Youtrack` is the process-wide half: base URL plus a `reqwest` connection pool, no token.
  `Youtrack::as_user(token)` mints a `YoutrackService` for one caller; it costs about nothing
  because `reqwest::Client` clones by `Arc` and shares the pool.
- `YoutrackService` is stateless beyond its client. Write APIs need internal project ids while MCP
  callers use `shortName`, so they resolve the id per call — there is no cache, marked with a
  `ponytail:` note.
- Field selection is the central design concern: YouTrack returns only what `fields=` asks for.
  Each entity has a *list* default (lean, no `description`/`content` — keeps LLM tool results small)
  and a *card* default (full). `State`/`Priority`/`Assignee` live inside `customFields` and need
  nested selection to come back at all.
- Articles have no server-side query in the YouTrack API, so `search_articles` scans up to
  `ARTICLE_SCAN_LIMIT` (500) articles and filters locally; the response carries `matched`,
  `next_skip` and `scan_truncated` because MCP tool results have no protocol-level pagination.
- `WriteError` adds locally-detectable failures (unknown project shortName) on top of `yt-rs`'s
  `YoutrackError`; read paths return `yt_rs::Result` directly. Both surface to the model as tool
  errors in the core crate, not as protocol errors.

**`yt-mcp-core`** — the MCP surface over that service. `YoutrackMCPServer` implements
`rmcp::ServerHandler`.
- Tools are split into four `#[tool_router(router = "...", vis = "pub")]` impl blocks
  (`issues_tools`, `articles_tools`, `commands_tools`, `projects_tools`); `YoutrackMCPServer::new`
  sums the four routers into one. Adding a tool means adding it to a router block *and* to the name
  list in the `mcp.rs` test, which asserts the router holds exactly those tools.
- `ServerHandler` is implemented by hand, so `#[tool_handler]` generates nothing: `call_tool`,
  `list_tools` and `get_tool` are all written out. Each has a failure mode when it is missing —
  `-32601` on every call, an empty `tools/list` over the wire, and skipped SEP-2243 `Mcp-Param-*`
  validation respectively.
- Every tool body takes `Extension(token): Extension<UserToken>` and calls `self.as_user(&token)`;
  `call_tool` puts the header's token into the request extensions before dispatching. `UserToken`
  has a hand-written `Debug` that redacts — never derive it.
- YouTrack failures come back as `CallToolResult::error` (`is_error`), not protocol errors: the
  model can act on "project not found" and fix itself. Protocol errors are reserved for our bugs.
- All argument structs (`FindIssueArgs`, `SearchArticlesArgs`, …) live in `mcp.rs`; their doc
  comments become the JSON Schema descriptions the LLM reads, so they carry real usage guidance.
- Call policy lives in MCP annotations (`read_only_hint`, `destructive_hint`, `idempotent_hint`,
  `open_world_hint`, `title`) on the `#[tool(...)]` attribute, not in the description text.
  `idempotent_hint = false` is set on all six write tools. Exactly two tools are
  destructive: `youtrack_update_article` (replaces the whole body) and `youtrack_apply_command`.
- Three MCP resources are served from `references.rs` (large `&str` consts, not files):
  `youtrack://reference/issues/search/query-syntax`, `.../issues/fields`, `.../articles/fields`.
  These teach the model YouTrack's query language and `fields` syntax.
- `extract_tools()` exists so a gateway can read the tool list without going through the transport.

**`yt-mcp-server`** — the binary, a single `main.rs`. Reads the environment above, builds one
`Youtrack`, and mounts `StreamableHttpService` at `/mcp` behind the level-1 `gate` middleware. The
router is built by `app()` rather than inline in `main` so the tests drive the real wiring instead
of a paraphrase of it. The bearer comparison goes through `subtle::ConstantTimeEq` — a naive `==`
leaks the secret prefix by prefix through timing, and one of the gate tests pins that by rejecting
a correct prefix.

## Tests

Integration tests live in `crates/yt-client/tests/` and use `wiremock` to assert the
exact HTTP the wrapper produces — mostly that the right `fields=` and `query=` strings go out. When
changing a default field list, the expected string constants in `service_read.rs` /
`service_articles.rs` must be updated too.

`crates/yt-mcp-core/tests/dispatch.rs` runs a real MCP client against the server over an in-memory
duplex stream: it pins that `tools/list` returns the router's tools and that a tool call without a
token comes back as a readable `is_error`. That transport has no HTTP headers, so it cannot reach a
tool body — the end-to-end check lives in `yt-mcp-server`'s `a_tool_call_travels_from_the_wire_to_youtrack`,
which drives the full `app()` through `tower::ServiceExt::oneshot` with both headers against a
`wiremock` YouTrack whose mock matches on `Authorization: Bearer user-token`. That mock being hit is
the only place the token that *arrived* in `X-YouTrack-Token` is checked against the one that *left*
for YouTrack, which is the whole point of the passthrough.

`crates/yt-client/docs/openapi.json` (500 KB) is the YouTrack OpenAPI spec, kept for
reference when adding endpoints.

## Conventions

Doc comments and inline explanations in this codebase are written in Russian; keep that style when
editing existing modules. `ponytail:` comments mark deliberate simplifications with a named ceiling
and upgrade path.

`README.md` is an unmodified `wasm-pack-template` leftover and describes nothing about this project.
