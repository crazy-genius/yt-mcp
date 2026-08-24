FROM rust:1.95-alpine3.23 AS builder

WORKDIR /src

# deps
RUN apk add --no-cache musl-dev

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# actual build. Пакет содержит ровно один бинарь, и Cargo называет его по
# имени пакета — `yt-mcp-server`, не `yt-mcp`.
RUN cargo build --release --locked -p yt-mcp-server

# validate
RUN ldd target/release/yt-mcp-server 2>&1 | grep -q '=>' \
    && { echo "ERROR: binary is dynamically linked"; exit 1; } || true


FROM alpine:3.23

RUN apk add --no-cache ca-certificates \
    && addgroup -g 65532 -S yt \
    && adduser -u 65532 -G yt -S -D -H -s /sbin/nologin yt

COPY --from=builder --chown=root:root --chmod=0555 \
     /src/target/release/yt-mcp-server /usr/local/bin/yt-mcp

USER 65532:65532

# В контейнере loopback недостижим снаружи, поэтому слушаем все интерфейсы.
ENV MCP_BIND=0.0.0.0:8080

# Транспорт rmcp по умолчанию принимает только loopback-хосты и проверяет
# заголовок Host на каждом запросе. Обратный прокси пробрасывает исходный
# Host, поэтому без MCP_ALLOWED_HOSTS проксированный деплой получает
# 403 Forbidden на КАЖДЫЙ запрос. Список задаётся через запятую и заменяет
# дефолт целиком — если нужен доступ и по localhost, включи его явно:
#   -e MCP_ALLOWED_HOSTS=mcp.example.com,mcp.example.com:8443,127.0.0.1
ENV MCP_ALLOWED_HOSTS=""

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/yt-mcp"]
