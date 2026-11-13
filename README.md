# Project Rust

基于 Rust 的聊天服务，使用 Axum/Tokio 提供 HTTP API，通过 SQLx/PostgreSQL 持久化会话与消息，调用兼容 OpenAI Chat Completions 协议的模型接口。

## 启动后端

在项目根目录运行。先安装并启动 Docker Desktop，以及下文的 SQLx CLI。新检出项目时复制环境变量模板；已有 `.env` 时保留现有文件：

Windows 编译需要 Visual Studio Build Tools 的 MSVC C++ 工具集和 Windows SDK。请使用 Developer PowerShell 或 x64 Native Tools Command Prompt，确保调用的是 Microsoft 链接器，而不是 Git/MSYS 环境中的同名 `link.exe`。

```powershell
if (-not (Test-Path .env)) { Copy-Item .env.example .env }
powershell -ExecutionPolicy Bypass -File .\scripts\start-db.ps1
sqlx migrate run
cargo run -p ai-server
```

默认地址为 `http://127.0.0.1:3000`，可通过 `.env` 中的 `PORT` 修改。

## API

| 方法 | 路径 | 请求体 / 用途 |
| --- | --- | --- |
| GET | `/api/health` | 服务状态与版本 |
| POST | `/api/echo` | `{"message":"Hello Rust"}` |
| POST | `/api/chat` | `{"prompt":"Hello Rust"}` |
| POST | `/api/conversations` | `{"title":"first chat"}`；创建会话，返回 201 |
| GET | `/api/conversations` | 列出会话，按创建时间倒序 |
| DELETE | `/api/conversations/{id}` | 删除会话及其消息，返回 204；不存在返回 404 |
| GET | `/api/conversations/{id}/messages?limit=50` | 获取该会话最近 N 条消息，按时间正序返回 |
| POST | `/api/conversations/{id}/chat` | `{"prompt":"Hello Rust"}`；多轮聊天，返回已保存的助手消息 |
| GET | `/api/stream-demo` | SSE 模拟流，约每 200ms 推送一个单词，共 25 条事件 |

`LLM_API_BASE` 和 `LLM_MODEL` 应填写同一服务商支持的接口与模型。`LLM_API_KEY` 留空或为模板占位值时，聊天接口返回本地 Mock 回复。配置保存在被 Git 忽略的 `.env` 中，不要提交真实密钥。

## 数据库

先安装并启动 Docker Desktop，使用 Linux 容器。然后在项目根目录运行：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\start-db.ps1
```

脚本兼容 Docker Desktop 的用户目录安装和系统目录安装，会检查 Docker、校验 Compose 配置、等待数据库就绪，再查询当前数据库、用户及可用的 vector 扩展。也可以在能识别 `docker` 命令的新终端中手动执行：

```powershell
docker compose config --quiet
docker compose up -d --wait postgres
docker compose ps
docker compose exec postgres psql -U ai_saas -d ai_saas -c "SELECT current_database(), current_user;"
```

数据库使用 `pgvector/pgvector:pg16`，仅绑定本机 `127.0.0.1:5432`，数据保存在命名卷 `postgres_data`。本地开发账号为 `ai_saas`，密码为 `ai_saas_dev`，数据库名为 `ai_saas`。

在 `.env` 中配置：

```dotenv
DATABASE_URL=postgres://ai_saas:ai_saas_dev@127.0.0.1:5432/ai_saas
```

后端启动时读取该配置建立连接池；SQLx CLI 与编译期 SQL 检查也使用它。使用 `127.0.0.1`，避免 Windows 将 `localhost` 解析到未映射的 IPv6 地址 `::1`。

安装与 SQLx 0.8 配套的 CLI：

```powershell
cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features rustls,postgres
sqlx --version
sqlx database create
sqlx migrate run
```

Compose 首次初始化时会创建数据库；`sqlx database create` 确保目标数据库存在。`sqlx migrate run` 按版本执行尚未应用的迁移，创建 `conversations`、`messages` 两张表及索引。当前迁移不启用 vector 扩展，服务不使用向量检索。

常用维护命令：

```powershell
docker compose logs --tail 50 postgres
docker compose stop postgres
docker compose start postgres
```

正常停止容器会保留数据。账号和数据库名的初始化环境变量只在首次创建数据目录时生效。

## 会话与消息历史

- `save_message` 保存 `system`、`user`、`assistant` 消息；数据库拒绝其他角色。
- `get_recent_messages` 先按 `(created_at DESC, id DESC)` 取最近 N 条，再翻转为正序。时间相同时以 UUID 稳定排序，不代表实际发送先后。
- 历史查询只返回指定会话的消息。`limit` 默认 50，数值限制在 1–200；例如 0 按 1 处理，999 按 200 处理。
- 已存在但没有消息的会话返回 `200 + []`；不存在的会话返回 `404`，错误代码为 `NOT_FOUND`。
- 非法 UUID 或非整数 `limit` 返回 `400`。Axum 0.8 路由参数使用 `/{id}`，不是 `/:id`。
- 删除会话会通过外键级联删除消息。

后端运行后，可在另一个 PowerShell 终端中调用接口：

```powershell
$base = "http://127.0.0.1:3000"
$conversation = Invoke-RestMethod -Method Post -Uri "$base/api/conversations" `
    -ContentType "application/json" -Body '{"title":"history-example"}'
Invoke-RestMethod -Uri "$base/api/conversations"
Invoke-RestMethod -Uri "$base/api/conversations/$($conversation.id)/messages?limit=3"
Invoke-RestMethod -Method Delete -Uri "$base/api/conversations/$($conversation.id)"
```

新建会话的历史是空数组。消息写入使用会话聊天接口 `POST /api/conversations/{id}/chat`；`POST /api/chat` 是独立问答，不保存会话历史。

## 多轮聊天持久化

`POST /api/conversations/{id}/chat` 的处理顺序：

1. 检查提问非空、会话存在。
2. 保存 `user` 消息，读取该会话最近 50 条历史，包含当前提问且不重复追加。
3. 按历史顺序将 `role` 和 `content` 发送给 LLM。
4. 保存 `assistant` 回复，返回该消息的 `id`、`conversation_id`、`role`、`content`、`token_count` 和 `created_at`。调用方读取 `content`，不是旧接口的 `reply`。

空白提问返回 400，不存在的会话返回 404，均不保存消息。LLM 调用失败返回 500，已保存的用户消息保留，不伪造助手回复；再次发送会另存一条用户消息，没有自动重试去重。串行发送同一会话的请求；当前没有并发轮次隔离。

配置同一提供商支持的 `LLM_API_BASE`、`LLM_MODEL` 和有效 `LLM_API_KEY`，重启后端后用 PowerShell 调用会话聊天接口：

```powershell
$base = "http://127.0.0.1:3000"
$conversation = Invoke-RestMethod -Method Post -Uri "$base/api/conversations" `
    -ContentType "application/json" -Body '{"title":"chat-example"}'
$prompts = @(
    "Remember this code: RUST-731.",
    "What code did I ask you to remember?",
    "Repeat that code without explanation."
)
foreach ($prompt in $prompts) {
    $body = @{ prompt = $prompt } | ConvertTo-Json
    $reply = Invoke-RestMethod -Method Post `
        -Uri "$base/api/conversations/$($conversation.id)/chat" `
        -ContentType "application/json" -Body $body
    $reply.content
}
Invoke-RestMethod -Uri "$base/api/conversations/$($conversation.id)/messages" |
    Format-Table role, content
```

多轮请求使用同一个会话 ID，历史包含依次保存的用户消息和助手回复。本地 Mock 只回显最后一条用户提问，不执行真实模型推理。

## LLM 流式客户端

`ai_core::llm::LlmClient::chat_stream(&messages)` 向 OpenAI 兼容的 `/chat/completions` 接口发送 `stream: true`，返回 `Stream<Item = anyhow::Result<String>>`。结果流支持 `Send + 'static`，不借用调用方的客户端或消息。

客户端按 SSE 事件解析响应，逐段产出非空 `delta.content`，跳过角色和用量事件，以 `[DONE]` 正常结束。HTTP 错误、无效 JSON、读取失败或未收到 `[DONE]` 的中断均返回错误；流式请求要求配置真实密钥，不使用 Mock。

在根目录 `.env` 配置 `LLM_API_BASE`、`LLM_API_KEY` 和 `LLM_MODEL` 后运行：

```powershell
cargo run -p ai-core --example stream_client
```

示例逐段刷新标准输出，无需启动数据库或 HTTP 服务。客户端总请求超时为 60 秒；此接口本身不写入数据库，也不新增 HTTP 路由。

## SSE 模拟流

`GET /api/stream-demo` 返回 `Content-Type: text/event-stream`，循环推送 `Rust`、`streams`、`tokens`、`with`、`SSE`，约 5 秒后结束连接。

接口通过有界 Tokio 通道和 `ReceiverStream` 输出事件。客户端断开后，发送失败会停止生成；接口不调用模型、不写入数据库。

使用 `curl.exe -N` 禁用客户端输出缓冲，观察逐条到达的事件：

```powershell
curl.exe -N http://127.0.0.1:3000/api/stream-demo
```

## SQLx 离线编译

根目录 `.sqlx/query-*.json` 保存 SQL 宏所需的参数与结果列类型，应随代码提交 Git。不要手改这些生成文件。

修改 SQL 宏查询或相关表结构后，先启动数据库、执行迁移，再重新生成并校验：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\start-db.ps1
sqlx migrate run
$previousOffline = $env:SQLX_OFFLINE
try {
    $env:SQLX_OFFLINE = "false"
    cargo sqlx prepare --workspace -- --all-targets
    if ($LASTEXITCODE -ne 0) { throw "SQLx metadata generation failed" }
    cargo sqlx prepare --workspace --check -- --all-targets
    if ($LASTEXITCODE -ne 0) { throw "SQLx metadata is not up to date" }

    $env:SQLX_OFFLINE = "true"
    cargo check --workspace --all-targets
    if ($LASTEXITCODE -ne 0) { throw "SQLx offline compilation failed" }
}
finally {
    $env:SQLX_OFFLINE = $previousOffline
}
```

`--workspace` 使用统一的根目录缓存，`--all-targets` 覆盖测试中的 SQL 宏。生成和 `--check` 校验需要真实数据库；`SQLX_OFFLINE=true` 强制编译时读取缓存，不连接数据库，也不会让运行中的服务脱离数据库。它与 Cargo 的依赖离线模式 `--offline` 不同。

## 开发检查

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

首次生成或校验 SQLx 元数据需要本机 PostgreSQL 正常运行且已执行迁移；已有 `.sqlx` 缓存时可用 `SQLX_OFFLINE=true` 离线编译。SQLx 数据库集成测试执行时仍需要数据库连接和创建临时数据库的权限，Compose 的本地开发账号满足该条件。

回归测试覆盖消息读写、最近 N 条及同时间戳排序、会话隔离、历史数量上下界、空历史、404、删除级联，以及聊天非法输入不写库、提供商失败时保留用户消息。SQLx 测试使用独立临时数据库，不写入开发会话。

GitHub Actions 的构建和测试步骤均设置 `SQLX_OFFLINE=true`，编译 SQL 宏时使用提交的缓存。PostgreSQL 服务与迁移步骤仍保留，供运行数据库集成测试使用；任何迁移 SQL 错误都会立即使步骤失败。
