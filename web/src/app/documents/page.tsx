"use client";

import React, { useState, useRef } from "react";
import Link from "next/link";
import {
  ArrowLeft,
  Upload,
  FileCode2,
  FileText,
  Fingerprint,
  Clock,
  ChevronRight,
  ChevronDown,
  AlertCircle,
  Trash2,
  Braces,
  Hash,
  Layers,
  Scissors,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { ChunkInspector } from "@/components/ChunkInspector";
import { api } from "@/lib/api";
import type {
  CodeSymbol,
  DocumentFingerprint,
  ParsedDocument,
  SymbolKind,
} from "@/types/api";

interface ParsedEntry {
  id: string;
  rawContent: string;
  document: ParsedDocument;
  fingerprint: DocumentFingerprint;
  parseTimeMs: number;
}

type InspectorTab = "outline" | "chunks";

const SAMPLE_RUST = {
  filename: "crates/engine/src/retriever.rs",
  content: `use std::collections::HashMap;

/// Configuration for hybrid vector + BM25 search.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub top_k: usize,
    pub rrf_k: f32,
}

/// Core retrieval interface implemented by storage backends.
pub trait Retriever {
    fn search(&self, query: &str) -> Vec<String>;
}

impl Retriever for SearchConfig {
    /// Execute a hybrid search pass over indexed chunks.
    fn search(&self, query: &str) -> Vec<String> {
        vec![format!("hit:{query}")]
    }
}

/// Fuse two ranked lists via Reciprocal Rank Fusion.
pub fn rrf_score(rank: usize, k: f32) -> f32 {
    1.0 / (k + rank as f32)
}

/// Merge vector hits and BM25 keyword hits using RRF weights.
pub fn merge_ranked_lists(vector_ids: &[String], bm25_ids: &[String], k: f32) -> Vec<(String, f32)> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    for (idx, id) in vector_ids.iter().enumerate() {
        *scores.entry(id.clone()).or_insert(0.0) += rrf_score(idx + 1, k);
    }
    for (idx, id) in bm25_ids.iter().enumerate() {
        *scores.entry(id.clone()).or_insert(0.0) += rrf_score(idx + 1, k);
    }
    let mut ranked: Vec<(String, f32)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked
}
`,
};

const SAMPLE_MARKDOWN = {
  filename: "docs/rag_architecture.md",
  content: `# Rust Hybrid RAG Engine

A low-memory, high-throughput retrieval pipeline written in pure Rust for developer knowledge bases.

## 1. Ingestion Pipeline
Files are discovered with \`ignore\` (respecting \`.gitignore\`) and parsed in parallel via \`rayon\`, keeping peak working set below 15 MB.

### 1.1 AST Symbol Extraction
Source files (.rs, .ts, .tsx, .py) are parsed with \`tree-sitter\` so functions and structs are never split mid-body.

\`\`\`rust
let report = RepositoryScanner::scan_directory("./crates")?;
println!("Parsed {} files in {}ms", report.documents.len(), report.elapsed_ms);
\`\`\`

### 1.2 Bounded Backpressure Streaming
Large repositories stream chunks through a bounded \`tokio::sync::mpsc::channel(128)\` so memory stays O(1) regardless of total corpus size.

## 2. Incremental Indexing
Every document and structural unit is hashed with \`blake3\` to skip unchanged embeddings and garbage-collect stale chunks automatically.

## 3. Hybrid Search & RRF Reranking
Dense cosine similarity from \`pgvector\` HNSW is executed concurrently with PostgreSQL \`tsvector\` BM25 keyword recall using \`tokio::join!\`, then fused with Reciprocal Rank Fusion.
`,
};

const SYMBOL_STYLES: Record<SymbolKind, string> = {
  function: "bg-blue-500/10 text-blue-600 border-blue-500/30 dark:text-blue-400",
  method: "bg-sky-500/10 text-sky-600 border-sky-500/30 dark:text-sky-400",
  struct: "bg-amber-500/10 text-amber-600 border-amber-500/30 dark:text-amber-400",
  enum: "bg-orange-500/10 text-orange-600 border-orange-500/30 dark:text-orange-400",
  trait: "bg-purple-500/10 text-purple-600 border-purple-500/30 dark:text-purple-400",
  impl: "bg-slate-500/10 text-slate-600 border-slate-500/30 dark:text-slate-400",
  class: "bg-emerald-500/10 text-emerald-600 border-emerald-500/30 dark:text-emerald-400",
  interface: "bg-teal-500/10 text-teal-600 border-teal-500/30 dark:text-teal-400",
  type_alias: "bg-pink-500/10 text-pink-600 border-pink-500/30 dark:text-pink-400",
};

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

export default function DocumentsPage() {
  const [entries, setEntries] = useState<ParsedEntry[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<InspectorTab>("chunks");
  const [expandedSymbol, setExpandedSymbol] = useState<string | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [isParsing, setIsParsing] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [batchStats, setBatchStats] = useState<{ count: number; elapsedMs: number } | null>(null);

  const fileInputRef = useRef<HTMLInputElement>(null);
  const selectedEntry = entries.find((e) => e.id === selectedId) ?? entries[0] ?? null;

  const parseFilesPayload = async (files: Array<{ filename: string; content: string }>) => {
    if (files.length === 0) return;
    setIsParsing(true);
    setErrorMsg(null);

    try {
      if (files.length === 1) {
        const res = await api.parseDocument(files[0].filename, files[0].content);
        const newEntry: ParsedEntry = {
          id: `${res.document.path}-${Date.now()}`,
          rawContent: files[0].content,
          document: res.document,
          fingerprint: res.fingerprint,
          parseTimeMs: res.parse_time_ms,
        };
        setEntries((prev) => [newEntry, ...prev]);
        setSelectedId(newEntry.id);
        setBatchStats({ count: 1, elapsedMs: res.parse_time_ms });
      } else {
        const res = await api.parseDocumentsBatch(files);
        const contentMap = new Map(files.map((f) => [f.filename.replace(/\\/g, "/"), f.content]));
        const now = Date.now();
        const perFileAvg = res.parsed.length > 0 ? res.elapsed_ms / res.parsed.length : 0;
        const newEntries: ParsedEntry[] = res.parsed.map((item, idx) => ({
          id: `${item.document.path}-${now}-${idx}`,
          rawContent: contentMap.get(item.document.path) ?? "",
          document: item.document,
          fingerprint: item.fingerprint,
          parseTimeMs: perFileAvg,
        }));
        if (newEntries.length > 0) {
          setEntries((prev) => [...newEntries, ...prev]);
          setSelectedId(newEntries[0].id);
        }
        setBatchStats({ count: res.parsed.length, elapsedMs: res.elapsed_ms });
        if (res.failed.length > 0) {
          setErrorMsg(
            `部分文件跳过 (${res.failed.length}): ` +
              res.failed.map((f) => `${f.filename} (${f.error})`).join("; ")
          );
        }
      }
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(`解析失败: ${msg}`);
    } finally {
      setIsParsing(false);
    }
  };

  const handleFileList = async (fileList: FileList | null) => {
    if (!fileList || fileList.length === 0) return;
    const files: Array<{ filename: string; content: string }> = [];
    for (const file of Array.from(fileList)) {
      const content = await file.text();
      files.push({ filename: file.name, content });
    }
    await parseFilesPayload(files);
  };

  const handleDrop = async (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setIsDragging(false);
    await handleFileList(e.dataTransfer.files);
  };

  const handleRemoveEntry = (id: string) => {
    setEntries((prev) => {
      const next = prev.filter((item) => item.id !== id);
      if (selectedId === id) {
        setSelectedId(next[0]?.id ?? null);
      }
      return next;
    });
  };

  return (
    <div className="flex h-screen w-full flex-col overflow-hidden bg-background text-foreground">
      {/* Top Header */}
      <header className="flex h-13 shrink-0 items-center justify-between border-b px-4">
        <div className="flex items-center gap-3">
          <Link href="/">
            <Button size="sm" variant="ghost" className="h-8 gap-1.5 px-2 text-xs">
              <ArrowLeft className="h-3.5 w-3.5" />
              <span>返回对话</span>
            </Button>
          </Link>
          <Separator orientation="vertical" className="h-5" />
          <div className="flex items-center gap-2">
            <Braces className="h-4 w-4 text-primary" />
            <h1 className="text-sm font-semibold">文档解析与切块探针工作台</h1>
          </div>
          <Badge variant="secondary" className="hidden font-mono text-[10px] sm:inline-flex">
            tree-sitter + BPE + SemanticChunker
          </Badge>
        </div>

        <div className="flex items-center gap-2">
          {batchStats && (
            <Badge variant="outline" className="gap-1 font-mono text-[11px] tabular-nums">
              <Clock className="h-3 w-3 text-emerald-500" />
              <span>
                最近解析 {batchStats.count} 个文件 · {batchStats.elapsedMs.toFixed(2)} ms
              </span>
            </Badge>
          )}
          <Button
            size="sm"
            variant="outline"
            className="h-8 text-xs"
            disabled={isParsing}
            onClick={() => void parseFilesPayload([SAMPLE_RUST])}
          >
            加载 Rust 示例
          </Button>
          <Button
            size="sm"
            variant="outline"
            className="h-8 text-xs"
            disabled={isParsing}
            onClick={() => void parseFilesPayload([SAMPLE_MARKDOWN])}
          >
            加载 Markdown 示例
          </Button>
        </div>
      </header>

      {/* Main Split Content */}
      <div className="flex flex-1 overflow-hidden">
        {/* Left Column: Dropzone & File List */}
        <aside className="flex w-80 shrink-0 flex-col border-r bg-muted/20">
          <div className="p-3">
            <div
              onDragOver={(e) => {
                e.preventDefault();
                setIsDragging(true);
              }}
              onDragLeave={() => setIsDragging(false)}
              onDrop={handleDrop}
              onClick={() => fileInputRef.current?.click()}
              className={`flex cursor-pointer flex-col items-center justify-center rounded-lg border-2 border-dashed p-5 text-center transition-colors ${
                isDragging
                  ? "border-primary bg-primary/5"
                  : "border-muted-foreground/25 hover:border-primary/50 hover:bg-muted/40"
              }`}
            >
              <Upload className="mb-2 h-6 w-6 text-muted-foreground" />
              <p className="text-xs font-medium">
                {isParsing ? "Rust 引擎解析中..." : "拖拽文件到此处，或点击上传"}
              </p>
              <p className="mt-1 font-mono text-[10px] text-muted-foreground">
                支持 .md .mdx .txt .rs .ts .tsx .py（支持多文件并发）
              </p>
              <input
                ref={fileInputRef}
                type="file"
                multiple
                accept=".md,.mdx,.markdown,.txt,.rs,.ts,.tsx,.py"
                className="hidden"
                onChange={(e) => {
                  void handleFileList(e.target.files);
                  e.target.value = "";
                }}
              />
            </div>

            {errorMsg && (
              <div className="mt-2.5 flex items-start gap-2 rounded-md border border-destructive/30 bg-destructive/10 p-2.5 text-[11px] text-destructive">
                <AlertCircle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span className="break-all">{errorMsg}</span>
              </div>
            )}
          </div>

          <Separator />

          <div className="flex items-center justify-between px-3 py-2 text-[11px] font-medium text-muted-foreground">
            <span>已解析文档 ({entries.length})</span>
            {entries.length > 0 && (
              <button
                type="button"
                onClick={() => {
                  setEntries([]);
                  setSelectedId(null);
                }}
                className="text-xs text-muted-foreground hover:text-destructive"
              >
                清空
              </button>
            )}
          </div>

          <ScrollArea className="flex-1 px-2 pb-3">
            {entries.length === 0 ? (
              <div className="px-3 py-8 text-center text-xs text-muted-foreground">
                暂无解析记录。上传本地代码/文档，或点击右上角加载示例体验 Rust 极速 AST 提取与实时切块探针。
              </div>
            ) : (
              <div className="space-y-1.5">
                {entries.map((entry) => {
                  const isActive = selectedEntry?.id === entry.id;
                  const isCode = entry.document.content.kind === "code";
                  return (
                    <div
                      key={entry.id}
                      onClick={() => setSelectedId(entry.id)}
                      className={`group flex cursor-pointer items-start justify-between rounded-md border px-2.5 py-2 text-xs transition-colors ${
                        isActive
                          ? "border-primary/40 bg-accent text-accent-foreground"
                          : "border-transparent hover:bg-muted/70"
                      }`}
                    >
                      <div className="min-w-0 flex-1 space-y-1">
                        <div className="flex items-center gap-1.5 font-medium">
                          {isCode ? (
                            <FileCode2 className="h-3.5 w-3.5 shrink-0 text-blue-500" />
                          ) : (
                            <FileText className="h-3.5 w-3.5 shrink-0 text-emerald-500" />
                          )}
                          <span className="truncate font-mono text-[11px]">
                            {entry.document.path}
                          </span>
                        </div>
                        <div className="flex items-center gap-2 font-mono text-[10px] text-muted-foreground tabular-nums">
                          <span>{formatBytes(entry.document.size_bytes)}</span>
                          <span>·</span>
                          <span>{entry.parseTimeMs.toFixed(2)} ms</span>
                          <span>·</span>
                          <span>{entry.fingerprint.unit_hashes.length} 单元</span>
                        </div>
                      </div>
                      <Button
                        size="icon"
                        variant="ghost"
                        className="h-6 w-6 shrink-0 opacity-0 group-hover:opacity-100"
                        onClick={(e) => {
                          e.stopPropagation();
                          handleRemoveEntry(entry.id);
                        }}
                      >
                        <Trash2 className="h-3 w-3 text-destructive" />
                      </Button>
                    </div>
                  );
                })}
              </div>
            )}
          </ScrollArea>
        </aside>

        {/* Right Column: Structured AST / Markdown & Interactive Chunk Inspector */}
        <main className="flex-1 overflow-hidden">
          {!selectedEntry ? (
            <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center text-muted-foreground">
              <Layers className="h-10 w-10 opacity-30" />
              <div className="space-y-1">
                <p className="text-sm font-medium text-foreground">等待文档输入</p>
                <p className="max-w-md text-xs">
                  Rust 后端将使用 <code className="font-mono">pulldown-cmark</code> 解析 Markdown
                  标题层级，使用 <code className="font-mono">tree-sitter</code> 提取代码 AST
                  符号，并通过 <code className="font-mono">tiktoken-rs</code> 实时调优 BPE 语义切块。
                </p>
              </div>
            </div>
          ) : (
            <ScrollArea className="h-full">
              <div className="mx-auto max-w-5xl space-y-5 p-6">
                {/* Metadata Header Card */}
                <Card className="p-4">
                  <div className="flex flex-wrap items-center justify-between gap-3">
                    <div className="space-y-1">
                      <div className="flex items-center gap-2">
                        <h2 className="font-mono text-sm font-semibold">
                          {selectedEntry.document.path}
                        </h2>
                        <Badge variant="secondary" className="font-mono text-[10px] uppercase">
                          {selectedEntry.document.content.kind === "code"
                            ? selectedEntry.document.content.language
                            : selectedEntry.document.content.kind}
                        </Badge>
                      </div>
                      <div className="flex flex-wrap items-center gap-3 font-mono text-[11px] text-muted-foreground tabular-nums">
                        <span>大小: {formatBytes(selectedEntry.document.size_bytes)}</span>
                        <span>解析耗时: {selectedEntry.parseTimeMs.toFixed(2)} ms</span>
                        <span>结构单元: {selectedEntry.fingerprint.unit_hashes.length}</span>
                      </div>
                    </div>

                    <div className="flex items-center gap-1.5 rounded-md border bg-muted/50 px-2.5 py-1.5 font-mono text-[11px]">
                      <Fingerprint className="h-3.5 w-3.5 text-primary" />
                      <span className="text-muted-foreground">BLAKE3:</span>
                      <span title={selectedEntry.document.content_hash}>
                        {selectedEntry.document.content_hash.slice(0, 16)}…
                      </span>
                    </div>
                  </div>
                </Card>

                {/* View Mode Switcher */}
                <div className="flex items-center gap-2 border-b pb-3">
                  <Button
                    size="sm"
                    variant={activeTab === "chunks" ? "default" : "outline"}
                    className="h-8 gap-1.5 text-xs"
                    onClick={() => setActiveTab("chunks")}
                  >
                    <Scissors className="h-3.5 w-3.5" />
                    <span>切块探针 (Chunk Inspector)</span>
                  </Button>
                  <Button
                    size="sm"
                    variant={activeTab === "outline" ? "default" : "outline"}
                    className="h-8 gap-1.5 text-xs"
                    onClick={() => setActiveTab("outline")}
                  >
                    <Braces className="h-3.5 w-3.5" />
                    <span>AST 符号树与 Blake3 指纹</span>
                  </Button>
                </div>

                {activeTab === "chunks" ? (
                  <ChunkInspector
                    filename={selectedEntry.document.path}
                    content={selectedEntry.rawContent}
                  />
                ) : (
                  <div className="space-y-5">
                    {/* Content-Type Specific Visualization */}
                    {selectedEntry.document.content.kind === "code" && (
                      <section className="space-y-3">
                        <div className="flex items-center justify-between">
                          <h3 className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                            <FileCode2 className="h-3.5 w-3.5" />
                            <span>
                              AST 符号树 ({selectedEntry.document.content.symbols.length} 个符号 ·{" "}
                              {selectedEntry.document.content.total_lines} 行)
                            </span>
                          </h3>
                        </div>

                        <div className="space-y-2">
                          {selectedEntry.document.content.symbols.map(
                            (sym: CodeSymbol, idx: number) => {
                              const symKey = `${sym.kind}-${sym.name}-${sym.start_line}-${idx}`;
                              const isOpen = expandedSymbol === symKey;
                              return (
                                <Card key={symKey} className="overflow-hidden">
                                  <button
                                    type="button"
                                    onClick={() => setExpandedSymbol(isOpen ? null : symKey)}
                                    className="flex w-full items-start justify-between gap-3 p-3 text-left transition-colors hover:bg-muted/40"
                                  >
                                    <div className="min-w-0 flex-1 space-y-1">
                                      <div className="flex flex-wrap items-center gap-2">
                                        {isOpen ? (
                                          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                                        ) : (
                                          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                                        )}
                                        <Badge
                                          variant="outline"
                                          className={`font-mono text-[10px] ${SYMBOL_STYLES[sym.kind]}`}
                                        >
                                          {sym.kind}
                                        </Badge>
                                        <span className="font-mono text-xs font-semibold">
                                          {sym.name}
                                        </span>
                                      </div>
                                      {sym.docstring && (
                                        <p className="pl-5 text-xs text-muted-foreground">
                                          {sym.docstring}
                                        </p>
                                      )}
                                      <p className="truncate pl-5 font-mono text-[11px] text-muted-foreground/80">
                                        {sym.signature}
                                      </p>
                                    </div>
                                    <Badge
                                      variant="outline"
                                      className="shrink-0 font-mono text-[10px] tabular-nums"
                                    >
                                      L{sym.start_line}–L{sym.end_line}
                                    </Badge>
                                  </button>

                                  {isOpen && (
                                    <div className="border-t bg-muted/30 p-3">
                                      <pre className="overflow-x-auto font-mono text-xs leading-relaxed">
                                        <code>{sym.source}</code>
                                      </pre>
                                    </div>
                                  )}
                                </Card>
                              );
                            }
                          )}
                        </div>
                      </section>
                    )}

                    {selectedEntry.document.content.kind === "markdown" && (
                      <section className="space-y-3">
                        <h3 className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                          <Hash className="h-3.5 w-3.5" />
                          <span>
                            Markdown 结构化目录 ({selectedEntry.document.content.sections.length}{" "}
                            个章节)
                          </span>
                        </h3>

                        <div className="space-y-2.5">
                          {selectedEntry.document.content.sections.map((section, idx) => (
                            <Card
                              key={`${section.title}-${idx}`}
                              className="p-4"
                              style={{
                                marginLeft: `${Math.max(0, section.level - 1) * 16}px`,
                              }}
                            >
                              <div className="mb-1.5 flex flex-wrap items-center justify-between gap-2">
                                <div className="flex items-center gap-2">
                                  <Badge variant="outline" className="font-mono text-[10px]">
                                    {section.level > 0 ? `H${section.level}` : "PREAMBLE"}
                                  </Badge>
                                  <span className="text-sm font-semibold">
                                    {section.title || "(无标题前置段落)"}
                                  </span>
                                </div>
                                {section.code_blocks.length > 0 && (
                                  <Badge variant="secondary" className="font-mono text-[10px]">
                                    {section.code_blocks.length} 代码块
                                  </Badge>
                                )}
                              </div>

                              {section.breadcrumbs.length > 0 && (
                                <p className="mb-2 font-mono text-[10px] text-muted-foreground">
                                  {section.breadcrumbs.join(" > ")}
                                </p>
                              )}

                              {section.content && (
                                <p className="whitespace-pre-wrap text-xs leading-relaxed text-foreground/90">
                                  {section.content}
                                </p>
                              )}
                            </Card>
                          ))}
                        </div>
                      </section>
                    )}

                    {selectedEntry.document.content.kind === "plain_text" && (
                      <Card className="p-4">
                        <pre className="whitespace-pre-wrap font-mono text-xs leading-relaxed">
                          {selectedEntry.document.content.content}
                        </pre>
                      </Card>
                    )}

                    {/* Blake3 Unit Fingerprints */}
                    <section className="space-y-2">
                      <h3 className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                        <Fingerprint className="h-3.5 w-3.5" />
                        <span>Blake3 单元级增量指纹 (用于零冗余 RAG 增量索引)</span>
                      </h3>
                      <Card className="divide-y">
                        {selectedEntry.fingerprint.unit_hashes.map((u, idx) => (
                          <div
                            key={`${u.unit_key}-${idx}`}
                            className="flex items-center justify-between gap-4 px-3.5 py-2 font-mono text-[11px]"
                          >
                            <span className="truncate text-foreground">{u.unit_key}</span>
                            <span className="shrink-0 text-muted-foreground">
                              {u.hash.slice(0, 20)}…
                            </span>
                          </div>
                        ))}
                      </Card>
                    </section>
                  </div>
                )}
              </div>
            </ScrollArea>
          )}
        </main>
      </div>
    </div>
  );
}
