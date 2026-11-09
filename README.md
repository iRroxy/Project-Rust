# Project Rust

按每日实战计划构建 Rust + Next.js AI SaaS。当前已完成 Day 12：Axum 后端已接入 PostgreSQL 连接池和数据库迁移，支持会话创建、列表、删除，以及消息保存和历史读取。多轮聊天持久化属于 Day 13，尚未实现。

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

新建会话的历史是空数组。Day 12 尚无 HTTP 消息写入接口；`POST /api/chat` 仍是独立问答，不保存会话历史。多轮提问、历史传给 LLM、回复存库留在 Day 13。

## 验证

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

测试需要本机 PostgreSQL 正常运行，且已执行迁移：`query_as!` 在编译时检查数据库 schema，数据库集成测试还需要创建临时数据库的权限。Compose 的本地开发账号满足该条件。

回归测试覆盖消息读写、最近 N 条及同时间戳排序、会话隔离、历史数量上下界、空历史、404 和删除级联。SQLx 测试使用独立临时数据库，不写入开发会话。

GitHub Actions 会启动 PostgreSQL 并在编译前执行迁移 SQL；任何迁移 SQL 错误都会立即使步骤失败。本地验证通过不等同于远端 CI 已通过，推送后需检查对应运行结果。
