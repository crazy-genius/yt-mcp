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

One MCP endpoint: `POST /mcp`, rmcp's streamable-HTTP transport. With OAuth v2 configured (see
"OAuth v2" below) the server also serves the OAuth routes next to it. By default (v1) there are
two independent authorizations, both headers, both required for a tool call:

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
| `MCP_PUBLIC_URL` | for v2 | external https origin of this server (`https://yt-mcp.dev`; `http` only for loopback; no path, query, fragment or userinfo — startup error otherwise): the OAuth `issuer`, the protected `resource`, and the base of the Hub callback `{MCP_PUBLIC_URL}/mcp/auth_callback` |
| `HUB_CLIENT_ID` | for v2 | id of the service registered in YouTrack Hub with that callback as its redirect URI |
| `HUB_CLIENT_SECRET` | no | set: the Hub client is confidential (token requests use Basic auth); unset or blank: public |
| `MCP_OAUTH_REDIRECT_URIS` | for v2 | comma-separated redirect URIs of MCP clients we may send users back to; exact match, except that a loopback `http` entry (`localhost`, `127.0.0.1`, `[::1]`) accepts any client port; a URI with a fragment or userinfo is never trusted |
| `MCP_OAUTH_UPSTREAM_TIMEOUT_SECS` | no (v2 only) | total timeout of requests to Hub, parsed only when v2 is configured (ignored otherwise), integer 1..=60, default 8 (connect timeout is min(10 s, this)); empty is unset, a bad value is a startup error |

### OAuth v2

A client opts in per request with `X-API-Version: v2`. Then `Authorization: Bearer` carries the
user's own YouTrack Hub token instead of `MCP_TOKEN` + `X-YouTrack-Token`, and the server is an
OAuth proxy in front of Hub so clients log in without manual setup. v2 is on only when
`MCP_PUBLIC_URL`, `HUB_CLIENT_ID` and `MCP_OAUTH_REDIRECT_URIS` are all set; some of them is a
startup error naming the missing one, none of them leaves v2 off. At startup the Hub endpoints and
the scope (the Hub service id) are discovered from `{YOUTRACK_URL}/api/config` and Hub's
`.well-known/oauth-authorization-server`; failure kills the process, and `main` returns
`anyhow::Result` so the whole error chain is printed. `MCP_TOKEN` stays required for v1.

- The OAuth routes (`/.well-known/*`, `/register`, `/authorize`, `/mcp/auth_callback`, `/token`)
  are mounted outside the level-1 gate, otherwise a client without a token could not start the
  login. `/mcp` itself stays behind the gate.
- In v1 the gate strips `X-API-Version` before the transport, so core never takes `Authorization`
  as a YouTrack token unless the gate verified it as v2. If v2 is not configured the header is
  simply ignored and the request is treated as v1.
- In v2 the gate does not check `MCP_TOKEN`. A bearer equal to `MCP_TOKEN` is refused with
  `invalid_token` without contacting YouTrack, so the server secret is never sent upstream. A
  missing or empty bearer gets 401 with the `resource_metadata` challenge (no `error=`); the scheme
  is case-insensitive.
- Verification is a request to YouTrack as the bearer. YouTrack 401/403 (and yt-rs `ApiError`)
  means 401 `invalid_token`; any other status or transport failure means 502. Successes are cached
  60 s (so a revoked token works for up to a minute), failures are never cached. Verification
  happens in the gate, before the transport, because the attachment-by-URL download runs before any
  YouTrack call and an unverified request reaching a tool is an SSRF vector.
- `/register` is stateless: it validates the client's redirect URIs against
  `MCP_OAUTH_REDIRECT_URIS` and always hands out `client_id` `yt-mcp`.
- `/authorize` stores the client's redirect URI and `state` under our own CSPRNG `state` (TTL
  600 s, at most 10 000 entries) and sends the browser to Hub with `client_id=HUB_CLIENT_ID` and our
  callback as `redirect_uri`; `/mcp/auth_callback` maps it back, keeping any query the client's URI
  already had and omitting `state` when the client sent none.
- `/token` proxies to Hub, rewriting `client_id` and `redirect_uri` (not added for
  `grant_type=refresh_token`); Basic auth to Hub when `HUB_CLIENT_SECRET` is set, otherwise the
  client is public. Neither the secret, a `code`, nor any token is logged.
- `/authorize` asks Hub for a `refresh_token` with `access_type=offline` and `scope` equal to the
  YouTrack service id only, as Hub's Authorization Code docs describe; Hub does not document an
  `offline_access` scope. Hub issues the refresh token on the first code exchange for a user.

Which mode a client uses is decided by the headers it sends. Hosted clients (claude.ai, Desktop,
mobile) cannot set custom headers, so they connect through a gateway that sets `X-API-Version` (v2
for the OAuth flow) in front of this server; Claude Code sets headers itself (see README). The
Hub-facing OAuth requests use the `MCP_OAUTH_UPSTREAM_TIMEOUT_SECS` timeout because Claude waits
only about 10 s for discovery, registration and token responses. Startup Hub discovery
(`discover` in `main.rs`) uses the same client, so a Hub slower than that timeout at boot kills the
process (the pod restarts until Hub answers). The AS metadata advertises
`client_id_metadata_document_supported: true`; the client's metadata document is **not** fetched
(`ponytail:` in `oauth/endpoints.rs`): `/authorize` ignores `client_id`, and a redirect URI is
trusted only through `MCP_OAUTH_REDIRECT_URIS`, never checked against the client's own document.
Upgrade path: fetch the document and compare its `redirect_uris`. Loopback entries in
`MCP_OAUTH_REDIRECT_URIS` ignore only the port; scheme, host and path must match exactly.

**Run a single replica** (or put sticky sessions in front). Three pieces of state live in process
memory: rmcp's `LocalSessionManager` (sessions identified by `Mcp-Session-Id`; another pod answers
404 for a foreign id), the OAuth `pending` map (the `/authorize` → `/mcp/auth_callback` state, so
the callback must land on the pod that started the login) and the `verified` token cache. The
kustomize base and example overlay set `replicas: 1` for the same reason.

In both modes the tokens ride in headers (v1: `MCP_TOKEN` and the YouTrack token; v2: the Hub
bearer), so **TLS termination at a reverse proxy is mandatory**: never expose this server over
plain HTTP.

Known limitations of v2: a token that expires or is revoked upstream keeps passing the gate for up
to 60 s (the verification cache), and its tool calls then fail as `is_error` results rather than
HTTP 401. With v2 configured, a v1 client with a wrong `MCP_TOKEN` may be led by its client into
the OAuth flow and still be rejected afterwards, because it sends no `X-API-Version`.

Attachment uploads take bytes through one of two channels, exactly one per call:
`content_base64` (for content the model produced itself) or `url`, which the **server** downloads
through a dedicated `reqwest::Client` (`attachment_http` in `Youtrack::new`), separate from the
pool used for every YouTrack API call. That client is built with `redirect::Policy::none()`,
`https_only(true)` and a 30-second timeout, and `fetch_bytes` rejects a 3xx response explicitly —
`error_for_status()` does not treat 3xx as an error, so without that check a refused redirect would
silently produce an empty "successful" file instead of failing. The refusal matters because
reqwest's default redirect policy follows up to 10 hops: an `https` URL that answers
`302 → http://169.254.169.254/...` would sail straight through, and the https-only rule
(`validate_attachment_url`) would have held for exactly one hop — `https_only(true)` closes that
same hop at the transport level too, as defense in depth. Both channels share `MAX_ATTACHMENT_BYTES`
(10 MiB); `Content-Length` is checked before the body is read, and the bytes are still counted while
reading because that header lies. What is still deliberately **not** checked is where the host
resolves — no DNS check, no private-range
refusal, no allowlist — so the server will fetch any https address reachable from its network
perimeter and the content lands as a YouTrack attachment: a prompt injection carrying an internal
URL is a real vector. A `ponytail:` note in `yt-client` names the ceiling and the upgrade path:
resolve the host and refuse loopback/private/link-local/CGNAT, or an env allowlist.

Reading an attachment costs two requests on purpose: metadata first (`mimeType`, `size`), then the
content only if the mime is text or an image and the reported size fits `MAX_READ_TEXT_BYTES`
(256 KiB) / `MAX_READ_IMAGE_BYTES` (5 MiB). One request would be shorter, but a 200 MB PDF would
travel into the server's memory only to be refused afterwards. `size` can come back `null` (YouTrack
does not always compute it); a text or image with an unknown size is refused, not read, because
`base64Content` arrives whole before any later check could stop it — treating `null` as zero would
let one big image OOM the pod. `size` is still only what YouTrack *claims*, so `content_block` in
`yt-mcp-core` re-checks the same limit against `content.bytes.len()`, the size of what the second
request actually returned, before building the tool result.

Every YouTrack API call shares one `reqwest::Client` built with a 10 s connect timeout and a 120 s
total timeout (room for a 10 MiB upload): reqwest's default is to wait forever, and a YouTrack that
accepts the connection and goes silent would otherwise hang the tool call and its session.

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
- Tools (30) are split into eight `#[tool_router(router = "...", vis = "pub")]` impl blocks
  (`issues_tools`, `articles_tools`, `commands_tools`, `projects_tools`, `comments_tools`,
  `attachments_tools`, `users_tools`, `reference_tools`); `YoutrackMCPServer::new` sums them into
  one. `comments_tools`, `attachments_tools` and `users_tools` are split by *capability*, not by
  entity: comments and attachments exist on both issues and articles, and putting them next to
  their entity would have grown `issues_tools` to twelve tools in one file. Adding a tool means adding it to a router block *and* to both
  tables in the `mcp.rs` tests — the name list, which asserts the router holds exactly those tools, and the call-policy
  table.
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
  comments become the JSON Schema descriptions the LLM reads, so they are English only (a test,
  `tool_schemas_carry_no_cyrillic`, enforces it; notes for developers go in plain `//`).
- Tool descriptions and schemas state facts (what the tool does, where ids come from); behavioural
  guidance ("prefer X", "always do Y") lives in `INSTRUCTIONS` in `mcp.rs`, which `get_info`
  returns as MCP `instructions` next to the server name `yt-mcp` and the `yt-mcp-core` version.
  `tool_texts_describe_instead_of_steering` fails on steering phrases in tool texts.
- Successful JSON results are `structuredContent` plus the same value as compact (no indentation)
  text in `content[0]`. MCP requires `structuredContent` to be an object, so an array is wrapped as
  `{"items": [...]}`. No `outputSchema` is declared: the `yt-rs` models do not implement
  `JsonSchema`.
- `youtrack_search_issues` and `youtrack_my_issues` take `top` (default 50, clamped to 1..=100) and
  `skip`, and return `{"issues": [...], "next_skip": <int|null>}`; the client asks YouTrack for
  `top + 1` to know whether another page exists.
- `youtrack_reference(topic)` (`reference_tools`; `topic` is `query-syntax`, `issue-fields` or
  `article-fields`) is read-only and returns the same texts as the resources below, for clients
  that do not expose MCP resources to the model. It still requires the user token like every tool.
- Four prompts are served from `prompts.rs` (`triage_my_issues`, `summarize_issue`, `draft_issue`,
  `kb_from_issues`). `list_prompts`/`get_prompt` are written by hand like the other handler
  methods, and prompts need no token. Their text is English, asks for the user's language, and
  never writes to YouTrack without the user's confirmation.
- Call policy lives in MCP annotations (`read_only_hint`, `destructive_hint`, `idempotent_hint`,
  `open_world_hint`, `title`) on the `#[tool(...)]` attribute, not in the description text.
  `annotations_match_the_call_policy_table` in `mcp.rs` holds the whole policy as a table of
  30 rows and is the place to read it. Nine tools are destructive: `youtrack_update_article`,
  `youtrack_update_issue`, `youtrack_apply_command`, and the six `update_*`/`delete_*` tools for
  comments and attachments. Every `update_*`/`delete_*` tool is `idempotent_hint = true` — a
  repeat with the same arguments leaves the same state — while everything that *adds* an entity is
  `idempotent_hint = false`, and so is `apply_command` (a command that logs spent time adds a second
  work item on repeat). Summary and description of an issue change only through
  `youtrack_update_issue`: YouTrack's command language cannot set them, so `apply_command` is no
  substitute. A call with neither field is refused locally — an empty POST would come back as the
  unchanged card and read as a successful edit.
- `youtrack_apply_command` links the YouTrack command language docs in its description.
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
duplex stream: it pins that `tools/list` returns the router's tools, that `prompts/list` returns
the four prompts, and that a tool call without a token comes back as a readable `is_error`. That
transport has no HTTP headers, so it cannot reach a
tool body — the end-to-end check lives in `yt-mcp-server`'s `a_tool_call_travels_from_the_wire_to_youtrack`,
which drives the full `app()` through `tower::ServiceExt::oneshot` with both headers against a
`wiremock` YouTrack whose mock matches on `Authorization: Bearer user-token`. That mock being hit is
the only place the token that *arrived* in `X-YouTrack-Token` is checked against the one that *left*
for YouTrack, which is the whole point of the passthrough.

`crates/yt-client/docs/openapi.json` (500 KB) is the YouTrack OpenAPI spec, kept for
reference when adding endpoints.

`crates/yt-client/src/lib.rs` carries unit tests for the attachment helpers — the `data:` prefix
strip, base64 decoding with its limit, the https-only URL check and `fetch_bytes`. The URL check
and the fetch are separate functions precisely so both are testable: wiremock speaks http, while
the production rule is https-only, so a single combined function could not be covered at all.

Other tests worth knowing: `json_results_are_compact_and_structured` and
`server_info_names_yt_mcp_and_carries_instructions` in `mcp.rs`; `the_reference_tool_returns_the_resource_text`;
`search_issues_asks_one_extra_and_reports_next_skip` in `service_read.rs`; in `main.rs::tests`
`as_metadata_advertises_cimd_for_public_clients`; in `oauth/config.rs`
`the_upstream_timeout_defaults_to_8_seconds_and_is_bounded`.

OAuth v2 tests: unit tests live next to the code in `crates/yt-mcp-server/src/oauth/*.rs`
(settings parsing, redirect trust rules, the TTL map, verification). The router-level tests
are in `main.rs::tests` and drive the real `app()` with `tower::ServiceExt::oneshot` against
`wiremock` standing in for Hub and YouTrack: metadata, `/register`, `/authorize` and the callback,
the `/token` rewrite, and the gate's v2 branch (challenge, `invalid_token`, 502, caching).

`crates/yt-mcp-core/src/mcp/attachments_tools.rs` unit-tests `plan_read` — which mime becomes
text, which becomes an image, and which is refused before anything is downloaded (including any
text or image whose `size` is `null`) — and separately
`content_block`, which re-checks the same limit against the bytes the content request actually
returned, independent of what `size` said in the metadata request.

## Conventions

Doc comments and inline explanations in this codebase are written in Russian; keep that style when
editing existing modules. `ponytail:` comments mark deliberate simplifications with a named ceiling
and upgrade path.

