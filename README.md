# Project Rust

按每日实战计划构建 Rust + Next.js AI SaaS。当前已有 Axum 后端、统一错误处理、日志、中间件和非流式 LLM 客户端，PostgreSQL 开发环境（Day 8）已通过本机验收。下一步是会话与消息表的数据库迁移（Day 9）。

## 启动后端

在项目根目录运行。新检出项目时，先复制环境变量模板；已有 `.env` 时直接使用现有文件：

```powershell
if (-not (Test-Path .env)) { Copy-Item .env.example .env }
cargo run -p ai-server
```

默认地址为 `http://127.0.0.1:3000`，可通过 `.env` 中的 `PORT` 修改。

| 方法 | 路径 | 请求体 / 用途 |
| --- | --- | --- |
| GET | `/api/health` | 服务状态与版本 |
| POST | `/api/echo` | `{"message":"Hello Rust"}` |
| POST | `/api/chat` | `{"prompt":"Hello Rust"}` |

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
DATABASE_URL=postgres://ai_saas:ai_saas_dev@localhost:5432/ai_saas
```

目前后端尚未接入数据库连接池；该配置供 SQLx CLI 和后续 `crates/db` 使用。

安装与后续 SQLx 0.8 开发配套的 CLI：

```powershell
cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features rustls,postgres
sqlx --version
sqlx database create
```

Compose 首次初始化时会创建数据库；`sqlx database create` 确保 `DATABASE_URL` 指向的数据库存在。Day 9 再添加会话和消息表的迁移，并启用 vector 扩展。

常用维护命令：

```powershell
docker compose logs --tail 50 postgres
docker compose stop postgres
docker compose start postgres
```

正常停止容器会保留数据。账号和数据库名的初始化环境变量只在首次创建数据目录时生效。

## 验证

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

当前 3 个接口测试通过，Clippy 检查通过。2026-11-08 已在本机完成 Day 8 验收：

- PostgreSQL 容器状态为 `healthy`，端口为 `127.0.0.1:5432`。
- SQL 查询返回数据库名、用户名均为 `ai_saas`。
- `pg_available_extensions` 中可查询到 `vector`，当前版本为 `0.8.7`；尚未在数据库中启用，留到迁移中处理。
- SQLx CLI `0.8.6` 安装成功，使用项目 `.env` 执行 `sqlx database create` 成功。

`pg16` 是可更新的镜像标签，后续拉取时扩展版本可能变化。
