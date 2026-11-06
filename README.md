# yt-mcp

MCP-сервер для YouTrack: агенты (Claude Code и другие MCP-клиенты) ищут и правят задачи, статьи,
комментарии и вложения от имени пользователя, приславшего свой YouTrack-токен.

```sh
YOUTRACK_URL=https://youtrack.example.com MCP_TOKEN=... make run   # POST /mcp на 127.0.0.1:8080
cargo test --workspace && make lint
```

Окружение, авторизация, архитектура и тесты описаны в [AGENTS.md](AGENTS.md); деплой — в
[deployments/kustomize/README.md](deployments/kustomize/README.md) и `docker/Dockerfile`.
