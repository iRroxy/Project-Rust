"use client";

import React, { useEffect, useState, useRef, useCallback } from "react";
import Link from "next/link";
import {
  Plus,
  Trash2,
  Send,
  Square,
  Bot,
  User,
  Sparkles,
  Zap,
  Cpu,
  MessageSquare,
  AlertCircle,
  FileCode2,
} from "lucide-react";
import { Button, buttonVariants } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";
import { MarkdownMessage } from "@/components/MarkdownMessage";
import { TechReportCard } from "@/components/TechReportCard";
import { api, streamConversationChat } from "@/lib/api";
import { cn } from "@/lib/utils";
import type {
  Conversation,
  Message,
  ModelsInfo,
  ModelTier,
  TechReport,
} from "@/types/api";

export default function WorkspacePage() {
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [modelsInfo, setModelsInfo] = useState<ModelsInfo | null>(null);
  const [selectedTier, setSelectedTier] = useState<ModelTier>("fast");
  const [deepAnalysisMode, setDeepAnalysisMode] = useState(false);

  const [isStreaming, setIsStreaming] = useState(false);
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [structuredReport, setStructuredReport] = useState<TechReport | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const streamAbortRef = useRef<AbortController | null>(null);
  const messagesAbortRef = useRef<AbortController | null>(null);
  const analyzeAbortRef = useRef<AbortController | null>(null);
  const skipNextLoadIdRef = useRef<string | null>(null);
  const activeIdRef = useRef<string | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    activeIdRef.current = activeId;
  }, [activeId]);

  const abortActiveRequests = useCallback(() => {
    if (streamAbortRef.current) {
      streamAbortRef.current.abort();
      streamAbortRef.current = null;
      setIsStreaming(false);
    }
    if (analyzeAbortRef.current) {
      analyzeAbortRef.current.abort();
      analyzeAbortRef.current = null;
      setIsAnalyzing(false);
    }
  }, []);

  // Abort all in-flight streams/fetches when unmounting
  useEffect(() => {
    return () => {
      streamAbortRef.current?.abort();
      messagesAbortRef.current?.abort();
      analyzeAbortRef.current?.abort();
    };
  }, []);

  // Auto-scroll to bottom
  const scrollToBottom = () => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  };

  useEffect(() => {
    scrollToBottom();
  }, [messages, structuredReport, isStreaming]);

  const loadConversations = useCallback(async (signal?: AbortSignal) => {
    try {
      const list = await api.listConversations(signal);
      if (signal?.aborted) return;
      setConversations(list);
      if (list.length > 0) {
        setActiveId((prev) => prev ?? list[0].id);
      }
    } catch (err: unknown) {
      if (signal?.aborted) return;
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(`获取会话列表失败: ${msg}`);
    }
  }, []);

  const loadMessages = useCallback(async (convId: string) => {
    messagesAbortRef.current?.abort();
    const controller = new AbortController();
    messagesAbortRef.current = controller;

    try {
      setErrorMsg(null);
      const history = await api.listMessages(convId, 50, controller.signal);
      if (controller.signal.aborted) return;
      setMessages(history);
      setStructuredReport(null);
    } catch (err: unknown) {
      if (controller.signal.aborted) return;
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(`加载消息历史失败: ${msg}`);
    }
  }, []);

  // Load models metadata and conversation list on mount
  useEffect(() => {
    const controller = new AbortController();
    api
      .getModels(controller.signal)
      .then((info) => {
        if (controller.signal.aborted) return;
        setModelsInfo(info);
        setSelectedTier(info.default_tier);
      })
      .catch((err) => {
        if (!controller.signal.aborted) {
          console.warn("Could not fetch models metadata:", err);
        }
      });

    void loadConversations(controller.signal);
    return () => controller.abort();
  }, [loadConversations]);

  // When active conversation changes, load its message history (unless just auto-created)
  useEffect(() => {
    if (!activeId) return;
    if (skipNextLoadIdRef.current === activeId) {
      skipNextLoadIdRef.current = null;
      return;
    }
    void loadMessages(activeId);
  }, [activeId, loadMessages]);

  const handleSelectConversation = (convId: string) => {
    if (convId === activeId) return;
    abortActiveRequests();
    setActiveId(convId);
  };

  const handleNewConversation = async () => {
    abortActiveRequests();
    try {
      setErrorMsg(null);
      const conv = await api.createConversation();
      skipNextLoadIdRef.current = conv.id;
      setConversations((prev) => [conv, ...prev]);
      setActiveId(conv.id);
      setMessages([]);
      setStructuredReport(null);
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(`创建新会话失败: ${msg}`);
    }
  };

  const handleDeleteConversation = async (e: React.MouseEvent, convId: string) => {
    e.stopPropagation();
    if (activeIdRef.current === convId) {
      abortActiveRequests();
    }
    try {
      await api.deleteConversation(convId);
      let nextActive: string | null | undefined;
      setConversations((prev) => {
        const updated = prev.filter((c) => c.id !== convId);
        if (activeIdRef.current === convId) {
          nextActive = updated.length > 0 ? updated[0].id : null;
        }
        return updated;
      });
      if (nextActive !== undefined) {
        setActiveId(nextActive);
        if (nextActive === null) {
          setMessages([]);
          setStructuredReport(null);
        }
      }
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(`删除会话失败: ${msg}`);
    }
  };

  const handleStopGeneration = () => {
    if (streamAbortRef.current) {
      streamAbortRef.current.abort();
      streamAbortRef.current = null;
    }
    setIsStreaming(false);
  };

  const handleSend = async () => {
    const prompt = input.trim();
    if (!prompt || isStreaming || isAnalyzing) return;

    setErrorMsg(null);
    setInput("");

    // Generative UI Mode: Structured TechReport Analysis
    if (deepAnalysisMode) {
      analyzeAbortRef.current?.abort();
      const controller = new AbortController();
      analyzeAbortRef.current = controller;
      setIsAnalyzing(true);
      try {
        const report = await api.analyzeTechReport(
          prompt,
          { tier: selectedTier },
          controller.signal
        );
        if (!controller.signal.aborted) {
          setStructuredReport(report);
        }
      } catch (err: unknown) {
        if (!controller.signal.aborted) {
          const msg = err instanceof Error ? err.message : String(err);
          setErrorMsg(`结构化分析失败: ${msg}`);
          setInput(prompt);
        }
      } finally {
        if (analyzeAbortRef.current === controller) {
          analyzeAbortRef.current = null;
        }
        setIsAnalyzing(false);
      }
      return;
    }

    // Standard Multi-turn Streaming Conversation
    let currentConvId = activeId;
    if (!currentConvId) {
      try {
        const title = prompt.length > 20 ? prompt.slice(0, 20) + "..." : prompt;
        const newConv = await api.createConversation(title);
        // Prevent the activeId useEffect from racing with the optimistic bubbles & SSE stream
        skipNextLoadIdRef.current = newConv.id;
        activeIdRef.current = newConv.id;
        setConversations((prev) => [newConv, ...prev]);
        setActiveId(newConv.id);
        currentConvId = newConv.id;
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        setErrorMsg(`创建会话失败: ${msg}`);
        setInput(prompt);
        return;
      }
    }

    const userMsgId = `temp-user-${Date.now()}`;
    const assistantMsgId = `temp-assistant-${Date.now()}`;

    const optimisticUserMessage: Message = {
      id: userMsgId,
      conversation_id: currentConvId,
      role: "user",
      content: prompt,
      token_count: null,
      created_at: new Date().toISOString(),
    };

    const optimisticAssistantMessage: Message = {
      id: assistantMsgId,
      conversation_id: currentConvId,
      role: "assistant",
      content: "",
      token_count: null,
      created_at: new Date().toISOString(),
    };

    setMessages((prev) => [...prev, optimisticUserMessage, optimisticAssistantMessage]);
    setIsStreaming(true);

    const controller = new AbortController();
    streamAbortRef.current = controller;
    let receivedAnyDelta = false;

    try {
      await streamConversationChat(
        currentConvId,
        prompt,
        {
          tier: selectedTier,
          signal: controller.signal,
        },
        {
          onDelta: (chunk) => {
            if (activeIdRef.current !== currentConvId) return;
            receivedAnyDelta = true;
            setMessages((prev) =>
              prev.map((m) =>
                m.id === assistantMsgId ? { ...m, content: m.content + chunk } : m
              )
            );
          },
          onDone: (messageId) => {
            if (activeIdRef.current !== currentConvId) return;
            if (messageId) {
              setMessages((prev) =>
                prev.map((m) => (m.id === assistantMsgId ? { ...m, id: messageId } : m))
              );
            }
          },
          onError: (err) => {
            if (activeIdRef.current !== currentConvId) return;
            setErrorMsg(`流式输出异常: ${err.message}`);
            if (!receivedAnyDelta) {
              setMessages((prev) =>
                prev.filter((m) => m.id !== userMsgId && m.id !== assistantMsgId)
              );
              setInput(prompt);
            }
          },
        }
      );
    } catch {
      // onError callback already reported the error; cleanup happens in finally
    } finally {
      if (streamAbortRef.current === controller) {
        streamAbortRef.current = null;
      }
      setIsStreaming(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing || e.keyCode === 229) {
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void handleSend();
    }
  };

  const reasoningAvailable = modelsInfo === null || Boolean(modelsInfo.tiers.reasoning);

  return (
    <div className="flex h-screen w-full overflow-hidden bg-background text-foreground">
      {/* 1. Left Sidebar */}
      <aside className="flex w-64 flex-col border-r bg-muted/20">
        <div className="flex items-center justify-between border-b p-3">
          <div className="flex items-center gap-2 font-semibold">
            <Bot className="h-5 w-5 text-primary" />
            <span className="text-sm">Rust AI SaaS</span>
          </div>
          <Button
            size="sm"
            variant="outline"
            className="h-8 gap-1 px-2 text-xs"
            onClick={handleNewConversation}
          >
            <Plus className="h-3.5 w-3.5" />
            <span>新建会话</span>
          </Button>
        </div>

        <ScrollArea className="flex-1 p-2">
          {conversations.length === 0 ? (
            <div className="p-4 text-center text-xs text-muted-foreground">
              暂无历史会话，点击上方新建
            </div>
          ) : (
            <div className="space-y-1">
              {conversations.map((conv) => {
                const isActive = conv.id === activeId;
                return (
                  <div
                    key={conv.id}
                    className={`group flex items-center justify-between rounded-md px-1.5 py-1 text-xs transition-colors ${
                      isActive
                        ? "bg-accent text-accent-foreground font-medium"
                        : "hover:bg-muted/80 text-muted-foreground"
                    }`}
                  >
                    <button
                      type="button"
                      onClick={() => handleSelectConversation(conv.id)}
                      className="flex min-w-0 flex-1 items-center gap-2 px-1.5 py-1 text-left"
                    >
                      <MessageSquare className="h-3.5 w-3.5 shrink-0 opacity-70" />
                      <span className="truncate">{conv.title || "新对话"}</span>
                    </button>
                    <Button
                      size="icon"
                      variant="ghost"
                      aria-label={`删除会话 ${conv.title || "新对话"}`}
                      className="h-6 w-6 shrink-0 opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
                      onClick={(e) => handleDeleteConversation(e, conv.id)}
                    >
                      <Trash2 className="h-3 w-3 text-destructive" />
                    </Button>
                  </div>
                );
              })}
            </div>
          )}
        </ScrollArea>

        {/* Navigation to AST Document Parser */}
        <div className="border-t p-2">
          <Link
            href="/documents"
            className={cn(
              buttonVariants({ size: "sm", variant: "ghost" }),
              "w-full justify-start gap-2 text-xs text-muted-foreground hover:text-foreground"
            )}
          >
            <FileCode2 className="h-3.5 w-3.5 text-primary" />
            <span>文档与代码 AST 解析</span>
          </Link>
        </div>

        {/* Engine status banner */}
        <div className="border-t p-3 text-[11px] text-muted-foreground">
          <div className="flex items-center justify-between">
            <span>后端引擎</span>
            <Badge variant="outline" className="font-mono text-[10px]">
              Axum + Tokio
            </Badge>
          </div>
        </div>
      </aside>

      {/* 2. Main Workspace */}
      <main className="flex flex-1 flex-col overflow-hidden">
        {/* Header / Model Toolbar */}
        <header className="flex h-12 items-center justify-between border-b px-4">
          <div className="flex items-center gap-2">
            <span className="text-xs font-medium text-muted-foreground">
              当前模型路由：
            </span>
            <div className="flex items-center gap-1.5">
              <Button
                size="sm"
                variant={selectedTier === "fast" ? "default" : "outline"}
                className="h-7 gap-1 px-2.5 text-xs"
                onClick={() => setSelectedTier("fast")}
              >
                <Zap className="h-3 w-3" />
                <span>日常 (Fast)</span>
              </Button>
              <Button
                size="sm"
                variant={selectedTier === "reasoning" ? "default" : "outline"}
                className="h-7 gap-1 px-2.5 text-xs"
                disabled={!reasoningAvailable}
                title={
                  reasoningAvailable
                    ? modelsInfo?.tiers.reasoning ?? "推理模型"
                    : "后端未配置 LLM_REASONING_MODEL"
                }
                onClick={() => setSelectedTier("reasoning")}
              >
                <Cpu className="h-3 w-3" />
                <span>推理 (Reasoning)</span>
              </Button>
            </div>
          </div>

          <div className="flex items-center gap-2">
            <Button
              size="sm"
              variant={deepAnalysisMode ? "secondary" : "ghost"}
              className={`h-7 gap-1 px-2.5 text-xs ${
                deepAnalysisMode ? "border border-primary/30 font-semibold" : ""
              }`}
              onClick={() => setDeepAnalysisMode((prev) => !prev)}
            >
              <Sparkles className="h-3 w-3 text-amber-500" />
              <span>{deepAnalysisMode ? "深度分析卡片模式 [开]" : "深度分析模式"}</span>
            </Button>
          </div>
        </header>

        {/* Messages Scroll Area */}
        <div className="flex-1 overflow-y-auto p-4 sm:p-6">
          <div className="mx-auto max-w-3xl space-y-4">
            {errorMsg && (
              <div
                role="alert"
                className="flex items-center gap-2 rounded-lg border border-destructive/20 bg-destructive/10 p-3 text-xs text-destructive"
              >
                <AlertCircle className="h-4 w-4 shrink-0" />
                <span>{errorMsg}</span>
              </div>
            )}

            {messages.length === 0 && !structuredReport ? (
              <div className="flex flex-col items-center justify-center py-20 text-center">
                <Bot className="mb-3 h-10 w-10 text-muted-foreground/40" />
                <h3 className="text-sm font-semibold text-foreground">
                  欢迎使用 Rust + Next.js 全栈 AI 工作台
                </h3>
                <p className="mt-1 max-w-sm text-xs text-muted-foreground">
                  支持毫秒级 SSE Token 流式打字机输出、即时断开与中止保护，以及强类型 JSON Schema 结构化卡片分析。
                </p>
              </div>
            ) : null}

            {messages.map((msg, idx) => {
              const isUser = msg.role === "user";
              const isLatestAssistant = !isUser && idx === messages.length - 1;
              return (
                <div
                  key={msg.id}
                  className={`flex gap-3 ${isUser ? "justify-end" : "justify-start"}`}
                >
                  {!isUser && (
                    <div className="flex h-7 w-7 shrink-0 select-none items-center justify-center rounded-full bg-primary/10 text-primary">
                      <Bot className="h-4 w-4" />
                    </div>
                  )}
                  <div
                    className={`rounded-xl px-3.5 py-2.5 text-xs ${
                      isUser
                        ? "max-w-[80%] bg-primary text-primary-foreground"
                        : "max-w-[85%] border bg-card text-card-foreground shadow-xs"
                    }`}
                  >
                    {isUser ? (
                      <div className="whitespace-pre-wrap">{msg.content}</div>
                    ) : (
                      <MarkdownMessage
                        content={msg.content || (isStreaming && isLatestAssistant ? "▍" : "")}
                      />
                    )}
                  </div>
                  {isUser && (
                    <div className="flex h-7 w-7 shrink-0 select-none items-center justify-center rounded-full bg-muted text-muted-foreground">
                      <User className="h-4 w-4" />
                    </div>
                  )}
                </div>
              );
            })}

            {/* Generative UI Structured Card Display */}
            {structuredReport && (
              <div className="mx-auto max-w-2xl">
                <TechReportCard report={structuredReport} />
              </div>
            )}

            <div ref={messagesEndRef} />
          </div>
        </div>

        {/* Input Bar */}
        <div className="border-t bg-background/80 p-3 backdrop-blur-sm sm:p-4">
          <div className="mx-auto max-w-3xl">
            <div className="relative rounded-xl border bg-card shadow-xs focus-within:ring-1 focus-within:ring-ring">
              <Textarea
                aria-label="聊天消息输入框"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder={
                  deepAnalysisMode
                    ? "输入需分析的技术栈或主题（将直接渲染为强类型可视化卡片）..."
                    : "向 Rust 流式后端发送消息... (Enter 发送，Shift+Enter 换行)"
                }
                className="min-h-[50px] max-h-36 resize-none border-0 bg-transparent p-3 text-xs shadow-none focus-visible:ring-0"
                rows={1}
              />
              <div className="flex items-center justify-between border-t px-3 py-2 text-xs">
                <div className="flex items-center gap-1.5 text-muted-foreground">
                  <span className="text-[11px]">
                    {deepAnalysisMode ? "模式：结构化 JSON 评估" : "模式：SSE 实时打字机"}
                  </span>
                </div>
                <div className="flex items-center gap-2">
                  {isStreaming ? (
                    <Button
                      size="sm"
                      variant="destructive"
                      className="h-7 gap-1 px-2.5 text-xs"
                      onClick={handleStopGeneration}
                    >
                      <Square className="h-3 w-3" />
                      <span>停止生成</span>
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      disabled={!input.trim() || isAnalyzing}
                      className="h-7 gap-1 px-3 text-xs"
                      onClick={handleSend}
                    >
                      <Send className="h-3 w-3" />
                      <span>{isAnalyzing ? "正在生成..." : "发送"}</span>
                    </Button>
                  )}
                </div>
              </div>
            </div>
          </div>
        </div>
      </main>
    </div>
  );
}
