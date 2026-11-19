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
| POST | `/api/chat` | `{"prompt":"Hello Rust"}`（可选 `tier` 或 `model`） |
| POST | `/api/conversations` | `{"title":"first chat"}`；创建会话，返回 201 |
| GET | `/api/conversations` | 列出会话，按创建时间倒序 |
| DELETE | `/api/conversations/{id}` | 删除会话及其消息，返回 204；不存在返回 404 |
| GET | `/api/conversations/{id}/messages?limit=50` | 获取该会话最近 N 条消息，按时间正序返回 |
| POST | `/api/conversations/{id}/chat` | `{"prompt":"Hello Rust"}`（可选 `tier` 或 `model`）；多轮聊天，返回已保存的助手消息 |
| POST | `/api/conversations/{id}/chat/stream` | `{"prompt":"Hello Rust"}`（可选 `tier` 或 `model`）；SSE 多轮聊天，保存完整回复或带中断标记的部分回复 |
| GET | `/api/stream-demo` | SSE 模拟流，约每 200ms 推送一个单词，共 25 条事件 |

`LLM_API_BASE` 和 `LLM_MODEL` 应填写同一服务商支持的接口与模型。`LLM_API_KEY` 留空或为模板占位值时，普通聊天接口返回本地 Mock 回复；流式聊天必须配置有效密钥。可选配置 `LLM_REASONING_MODEL`（以及同主机可省略的 `LLM_REASONING_API_BASE` / `LLM_REASONING_API_KEY`）启用 `reasoning` 档位路由。配置保存在被 Git 忽略的 `.env` 中，不要提交真实密钥。

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

新建会话的历史是空数组。消息写入使用会话普通聊天接口 `POST /api/conversations/{id}/chat` 或流式聊天接口 `POST /api/conversations/{id}/chat/stream`；`POST /api/chat` 是独立问答，不保存会话历史。

## 多轮聊天持久化

`POST /api/conversations/{id}/chat` 的处理顺序：

1. 检查模型选择合法、提问非空、会话存在。
2. 保存 `user` 消息，读取该会话最近 50 条历史，包含当前提问且不重复追加。
3. 按历史顺序将 `role` 和 `content` 发送给 LLM。
4. 保存 `assistant` 回复，返回该消息的 `id`、`conversation_id`、`role`、`content`、`token_count` 和 `created_at`。调用方读取 `content`，不是旧接口的 `reply`。

空白提问或非法模型选择返回 400，不存在的会话返回 404，均不保存消息。LLM 调用失败返回 500，已保存的用户消息保留，不伪造助手回复；再次发送会另存一条用户消息，没有自动重试去重。串行发送同一会话的请求；当前没有并发轮次隔离。

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

## 会话流式聊天

`POST /api/conversations/{id}/chat/stream` 接收 `{"prompt":"..."}`（可选 `tier` 或 `model`），复用普通聊天的校验、用户消息保存和最近 50 条历史。响应为 `text/event-stream`，不支持 Mock。

| SSE 事件 | `data` 格式 | 含义 |
| --- | --- | --- |
| `delta` | JSON 字符串，如 `"你好\n"` | 一段回复文本；JSON 编码保留原始换行和 Unicode |
| `done` | `{"message_id":"UUID"}` | 上游收到 `[DONE]`，且完整助手回复已成功存库 |
| `error` | `{"error":{"code":"INTERNAL_ERROR","message":"Unable to complete streaming reply"}}` | 流读取、解析或持久化失败，随后关闭连接 |

后台任务使用容量为 16 的有界通道推送分片，同时拼接完整回复。正常结束后只保存一条 `assistant` 消息，再发送 `done`；不逐片写库。上游异常时保留用户消息，不保存半截助手回复。

建立 SSE 前的失败沿用普通 JSON 错误响应：空白提问或非法模型选择返回 400、会话不存在返回 404，均不写库；模型请求失败返回 500，已保存的用户消息保留。建立 SSE 后的失败通过 `error` 事件报告，不能再修改 HTTP 状态。客户端必须以 `done` 确认保存成功，不能把连接关闭当成成功。

后台读取通过 `tokio::select!` 同时等待上游分片和 `tx.closed()`；客户端断开时，即使上游没有新数据，也会停止读取。发送失败同样按中断处理。上游流在独立作用域中销毁，先释放当前上游响应流，再等待数据库保存，数据库繁忙不会延迟取消。

模型尚未完成时断开，会保存一条 `assistant` 消息：已生成文本后附独立一行 `[Interrupted]`；正文为空时仅保存该标记。保存的是服务端已生成的文本，可能包含尚未送达客户端的分片。中断后的保存失败会记录服务端错误，不伪造保存成功；不会向已经断开的客户端发送 `done`。

如果已读取上游 `[DONE]` 并进入最终保存阶段，随后断开不会把完整回复误标为中断，也不回滚已保存的消息。上游读取或解析失败仍沿用 `error` 行为，不将异常产生的半截回复保存为中断记录。

可在 `curl` 请求过程中按 Ctrl+C 断开，待后台保存后通过消息历史接口读取中断记录。中断记录与普通助手消息一样进入后续会话上下文。同一会话需串行发送请求，当前没有并发轮次隔离。

后端运行并配置真实模型后，在 PowerShell 中调用：

```powershell
$base = "http://127.0.0.1:3000"
$conversation = Invoke-RestMethod -Method Post -Uri "$base/api/conversations" `
    -ContentType "application/json" -Body '{"title":"stream-example"}'
$body = @{ prompt = "Briefly explain Rust streams." } | ConvertTo-Json -Compress
$body | curl.exe -N -H "Content-Type: application/json" --data-binary "@-" `
    "$base/api/conversations/$($conversation.id)/chat/stream"
Invoke-RestMethod -Uri "$base/api/conversations/$($conversation.id)/messages" |
    Format-Table id, role, content
```

## LLM 流式客户端

`ai_core::llm::LlmClient::chat_stream(&messages)` 向 OpenAI 兼容的 `/chat/completions` 接口发送 `stream: true`，返回 `Stream<Item = anyhow::Result<String>>`。结果流支持 `Send + 'static`，不借用调用方的客户端或消息。

客户端按 SSE 事件解析响应，逐段产出非空 `delta.content`，跳过角色和用量事件，以 `[DONE]` 正常结束。HTTP 错误、无效 JSON、读取失败或未收到 `[DONE]` 的中断均返回错误；流式请求要求配置真实密钥，不使用 Mock。

在根目录 `.env` 配置 `LLM_API_BASE`、`LLM_API_KEY` 和 `LLM_MODEL` 后运行：

```powershell
cargo run -p ai-core --example stream_client
```

示例逐段刷新标准输出，无需启动数据库或 HTTP 服务。客户端总请求超时为 60 秒；此 core 方法不直接访问数据库。

## 强类型结构化输出

`LlmClient::generate_structured<T>(&self, prompt: &str)` 返回 `anyhow::Result<T>`，要求 `T: schemars::JsonSchema + serde::de::DeserializeOwned`。

方法使用 `schemars` 生成 Schema，向兼容的 Chat Completions 接口发送 `response_format: {"type":"json_object"}` 和 Schema 提示。传输层通过 `{"result": ...}` 对象承载目标值，因此目标类型也可以是数组、标量或可空类型；返回值只包含解包后的 `T`。

JSON 模式只保证 JSON 格式，不代表符合 Schema。方法先用 `jsonschema` 在本地校验，再反序列化为 Rust 类型；启用已知 `format` 检查，未知 `format` 按验证器默认策略处理。Schema 使用本地 `$defs` 引用，验证器依赖关闭默认的 HTTP 和文件引用解析特性。

`ai_core::report::TechReport` 包含 `summary: String`、`pros: Vec<String>`、`cons: Vec<String>` 和 `score: u8`，拒绝未知字段。生成接口通过 Schema 将 `score` 限制在 0–100，而不是仅依赖 `u8` 的数值范围。

```rust
use ai_core::report::TechReport;

let report: TechReport = client.generate_structured("评估 Rust + Axum 构建聊天服务").await?;
```

配置有效的 `LLM_API_BASE`、`LLM_API_KEY` 和 `LLM_MODEL` 后，可运行独立示例：

```powershell
cargo run -p ai-core --example structured_client
cargo run -p ai-core --example structured_client -- "评估 PostgreSQL 用于聊天消息持久化"
```

示例将强类型结果打印为 JSON，不启动 HTTP 服务、不访问数据库，也不新增 HTTP 路由。空提问或缺少密钥会在请求前失败；此方法不使用 Mock。

HTTP 错误、空 choices、模型拒绝、非 `stop` 的结束原因、空白内容、非法 JSON、Schema 违规或类型解码失败均返回错误。不剥离 Markdown 代码围栏，不修补输出，不自动重试。Schema 和 Rust 类型只能约束数据形状，不能保证模型分析的事实正确性。

协议说明：[DeepSeek JSON Output](https://api-docs.deepseek.com/guides/json_mode)、[Schemars](https://docs.rs/schemars/)、[JSON Schema 验证器](https://docs.rs/jsonschema/)。

## 多模型 Provider 与路由

`ai_core::provider` 定义了对象安全的 `ModelProvider: Send + Sync` Trait（基于 `BoxFuture` 和 `TokenStream`，支持 `Arc<dyn ModelProvider>` 动态分发），统一普通对话、流式输出和结构化 JSON 能力，并通过扩展 Trait `ModelProviderExt::generate_structured<T>` 为所有 Provider 提供一致的本地 Schema 校验与强类型解码。

- `ModelTier` 支持 `fast`（默认，日常对话）和 `reasoning`（复杂推理/旗舰模型）；`ModelSelection` 支持 `Default`、`Tier(ModelTier)` 和显式 `Model(&str)`。
- `LlmClient` 实现 `ModelProvider`，可在单次请求中覆盖上游 `model` 字段，保留原有默认模型便捷方法不变。
- `ModelRouter` 同样实现 `ModelProvider`，按档位或已注册模型名将请求分发到不同后端 Provider，未配置的档位或未知模型会在请求上游与写库前明确报错。
- `AppState` 持有 `Arc<dyn ModelProvider>`，默认由 `ModelRouter` 构造：`fast` 绑定 `LLM_MODEL`，配置 `LLM_REASONING_MODEL` 后自动注册 `reasoning` 档位；同主机且未单独指定密钥时复用同一个 `LlmClient` 连接池，跨主机且未设置 `LLM_REASONING_API_KEY` 时不会复用 `LLM_API_KEY`。
- `POST /api/chat`、`POST /api/conversations/{id}/chat` 和 `POST /api/conversations/{id}/chat/stream` 均接受可选的 `tier`（`"fast"` / `"reasoning"`）或 `model` 字段；两者同时传入、传入空白模型、未知模型或未配置的档位均返回 `400 Bad Request`，不写库。

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

回归测试覆盖消息读写、最近 N 条及同时间戳排序、会话隔离、历史数量上下界、空历史、404、删除级联，以及聊天非法输入不写库、提供商失败时保留用户消息。流式测试验证完成前可收到分片、换行和 Unicode 保真、完整回复保存后才发送 `done`、上游异常不保存半截回复、持久化失败不报告成功。取消测试覆盖上游空闲时断开、正文为空时保存中断标记、数据库连接耗尽时先取消上游，以及上游完成后断开仍保留完整回复。SQLx 测试使用独立临时数据库，不写入开发会话。

core 的回归测试覆盖结构化输出（评分边界及越界、缺失字段、错误类型、未知字段、数组与可空类型、截断与拒绝、空或非法内容和 HTTP 失败）与多模型 Provider 路由（跨 Provider 档位与显式模型分发、`LlmClient` 动态 `model` 透传、非法/未配置选择提前拒绝）；server 的路由测试额外覆盖跨主机密钥隔离、同主机连接复用、多模型 `/api/chat` 与会话路由，以及非法模型选择返回 400 且不写库。

GitHub Actions 的构建和测试步骤均设置 `SQLX_OFFLINE=true`，编译 SQL 宏时使用提交的缓存。PostgreSQL 服务与迁移步骤仍保留，供运行数据库集成测试使用；任何迁移 SQL 错误都会立即使步骤失败。
