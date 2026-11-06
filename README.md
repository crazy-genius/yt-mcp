# yt-mcp

MCP-сервер для YouTrack: агенты (Claude Code и другие MCP-клиенты) ищут и правят задачи, статьи,
комментарии и вложения от имени пользователя, приславшего свой YouTrack-токен.

```sh
YOUTRACK_URL=https://youtrack.example.com MCP_TOKEN=... make run   # POST /mcp на 127.0.0.1:8080
cargo test --workspace && make lint
```

Окружение, авторизация, архитектура и тесты описаны в [AGENTS.md](AGENTS.md); деплой — в
[deployments/kustomize/README.md](deployments/kustomize/README.md) и `docker/Dockerfile`.

## Подключение к Claude

**Claude Code** ставит заголовки сам. Режим v1 (статические токены):

```sh
claude mcp add --transport http yt https://yt-mcp.example.com/mcp \
  --header "Authorization: Bearer <MCP_TOKEN>" \
  --header "X-YouTrack-Token: <permanent-token>"
```

Режим v2 (вход через YouTrack Hub, без ручных токенов); в `Authorization` Claude Code подставит
токен Hub после входа (`/mcp` в сессии):

```sh
claude mcp add --transport http yt https://yt-mcp.example.com/mcp --header "X-API-Version: v2"
```

Для v2 сервер должен быть настроен (`MCP_PUBLIC_URL`, `HUB_CLIENT_ID`, `MCP_OAUTH_REDIRECT_URIS`),
а в `MCP_OAUTH_REDIRECT_URIS` для Claude Code нужны loopback-записи
`http://localhost/callback,http://127.0.0.1/callback`: порт может быть любым, путь должен совпасть.

**claude.ai, Claude Desktop и мобильное приложение** не умеют ставить свои заголовки, поэтому
подключаются через gateway, который выставляет режим (`X-API-Version: v2`) перед сервером. В
`MCP_OAUTH_REDIRECT_URIS` обязательно добавить `https://claude.ai/api/mcp/auth_callback`.

Claude ждёт ответа OAuth-эндпоинтов (discovery, регистрация, обмен кода) около 10 с. Запросы
сервера в Hub ограничены `MCP_OAUTH_UPSTREAM_TIMEOUT_SECS` (1..=60, по умолчанию 8; читается только при включённом v2); если Hub
медленный, не поднимайте значение выше 10.

Сервер держит сессии MCP, незавершённые входы и кэш токенов в памяти процесса, поэтому запускайте
**одну реплику** (или включите sticky sessions): иначе запрос с чужим `Mcp-Session-Id` получит 404,
а возврат из Hub попадёт не в тот под.
