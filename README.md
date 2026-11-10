# Project Rust

按每日实战计划构建 Rust + Next.js AI SaaS。当前已实现 Day 13 多轮聊天持久化：用户提问存库、读取历史调用 LLM、助手回复存库。Day 12 的会话与消息历史接口保持可用。真实模型三轮记忆验收需要有效 API Key；本地 Mock 不能证明模型记忆。

## 启动后端

在项目根目录运行。先安装并启动 Docker Desktop，以及下文的 SQLx CLI。新检出项目时复制环境变量模板；已有 `.env` 时保留现有文件：

```powershell
if (-not (Test-Path .env)) { Copy-Item .env.example .env }
powershell -ExecutionPolicy Bypass -File .\scripts\start-db.ps1
sqlx migrate run
cargo run -p ai-server
```

默认地址为 `http://127.0.0.1:3000`，可通过 `.env` 中的 `PORT` 修改。

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

`LLM_API_BASE` 和 `LLM_MODEL` 应填写同一服务商支持的接口与模型。`LLM_API_KEY` 留空或为模板占位值时，聊天接口返回本地 Mock 回复。

## Day 8：启动数据库

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

Compose 首次初始化时会创建数据库；`sqlx database create` 确保目标数据库存在。`sqlx migrate run` 按版本执行尚未应用的迁移，创建 `conversations`、`messages` 两张表及索引。当前迁移不启用 vector 扩展；Day 12 尚不使用向量检索。

常用维护命令：

```powershell
docker compose logs --tail 50 postgres
docker compose stop postgres
docker compose start postgres
```

正常停止容器会保留数据。账号和数据库名的初始化环境变量只在首次创建数据目录时生效。

## Day 12：消息历史验收

- `save_message` 保存 `system`、`user`、`assistant` 消息；数据库拒绝其他角色。
- `get_recent_messages` 先按 `(created_at DESC, id DESC)` 取最近 N 条，再翻转为正序。时间相同时以 UUID 稳定排序，不代表实际发送先后。
- 历史查询只返回指定会话的消息。`limit` 默认 50，数值限制在 1–200；例如 0 按 1 处理，999 按 200 处理。
- 已存在但没有消息的会话返回 `200 + []`；不存在的会话返回 `404`，错误代码为 `NOT_FOUND`。
- 非法 UUID 或非整数 `limit` 返回 `400`。Axum 0.8 路由参数使用 `/{id}`，不是 `/:id`。
- 删除会话会通过外键级联删除消息。

后端运行后，在另一个 PowerShell 终端中手动检查：

```powershell
$base = "http://127.0.0.1:3000"
$conversation = Invoke-RestMethod -Method Post -Uri "$base/api/conversations" `
    -ContentType "application/json" -Body '{"title":"Day 12 check"}'
Invoke-RestMethod -Uri "$base/api/conversations"
Invoke-RestMethod -Uri "$base/api/conversations/$($conversation.id)/messages?limit=3"
Invoke-RestMethod -Method Delete -Uri "$base/api/conversations/$($conversation.id)"
```

新建会话的历史是空数组。消息写入使用下文 Day 13 的会话聊天接口；原有 `POST /api/chat` 仍是独立问答，不保存会话历史。

## Day 13：多轮聊天持久化

`POST /api/conversations/{id}/chat` 的处理顺序：

1. 检查提问非空、会话存在。
2. 保存 `user` 消息，读取该会话最近 50 条历史，包含当前提问且不重复追加。
3. 按历史顺序将 `role` 和 `content` 发送给 LLM。
4. 保存 `assistant` 回复，返回该消息的 `id`、`conversation_id`、`role`、`content`、`token_count` 和 `created_at`。调用方读取 `content`，不是旧接口的 `reply`。

空白提问返回 400，不存在的会话返回 404，均不保存消息。LLM 调用失败返回 500，已保存的用户消息保留，不伪造助手回复；再次发送会另存一条用户消息，没有自动重试去重。串行发送同一会话的请求；当前没有并发轮次隔离。

配置同一提供商支持的 `LLM_API_BASE`、`LLM_MODEL` 和有效 `LLM_API_KEY`，重启后端后用 PowerShell 连续验证三轮：

```powershell
$base = "http://127.0.0.1:3000"
$conversation = Invoke-RestMethod -Method Post -Uri "$base/api/conversations" `
    -ContentType "application/json" -Body '{"title":"Day 13 check"}'
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

三轮使用同一个会话 ID；全部成功后，历史应有 6 条交替的用户和助手消息。真实模型第二、三轮应回答 `RUST-731`。如果回复含 `[Mock LLM ...]`，只能检查接口与存库，不能勾选真实模型记忆验收。

本地已通过受控 HTTP 提供商的三轮历史与落库验收，以及上下文 50 条上限、会话隔离、错误路径和旧单轮接口检查。这不是外部真实模型验收。临时会话数据已清理。

## 验证

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

测试需要本机 PostgreSQL 正常运行，且已执行迁移：`query_as!` 在编译时检查数据库 schema，数据库集成测试还需要创建临时数据库的权限。Compose 的本地开发账号满足该条件。

回归测试覆盖消息读写、最近 N 条及同时间戳排序、会话隔离、历史数量上下界、空历史、404、删除级联，以及聊天非法输入不写库、提供商失败时保留用户消息。SQLx 测试使用独立临时数据库，不写入开发会话。

GitHub Actions 会启动 PostgreSQL 并在编译前执行迁移 SQL；任何迁移 SQL 错误都会立即使步骤失败。本地验证通过不等同于远端 CI 已通过，推送后需检查对应运行结果。
