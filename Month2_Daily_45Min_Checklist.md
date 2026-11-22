# 第 2 个月（Day 31 – Day 60）每日 45 分钟实战打卡表
## 目标：发挥 Rust 极致性能，构建极速文档/代码 AST 解析管道与生产级混合 RAG 检索内核

> **使用说明（每天 <1 小时铁律）**
> 1. **时间分配**：每次打开电脑只专注 **40~45 分钟**。前 10 分钟快速浏览当日核心概念/文档，中间 30 分钟动手写代码跑通，最后 5 分钟提交 `git commit` 并在代码里写下一行的 `// TODO(Tomorrow): ...`。
> 2. **完成标志**：每完成一天，将 `[ ]` 改为 `[x]`。如果某天加班没空，第二天直接顺延，**绝不因为断更一天而焦虑放弃**。
> 3. **本月最终交付物**：一个具备 **Rust 高性能 AST 代码/文档解析 + 原生分词语义切块 + pgvector HNSW 向量检索 + BM25 混合搜索 + RRF 重排 + Next.js 前端流式精准溯源引用** 的完整 RAG SaaS 引擎（Tag: `v0.2.0`）。

---

## 第五周（Day 31 – Day 37）：Rust 极速文档与代码 AST 解析管道

**本周目标**：脱离慢速笨重的 Python 文档解析工具，用纯 Rust 构建高并发、低内存消耗的文档与多语言代码 AST 符号解析器。

- [x] **Day 31：初始化 `crates/parser` 模块与 `pulldown-cmark` 流式 Markdown 结构化解析**
  - **10 min 速览**：学习 `pulldown-cmark` 的基于事件迭代器（Event-based Pull Parser）的零拷贝解析原理。
  - **30 min 实战**：
    1. 在 Workspace 中新建 `crates/parser`（`ai-parser`），并在根 `Cargo.toml` 中注册。
    2. 引入 `pulldown-cmark`，实现 `MarkdownParser`：从 Markdown 流中精确提取标题层级（H1-H6）、正文段落、代码块语言与内容。
    3. 编写单元测试验证带复杂格式的 Markdown 文档解析准确性。
  - **验收 Commit**：`git commit -m "feat(parser): init ai-parser crate and implement streaming markdown parser"`

- [x] **Day 32：集成 `tree-sitter` 实现代码 AST 语法树解析与符号提取**
  - **10 min 速览**：理解 `tree-sitter` 增量语法解析和 S-expression 查询（Queries）语法。
  - **30 min 实战**：
    1. 引入 `tree-sitter` 及语言解析器（Rust、TypeScript、Python）。
    2. 实现 `CodeParser`：解析源码文件，提取 `functions`、`structs/classes`、`traits/interfaces` 及其 docstring 注释与行号范围。
    3. 编写针对 Rust 和 TypeScript 代码文件的解析单元测试。
  - **验收 Commit**：`git commit -m "feat(parser): add tree-sitter AST symbol extractor for code files"`

- [x] **Day 33：利用 `rayon` 与 `tokio::task::JoinSet` 实现大目录文件并发扫描与解析**
  - **10 min 速览**：理解 CPU 密集型任务（AST 解析）与 I/O 任务（文件读取）在 Tokio 与 Rayon 之间的线程池划分。
  - **30 min 实战**：
    1. 引入 `rayon` 与 `ignore`（或 `walkdir`，支持 `.gitignore` 过滤）。
    2. 实现 `RepositoryScanner`：多线程扫描代码目录，并发解析所有支持的文档与代码文件，输出统一的 `ParsedDocument` 集合。
  - **验收 Commit**：`git commit -m "feat(parser): implement multi-threaded repository scanner with rayon"`

- [x] **Day 34：引入 `blake3` 极速哈希计算，实现文档指纹与零冗余增量变更比对**
  - **10 min 速览**：了解 Blake3 算法（SIMD 优化、树状哈希，比 SHA-256 快数十倍）的高速哈希特性。
  - **30 min 实战**：
    1. 引入 `blake3` crate。
    2. 为 `ParsedDocument` 生成全文及单个段落/符号的 Blake3 校验哈希（Fingerprint）。
    3. 实现 `DiffCalculator`：通过哈希集合比对，精准找出新增（Added）、修改（Modified）、未变更（Unchanged）的文件与段落。
  - **验收 Commit**：`git commit -m "feat(parser): add blake3 document fingerprinting and incremental diffing"`

- [x] **Day 35：在 `crates/server` 中开辟文档摄入与结构化解析接口 (`POST /api/documents/parse`)**
  - **10 min 速览**：学习 Axum `multipart/form-data` 大文件分块上传或纯文本 JSON 批量提交。
  - **30 min 实战**：
    1. 在 `crates/server` 引入 `ai-parser` 依赖。
    2. 编写 `POST /api/documents/parse` 接口，接收文件或文本内容，调用 `ai-parser` 返回结构化文档元数据与符号清单。
    3. 编写 API 接口集成测试。
  - **验收 Commit**：`git commit -m "feat(server): add document parse endpoint with structured response"`

- [x] **Day 36：前端 Next.js 接入文档拖拽上传与 AST 符号树 / 结构化目录可视化预览**
  - **10 min 速览**：学习 HTML5 拖拽上传与 Lucide 图标符号树展示。
  - **30 min 实战**：
    1. 在前端新建文档解析页面 `/documents`。
    2. 实现文件拖拽上传区，调用 `POST /api/documents/parse`。
    3. 渲染解析出的 Markdown 标题大纲树与代码函数/结构体符号列表。
  - **验收 Commit**：`git commit -m "feat(web): add document upload and parsed AST outline preview"`

- [x] **Day 37：第五周集成基准测试：压测 Rust 并发 AST 解析器吞吐性能**
  - **45 min 实战**：
    1. 编写压测脚本（或集成测试），并行解析 500 个代码与 Markdown 文件。
    2. 记录吞吐量（MB/s）、CPU 占用与峰值内存。
    3. 验证内存占用始终保持在低水位（<35MB）。
  - **验收 Commit**：`git commit -m "test(parser): add multi-file throughput benchmark and memory check"`

---

## 第六周（Day 38 – Day 44）：Rust 原生 Token 分词与语义/代码智能切分引擎

**本周目标**：引入 HuggingFace 原生分词器，告别粗暴的按字符截断，构建感知标题层级与代码语法结构的语义切块器（Semantic Chunker）。

- [x] **Day 38：集成 HuggingFace `tokenizers` Rust 官方库与 BPE 分词**
  - **10 min 速览**：了解 Byte-Pair Encoding (BPE) 原理与主流 LLM（OpenAI cl100k_base / o200k_base / Llama）分词模型。
  - **30 min 实战**：
    1. 引入 `tokenizers` crate（或者 `tiktoken-rs` 轻量预编译包）。
    2. 封装 `TokenCounter` 工具，实现极速精确的 Token 计数与按 Token 限制安全截断。
    3. 编写针对多语言混合（中英符号）文本的 Token 计数测试。
  - **验收 Commit**：`git commit -m "feat(parser): integrate tokenizers for precise BPE token counting"`

- [x] **Day 39：编写基于层级标题边界的 Markdown 语义切块器 (`SemanticChunker`)**
  - **10 min 速览**：阅读 RAG 切块最佳实践：基于标题面包屑（Breadcrumbs）保留父级上下文。
  - **30 min 实战**：
    1. 实现 `SemanticChunker`：以 Markdown H1-H6 为逻辑段落边界，在设定的目标 Token 窗口（如 512 tokens）与重叠度（如 64 tokens）内智能合并。
    2. 每个 Chunk 自动保留标题链路前缀（如 `[Doc > Section 2 > Step 1]`）。
    3. 编写切块测试用例验证无边界撕裂。
  - **验收 Commit**：`git commit -m "feat(parser): implement hierarchical markdown semantic chunker"`

- [x] **Day 40：编写代码感知切块器 (`CodeChunker`)：基于 AST 保持函数完整性**
  - **10 min 速览**：了解为什么代码 RAG 不能跨函数随意截断（破坏语法上下文导致大模型理解失真）。
  - **30 min 实战**：
    1. 结合 Day 32 的 `tree-sitter` 符号边界，实现 `CodeChunker`。
    2. 确保完整函数（Function）或结构体（Struct）作为最小独立切块，并附加所在文件路径与模块声明。
  - **验收 Commit**：`git commit -m "feat(parser): implement AST-aware code chunker for functions and structs"`

- [ ] **Day 41：切块元数据模型与强类型 Schema 定义**
  - **10 min 速览**：设计 RAG 检索可追溯所需的元数据字段（File ID、Breadcrumbs、Byte Offsets、Token Count、Hash）。
  - **30 min 实战**：
    1. 定义 `pub struct DocumentChunk` 强类型结构，集成 `serde` 与 `schemars`。
    2. 生成切块 JSON Schema 并支持反序列化检验。
  - **验收 Commit**：`git commit -m "feat(parser): define DocumentChunk schema with metadata and offsets"`

- [ ] **Day 42：构建异步背压切块流水线（`tokio::sync::mpsc` 异步通道）**
  - **10 min 速览**：掌握通过有界通道（Bounded Channel）实现流式切块与背压（Backpressure），防止超大文件爆内存。
  - **30 min 实战**：
    1. 使用 `tokio::sync::mpsc::channel(128)` 搭建管道。
    2. 生产者流式解析并产出 Chunks，消费者批量接收，内存占用与文件体积解耦。
  - **验收 Commit**：`git commit -m "feat(parser): build asynchronous bounded chunking pipeline with mpsc"`

- [ ] **Day 43：前端 Next.js 构建“切块探针 (Chunk Inspector)”调试视图**
  - **10 min 速览**：设计可视化切块高亮组件，直观查看切分效果。
  - **30 min 实战**：
    1. 前端增加“分块预览”卡片，展示每个 Chunk 的 Token 占用、标题面包屑与正文。
    2. 支持动态调节 Chunk Size / Overlap 滑块并即时查看切块结果。
  - **验收 Commit**：`git commit -m "feat(web): add interactive chunk inspector and size tuning UI"`

- [ ] **Day 44：第六周分块引擎内存与吞吐基准压测**
  - **45 min 实战**：
    1. 编写对 10MB+ 长文档和万行源码切块的性能测试。
    2. 验证多线程切块吞吐量并确认内存开销始终在 20MB 以内。
  - **验收 Commit**：`git commit -m "test(parser): benchmark chunking pipeline throughput and memory footprint"`

---

## 第七周（Day 45 – Day 51）：pgvector 向量底座与高并发 Embedding 管道

**本周目标**：在 PostgreSQL 中部署 `pgvector` 扩展，编写纯 Rust 异步并发向量化管道，建立高吞吐向量入库与毫秒级余弦检索。

- [ ] **Day 45：数据库启用 `pgvector` 扩展并编写 SQLx 迁移脚本**
  - **10 min 速览**：了解 `pgvector` 的 `vector` 数据类型与 HNSW (Hierarchical Navigable Small World) 图索引原理。
  - **30 min 实战**：
    1. 更新 `docker-compose.yml` 使用 `pgvector/pgvector:pg17` 镜像。
    2. 在 `crates/db/migrations` 创建新迁移：启用 `CREATE EXTENSION IF NOT EXISTS vector;`。
    3. 创建 `document_chunks` 表，包含 `id`, `document_id`, `content`, `metadata`, `embedding vector(1536)` 及 HNSW 索引。
  - **验收 Commit**：`git commit -m "feat(db): add pgvector extension and document_chunks table migration"`

- [ ] **Day 46：在 `crates/core` 抽象 `EmbeddingProvider` Trait**
  - **10 min 速览**：理解多嵌入模型路由架构（兼容 OpenAI `text-embedding-3-small` 与本地 Ollama `nomic-embed-text`）。
  - **30 min 实战**：
    1. 定义 `pub trait EmbeddingProvider: Send + Sync`，声明 `async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, AppError>`。
    2. 实现 `OpenAiEmbeddingProvider` 与 `OllamaEmbeddingProvider`。
  - **验收 Commit**：`git commit -m "feat(core): abstract EmbeddingProvider trait with OpenAI and Ollama implementations"`

- [ ] **Day 47：基于 `tokio::sync::Semaphore` 编写严格限流的批量 Embedding 管道**
  - **10 min 速览**：学习使用信号量限制上游并发，防止触发模型 API 的 Rate Limit (429 Too Many Requests)。
  - **30 min 实战**：
    1. 编写 `BatchEmbeddingPipeline`，按最大批量（如每次 16 条）打包 Chunks。
    2. 用 `Arc<Semaphore>` 限制最大并发并发请求数（如 5 并发），内建指数退避重试（Exponential Backoff）。
  - **验收 Commit**：`git commit -m "feat(core): implement rate-limited batch embedding pipeline with semaphore"`

- [ ] **Day 48：编写 `crates/db` 批量向量 Upsert 仓储层方法**
  - **10 min 速览**：了解 SQLx 中使用 UNNEST 或批量 VALUES 高效插入向量数据。
  - **30 min 实战**：
    1. 在 `crates/db` 中编写 `ChunkRepository`。
    2. 实现批量入库 `batch_insert_chunks(pool, chunks)`，并支持基于 chunk_hash 的冲突跳过或更新（ON CONFLICT DO UPDATE）。
  - **验收 Commit**：`git commit -m "feat(db): implement ChunkRepository for bulk vector upsert"`

- [ ] **Day 49：编写 Top-K 向量余弦距离搜索方法与相似度过滤**
  - **10 min 速览**：学习 pgvector 的余弦距离操作符 `<=>` 与内积 `<#>`，以及距离到相似度（$1 - \text{distance}$）的转换。
  - **30 min 实战**：
    1. 在 `ChunkRepository` 中编写 `search_similar_chunks(pool, query_vector, limit, min_similarity)`。
    2. 编写单元/集成测试：插入测试向量并验证余弦近邻排序结果正确。
  - **验收 Commit**：`git commit -m "feat(db): implement vector cosine similarity search with score threshold"`

- [ ] **Day 50：在 `crates/server` 中开辟 `POST /api/search/vector` 语义检索端点**
  - **10 min 速览**：将自然语言 Query 向量化后直达数据库检索。
  - **30 min 实战**：
    1. 编写 `POST /api/search/vector` Handler：接收 `{"query": "...", "top_k": 5}`。
    2. 后端先调用 `EmbeddingProvider` 生成 query 向量，再调用 `ChunkRepository` 检索相似段落并返回 JSON。
  - **验收 Commit**：`git commit -m "feat(server): add vector semantic search API endpoint"`

- [ ] **Day 51：第七周全流程集成测试：验证「解析 $\rightarrow$ 切块 $\rightarrow$ 嵌入 $\rightarrow$ 向量检索」闭环**
  - **45 min 实战**：
    1. 编写完整集成测试：上传一段示例文档，自动切块并生成向量入库。
    2. 输入提问，验证返回的相似段落精确命中文档核心事实。
  - **验收 Commit**：`git commit -m "test(integration): verify end-to-end ingest-embed-search workflow"`

---

## 第八周（Day 52 – Day 60）：混合检索 (BM25 + 向量)、RRF 重排与精准溯源引用

**本周目标**：融合关键词全文检索与语义向量检索（双路召回），用 Rust 实现 RRF 融合重排算法，并在前后端打通带段落精准高亮的流式引用溯源。

- [ ] **Day 52：在 PostgreSQL 中配置全文检索 (`tsvector` + GIN 索引)**
  - **10 min 速览**：了解全文检索 BM25 变体与语义向量检索的互补性（专有名词、错误码、精准符号靠全文检索，泛语义靠向量）。
  - **30 min 实战**：
    1. 数据库迁移为 `document_chunks` 表增加 `tsv_content tsvector GENERATED ALWAYS AS (to_tsvector('simple', content)) STORED;`。
    2. 为 `tsv_content` 建立 GIN 倒排索引。
    3. 在 `ChunkRepository` 中实现 `search_keyword_chunks(pool, query_text, limit)`。
  - **验收 Commit**：`git commit -m "feat(db): add full-text tsvector and GIN index for BM25 keyword search"`

- [ ] **Day 53：利用 `tokio::join!` 实现向量与关键词双路并发召回**
  - **10 min 速览**：掌握 Tokio 多 Future 并发执行机制，两路检索耗时取决于最慢的一路而非串行累加。
  - **30 min 实战**：
    1. 编写 `HybridSearchService`。
    2. 使用 `tokio::join!(search_vector, search_keyword)` 零等待并发获得双路候选集。
  - **验收 Commit**：`git commit -m "feat(core): implement dual-channel concurrent search with tokio::join"`

- [ ] **Day 54：纯 Rust 实现倒数排名融合算法（Reciprocal Rank Fusion, RRF）**
  - **10 min 速览**：学习工业级 RAG 标配融合算法 RRF：$RRF(d) = \sum_{m \in M} \frac{1}{k + r_m(d)}$（通常 $k=60$）。
  - **30 min 实战**：
    1. 在 `crates/core` 中实现纯 Rust 的 `rrf_merge(vector_results, keyword_results, k, top_n)`。
    2. 针对分值无须做脆弱的归一化，依据纯排名进行稳定融合重排。
    3. 编写严密的属性测试与单元测试。
  - **验收 Commit**：`git commit -m "feat(core): implement pure rust Reciprocal Rank Fusion (RRF) reranker"`

- [ ] **Day 55：RAG 上下文动态组装与 Token 预算感知提示词注入**
  - **10 min 速览**：学习上下文组装防爆 Token 策略（按 RRF 排序优先填充最重要 Chunk，超出预算自动截断）。
  - **30 min 实战**：
    1. 实现 `ContextAssembler`：根据 Top-N Chunks 格式化为带有引用标签 `[Doc N: Title]` 的上下文提示词。
    2. 结合 Day 38 的 Token 计数器，严格保证注入上下文不超出当前模型的 Context Window 上限。
  - **验收 Commit**：`git commit -m "feat(core): build token-budget aware RAG prompt context assembler"`

- [ ] **Day 56：扩展 SSE 流式协议输出带引用标记的回答**
  - **10 min 速览**：设计流式传输中的引用结构体与前端解析协议。
  - **30 min 实战**：
    1. 修改 SSE 协议：在首包或者尾包中附带本次回答引用的 `citations: Vec<CitationMeta>` 结构。
    2. 大模型回答正文中生成的 `[^1]`、`[^2]` 能与引用元数据精准对应。
  - **验收 Commit**：`git commit -m "feat(server): stream citations metadata via SSE protocol"`

- [ ] **Day 57：前端 Next.js 渲染引用角标与原文溯源侧边抽屉**
  - **10 min 速览**：学习交互式 Citation Pill 与侧边抽屉（Sheet/Drawer）高亮设计。
  - **30 min 实战**：
    1. 前端解析回答中的引用标签，渲染为可点击的数字角标。
    2. 点击角标在右侧滑出抽屉，显示被引用 Chunk 的原始文本、文件路径、代码行号与相似度得分。
  - **验收 Commit**：`git commit -m "feat(web): add interactive citation badges and source preview drawer"`

- [ ] **Day 58：基于 Blake3 哈希的文档增量同步与失效 Chunk 自动清理 (GC)**
  - **10 min 速览**：理解生产级知识库防脏数据机制（当文件更新时自动下线旧 Chunk，保持向量库纯净）。
  - **30 min 实战**：
    1. 编写垃圾回收逻辑 `clean_stale_chunks(pool, document_id, valid_hashes)`。
    2. 验证更新文件后，旧向量被彻底清除，避免重复和过时答案。
  - **验收 Commit**：`git commit -m "feat(db): implement incremental sync and stale chunk garbage collection"`

- [ ] **Day 59：长文档与大型代码库端到端 RAG 延迟与内存全链路压测**
  - **45 min 实战**：
    1. 准备真实的开源项目代码库（如 100 个文件），进行全流程摄入、切块、向量化与并发问答。
    2. 测量端到端首字延迟（TTFT）、向量检索延迟、RRF 排序耗时与服务内存曲线。
  - **验收 Commit**：`git commit -m "test(benchmark): full-pipeline RAG throughput, latency and memory profiling"`

- [ ] **Day 60（满双月里程碑）：完善中英文架构文档，发布 Git Tag `v0.2.0`**
  - **45 min 实战**：
    1. 更新根目录 `README.md` 与架构图，详细补充 Rust 高性能 RAG 引擎的设计与 benchmark 表现。
    2. 进行全库编译检查与代码格式化：`cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all`。
    3. 打上第二个正式 Git Tag：`git tag v0.2.0` 并 push 到远程仓库。
  - **验收 Commit**：`git commit -m "docs: complete v0.2.0 RAG engine milestone documentation and release"`
