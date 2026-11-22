"use client";

import React, { useEffect, useState } from "react";
import {
  Scissors,
  Sliders,
  Hash,
  Fingerprint,
  Clock,
  Copy,
  Check,
  AlertCircle,
  Loader2,
  Code2,
  FileText,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { api } from "@/lib/api";
import type {
  ChunkDocumentResponse,
  DocumentChunk,
  TokenizerEncoding,
} from "@/types/api";

interface ChunkInspectorProps {
  filename: string;
  content: string;
}

const CHUNK_ACCENT_BORDERS = [
  "border-l-blue-500",
  "border-l-emerald-500",
  "border-l-amber-500",
  "border-l-purple-500",
  "border-l-rose-500",
  "border-l-cyan-500",
];

export function ChunkInspector({ filename, content }: ChunkInspectorProps) {
  const [maxTokens, setMaxTokens] = useState<number>(256);
  const [overlapTokens, setOverlapTokens] = useState<number>(32);
  const [encoding, setEncoding] = useState<TokenizerEncoding>("cl100k_base");
  const [includeHeaders, setIncludeHeaders] = useState<boolean>(true);
  const [showRawOnly, setShowRawOnly] = useState<boolean>(false);

  const [result, setResult] = useState<ChunkDocumentResponse | null>(null);
  const [isLoading, setIsLoading] = useState<boolean>(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const maxAllowedOverlap = Math.min(256, Math.max(0, maxTokens - 16));
  const safeOverlap = Math.min(overlapTokens, maxAllowedOverlap);

  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(async () => {
      setIsLoading(true);
      setErrorMsg(null);
      try {
        const res = await api.chunkDocument({
          filename,
          content,
          max_tokens: maxTokens,
          overlap_tokens: safeOverlap,
          min_chunk_tokens: Math.min(32, Math.floor(maxTokens / 4)),
          encoding,
          include_context_headers: includeHeaders,
        });
        if (!cancelled) {
          setResult(res);
        }
      } catch (err: unknown) {
        if (!cancelled) {
          const msg = err instanceof Error ? err.message : String(err);
          setErrorMsg(`切块计算失败: ${msg}`);
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    }, 150);

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [filename, content, maxTokens, safeOverlap, encoding, includeHeaders]);

  const handleCopy = async (chunk: DocumentChunk) => {
    const text = showRawOnly ? chunk.raw_content : chunk.content;
    try {
      await navigator.clipboard.writeText(text);
      setCopiedId(chunk.chunk_id);
      setTimeout(() => setCopiedId((prev) => (prev === chunk.chunk_id ? null : prev)), 1500);
    } catch {
      // clipboard unavailable
    }
  };

  return (
    <div className="space-y-4">
      {/* Parameter Tuning Controls */}
      <Card className="p-4">
        <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2 text-xs font-semibold">
            <Sliders className="h-3.5 w-3.5 text-primary" />
            <span>实时切块参数调优</span>
            {isLoading && <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />}
          </div>

          <div className="flex flex-wrap items-center gap-1.5">
            <div className="flex items-center rounded-md border bg-muted/40 p-0.5">
              {(["cl100k_base", "o200k_base"] as TokenizerEncoding[]).map((enc) => (
                <button
                  key={enc}
                  type="button"
                  onClick={() => setEncoding(enc)}
                  className={`rounded px-2 py-0.5 font-mono text-[10px] transition-colors ${
                    encoding === enc
                      ? "bg-background font-semibold text-foreground shadow-xs"
                      : "text-muted-foreground hover:text-foreground"
                  }`}
                >
                  {enc}
                </button>
              ))}
            </div>

            <Button
              size="sm"
              variant={includeHeaders ? "default" : "outline"}
              className="h-7 px-2 text-[11px]"
              onClick={() => setIncludeHeaders((v) => !v)}
            >
              {includeHeaders ? "已附加面包屑/模块头" : "无上下文头"}
            </Button>

            <Button
              size="sm"
              variant={showRawOnly ? "secondary" : "outline"}
              className="h-7 px-2 text-[11px]"
              onClick={() => setShowRawOnly((v) => !v)}
            >
              {showRawOnly ? "视图：原始片段" : "视图：完整 Embedding 文本"}
            </Button>
          </div>
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          {/* Chunk Size Slider */}
          <div className="space-y-1.5">
            <div className="flex items-center justify-between text-xs">
              <label htmlFor="chunk-size-slider" className="text-muted-foreground">
                Chunk Size (max_tokens)
              </label>
              <span className="font-mono font-semibold tabular-nums">{maxTokens} tokens</span>
            </div>
            <input
              id="chunk-size-slider"
              type="range"
              min={64}
              max={1024}
              step={16}
              value={maxTokens}
              onChange={(e) => {
                const next = Number(e.target.value);
                setMaxTokens(next);
                if (overlapTokens >= next - 16) {
                  setOverlapTokens(Math.max(0, next - 16));
                }
              }}
              className="h-1.5 w-full cursor-pointer accent-primary"
            />
            <div className="flex justify-between font-mono text-[10px] text-muted-foreground">
              <span>64</span>
              <span>256</span>
              <span>512</span>
              <span>1024</span>
            </div>
          </div>

          {/* Overlap Slider */}
          <div className="space-y-1.5">
            <div className="flex items-center justify-between text-xs">
              <label htmlFor="chunk-overlap-slider" className="text-muted-foreground">
                Overlap (overlap_tokens)
              </label>
              <span className="font-mono font-semibold tabular-nums">{safeOverlap} tokens</span>
            </div>
            <input
              id="chunk-overlap-slider"
              type="range"
              min={0}
              max={maxAllowedOverlap}
              step={8}
              value={safeOverlap}
              onChange={(e) => setOverlapTokens(Number(e.target.value))}
              className="h-1.5 w-full cursor-pointer accent-primary"
            />
            <div className="flex justify-between font-mono text-[10px] text-muted-foreground">
              <span>0</span>
              <span>{Math.floor(maxAllowedOverlap / 2)}</span>
              <span>{maxAllowedOverlap}</span>
            </div>
          </div>
        </div>
      </Card>

      {errorMsg && (
        <div className="flex items-center gap-2 rounded-md border border-destructive/30 bg-destructive/10 p-3 text-xs text-destructive">
          <AlertCircle className="h-4 w-4 shrink-0" />
          <span>{errorMsg}</span>
        </div>
      )}

      {/* Summary Metrics Strip */}
      {result && (
        <div className="grid grid-cols-2 gap-2.5 sm:grid-cols-4">
          <Card className="p-3">
            <p className="text-[11px] text-muted-foreground">切块总数</p>
            <p className="mt-0.5 font-mono text-lg font-semibold tabular-nums">
              {result.total_chunks}
            </p>
          </Card>
          <Card className="p-3">
            <p className="text-[11px] text-muted-foreground">BPE 总 Token</p>
            <p className="mt-0.5 font-mono text-lg font-semibold tabular-nums">
              {result.total_tokens}
            </p>
          </Card>
          <Card className="p-3">
            <p className="text-[11px] text-muted-foreground">平均 Token / Chunk</p>
            <p className="mt-0.5 font-mono text-lg font-semibold tabular-nums">
              {result.avg_tokens.toFixed(1)}
            </p>
          </Card>
          <Card className="p-3">
            <p className="flex items-center gap-1 text-[11px] text-muted-foreground">
              <Clock className="h-3 w-3 text-emerald-500" />
              <span>Rust 切块耗时</span>
            </p>
            <p className="mt-0.5 font-mono text-lg font-semibold tabular-nums">
              {result.chunk_time_ms.toFixed(2)} ms
            </p>
          </Card>
        </div>
      )}

      {/* Chunk Cards */}
      {result && result.chunks.length === 0 ? (
        <Card className="p-8 text-center text-xs text-muted-foreground">
          未生成任何有效 Chunk。
        </Card>
      ) : (
        <div className="space-y-3">
          {result?.chunks.map((chunk, idx) => {
            const utilization = Math.min(100, Math.round((chunk.token_count / maxTokens) * 100));
            const borderClass = CHUNK_ACCENT_BORDERS[idx % CHUNK_ACCENT_BORDERS.length];
            const barColor =
              utilization >= 90
                ? "bg-amber-500"
                : utilization >= 40
                  ? "bg-emerald-500"
                  : "bg-blue-500";
            const displayedText = showRawOnly ? chunk.raw_content : chunk.content;

            return (
              <Card
                key={chunk.chunk_id}
                className={`overflow-hidden border-l-4 ${borderClass}`}
              >
                <div className="flex flex-wrap items-center justify-between gap-2 border-b bg-muted/25 px-3.5 py-2.5">
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge variant="default" className="font-mono text-[10px] tabular-nums">
                      <Scissors className="mr-1 h-2.5 w-2.5" />
                      Chunk #{chunk.chunk_index}
                    </Badge>

                    <Badge variant="outline" className="gap-1 font-mono text-[10px]">
                      {chunk.metadata.kind === "code" ? (
                        <Code2 className="h-3 w-3 text-blue-500" />
                      ) : (
                        <FileText className="h-3 w-3 text-emerald-500" />
                      )}
                      <span>
                        {chunk.metadata.language ?? chunk.metadata.kind}
                      </span>
                    </Badge>

                    {chunk.metadata.breadcrumbs.length > 0 && (
                      <Badge variant="secondary" className="gap-1 font-mono text-[10px]">
                        <Hash className="h-2.5 w-2.5" />
                        <span>{chunk.metadata.breadcrumbs.join(" > ")}</span>
                      </Badge>
                    )}

                    {chunk.metadata.symbol_name && (
                      <Badge variant="secondary" className="font-mono text-[10px]">
                        {chunk.metadata.symbol_kind ? `${chunk.metadata.symbol_kind} ` : ""}
                        {chunk.metadata.parent_symbol
                          ? `${chunk.metadata.parent_symbol}::${chunk.metadata.symbol_name}`
                          : chunk.metadata.symbol_name}
                      </Badge>
                    )}

                    {chunk.metadata.line_range && (
                      <span className="font-mono text-[10px] text-muted-foreground tabular-nums">
                        L{chunk.metadata.line_range.start}–L{chunk.metadata.line_range.end}
                      </span>
                    )}
                  </div>

                  <div className="flex items-center gap-2.5">
                    <div className="flex items-center gap-1.5">
                      <div className="h-1.5 w-16 overflow-hidden rounded-full bg-muted">
                        <div
                          className={`h-full rounded-full transition-all ${barColor}`}
                          style={{ width: `${utilization}%` }}
                        />
                      </div>
                      <span className="font-mono text-[11px] font-medium tabular-nums">
                        {chunk.token_count}/{maxTokens} tok ({utilization}%)
                      </span>
                    </div>

                    <Button
                      size="icon"
                      variant="ghost"
                      className="h-6 w-6"
                      title="复制 Chunk 内容"
                      onClick={() => void handleCopy(chunk)}
                    >
                      {copiedId === chunk.chunk_id ? (
                        <Check className="h-3.5 w-3.5 text-emerald-500" />
                      ) : (
                        <Copy className="h-3.5 w-3.5 text-muted-foreground" />
                      )}
                    </Button>
                  </div>
                </div>

                <div className="p-3.5">
                  <pre className="overflow-x-auto whitespace-pre-wrap break-words font-mono text-xs leading-relaxed text-foreground/95">
                    <code>{displayedText}</code>
                  </pre>

                  <div className="mt-3 flex flex-wrap items-center justify-between gap-2 border-t pt-2 font-mono text-[10px] text-muted-foreground tabular-nums">
                    <span>
                      Bytes: [{chunk.metadata.byte_range.start}, {chunk.metadata.byte_range.end})
                      {chunk.metadata.module_path ? ` · Module: ${chunk.metadata.module_path}` : ""}
                    </span>
                    <span className="flex items-center gap-1" title={chunk.content_hash}>
                      <Fingerprint className="h-3 w-3" />
                      <span>{chunk.content_hash.slice(0, 14)}…</span>
                    </span>
                  </div>
                </div>
              </Card>
            );
          })}
        </div>
      )}
    </div>
  );
}
