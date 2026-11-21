# Project Rust (AI Native SaaS Starter)

<div align="center">

![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange?logo=rust)
![Axum](https://img.shields.io/badge/Axum-0.8-blue?logo=rust)
![Next.js](https://img.shields.io/badge/Next.js-16%20(App%20Router)-black?logo=next.js)
![TailwindCSS](https://img.shields.io/badge/TailwindCSS-v4-38bdf8?logo=tailwindcss)
![PostgreSQL](https://img.shields.io/badge/PostgreSQL-16%20(pgvector)-336791?logo=postgresql)
![License](https://img.shields.io/badge/License-MIT-green)

**基于 Rust (Axum + Tokio + SQLx) 与 Next.js 16 (App Router + shadcn/ui) 的高并发、低延迟 AI 原生全栈 SaaS 脚手架。**

[English](#english) | [中文说明](#中文说明)

</div>

---

<a name="中文说明"></a>
## 架构总览 (Architecture)

```mermaid
flowchart TD
    subgraph Client ["前端层 (Next.js 16 / Web)"]
        UI["shadcn/ui 现代化工作台"]
        SSE_Reader["原生 ReadableStream 打字机"]
        AbortCtrl["AbortController 立即中止"]
        GenUI["Generative UI 结构化卡片"]
    end

    subgraph Gateway ["Rust 高性能网关 (Axum + Tokio)"]
        Router["Axum 0.8 路由 & CORS & 错误处理"]
        AppRouter["ModelRouter (多模型 Provider 路由)"]
        CancelGuard["Tokio select! 立即断开取消上游"]
    end

    subgraph LLM_Cloud ["模型服务商 (OpenAI / DeepSeek / 兼容协议)"]
        Fast["日常模型 (Fast: deepseek-chat)"]
        Reasoner["推理模型 (Reasoning: deepseek-reasoner)"]
    end

    subgraph Storage ["持久化层 (PostgreSQL 16)"]
        SQLx["SQLx 编译期类型检查连接池"]
        PG[("PostgreSQL + pgvector")]
    end

    UI -->|用户输入 / 模式切换| Router
    Router -->|动态模型分发| AppRouter
    AppRouter -->|SSE Token 流式长连接| Fast
    AppRouter -->|复杂推理 / 结构化| Reasoner
    Fast -.->|Token 分片推送| SSE_Reader
    SSE_Reader --> GenUI
    AbortCtrl -.->|客户端关闭 / 停止| CancelGuard
    CancelGuard -->|立即释放上游连接| Fast
    CancelGuard -->|带 [Interrupted] 标记安全存库| SQLx
    Router -->|会话 & 消息历史读写| SQLx
    SQLx --> PG
```

### 核心特性

1. **极致轻量与低内存占用**：
   - 生产环境空载工作集仅 **~15MB**，10 路并发 SSE 全程压测内存峰值仅 **~16.9MB**（增量仅 +1.13MB）。
2. **真正的双向取消流控（省 Token 神器）**：
   - 采用 `tokio::select!` 监听客户端连接断开，用户点击“停止生成”或关闭网页时，后端立刻主动断开与 LLM 提供商的长连接，并安全持久化已生成的分片与 `[Interrupted]` 标记。
3. **强类型 Generative UI（结构化输出）**：
   - 利用 `schemars` 自动生成 JSON Schema，通过本地本地二次严格校验并解包为强类型 `TechReport`，前端直接渲染带进度条与优缺点对比的可视化卡片。
4. **统一多模型路由体系（ModelProvider）**：
   - 原生支持 `fast`（日常快速模型）与 `reasoning`（旗舰推理模型）档位分发，支持同主机连接池复用与跨主机密钥隔离。
5. **生产级 Docker 全栈编排**：
   - Rust 多阶段二进制极小镜像 + Next.js Alpine 容器 + pgvector 数据库一键起步。

---

## 快速开始 (Quick Start)

### 1. 本地启动服务（Docker 数据库 + 本地后端与前端）

```powershell
# 1. 克隆代码
git clone https://github.com/iRroxy/Project-Rust.git
cd Project-Rust

# 2. 启动数据库并执行迁移
powershell -ExecutionPolicy Bypass -File .\scripts\start-db.ps1
sqlx migrate run

# 3. 运行 Rust 后端
cargo run -p ai-server

# 4. 在另一个终端启动 Next.js 前端
cd web
npm install
npm run dev
```
- 前端地址：`http://localhost:3000`
- 后端地址：`http://127.0.0.1:3000`

### 2. Docker Compose 全栈一键启动

```powershell
cp .env.production.example .env
docker compose up -d --build
```
访问 `http://localhost:8080` 即可开始流式会话！

---

## 核心 API 说明

| 方法 | 路径 | 功能说明 |
| :--- | :--- | :--- |
| **GET** | `/api/health` | 服务健康检查与当前引擎版本 |
| **GET** | `/api/models` | 查询可用模型与路由档位（Fast / Reasoning） |
| **POST** | `/api/conversations` | 创建新会话 |
| **GET** | `/api/conversations` | 会话列表分页查询 |
| **DELETE** | `/api/conversations/{id}` | 级联删除会话及历史消息 |
| **GET** | `/api/conversations/{id}/messages` | 获取指定会话的历史消息（正序） |
| **POST** | `/api/conversations/{id}/chat` | 多轮同步对话落库 |
| **POST** | `/api/conversations/{id}/chat/stream` | **核心 SSE Token 流式推送 + 即时取消 + 自动落库** |
| **POST** | `/api/analyze` | 强类型 JSON Schema 结构化深度分析 |

---

<a name="english"></a>
## English Overview

**Project Rust (AI Native SaaS Starter)** is a production-grade, highly concurrent, and low-latency full-stack template crafted with **Rust (Axum + Tokio + SQLx)** and **Next.js 16 (App Router + Tailwind CSS + shadcn/ui)**.

### Highlights
- **Sub-20MB Memory Footprint**: Idles at ~15MB RSS; peaks at only ~16.9MB under 10 concurrent real-time token streams.
- **Instant Upstream Cancellation**: Real-time client disconnect detection with `tokio::select!` drops upstream LLM connections in milliseconds to avoid unnecessary token billing.
- **Type-safe Generative UI**: Schema-guided structured JSON output validated at runtime and seamlessly rendered as rich cards.
- **Extensible Model Routing**: Dynamic abstraction over fast & reasoning tiers with connection pooling and credential isolation.
- **Production-Ready Docker Setup**: Automated multi-stage builds for minimal distroless/slim images.

For detailed deployment instructions, please refer to [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md).
