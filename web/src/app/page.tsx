"use client";

import React, { useEffect, useState, useRef, useCallback } from "react";
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
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";
import { MarkdownMessage } from "@/components/MarkdownMessage";
import { TechReportCard } from "@/components/TechReportCard";
import {
  api,
  streamConversationChat,
} from "@/lib/api";
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
  const [, setModelsInfo] = useState<ModelsInfo | null>(null);
  const [selectedTier, setSelectedTier] = useState<ModelTier>("fast");
  const [selectedModel, setSelectedModel] = useState<string>("");
  const [deepAnalysisMode, setDeepAnalysisMode] = useState(false);

  const [isStreaming, setIsStreaming] = useState(false);
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [structuredReport, setStructuredReport] = useState<TechReport | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const abortControllerRef = useRef<AbortController | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom
  const scrollToBottom = () => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  };

  useEffect(() => {
    scrollToBottom();
  }, [messages, structuredReport, isStreaming]);

  const loadConversations = useCallback(async () => {
    try {
      const list = await api.listConversations();
      setConversations(list);
      if (list.length > 0) {
        setActiveId((prev) => prev ?? list[0].id);
      }
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(`获取会话列表失败: ${msg}`);
    }
  }, []);

  const loadMessages = useCallback(async (convId: string) => {
    try {
      setErrorMsg(null);
      const history = await api.listMessages(convId);
      setMessages(history);
      setStructuredReport(null);
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(`加载消息历史失败: ${msg}`);
    }
  }, []);

  // Load models metadata and conversation list on mount
  useEffect(() => {
    api
      .getModels()
      .then((info) => {
        setModelsInfo(info);
        setSelectedTier(info.default_tier);
        if (info.models.length > 0) {
          setSelectedModel(info.models[0]);
        }
      })
      .catch((err) => console.warn("Could not fetch models metadata:", err));

    void loadConversations();
  }, [loadConversations]);

  // When active conversation changes, load its message history
  useEffect(() => {
    if (activeId) {
      void loadMessages(activeId);
    }
  }, [activeId, loadMessages]);

  const handleNewConversation = async () => {
    try {
      setErrorMsg(null);
      const conv = await api.createConversation();
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
    try {
      await api.deleteConversation(convId);
      const updated = conversations.filter((c) => c.id !== convId);
      setConversations(updated);
      if (activeId === convId) {
        const nextId = updated.length > 0 ? updated[0].id : null;
        setActiveId(nextId);
        if (!nextId) {
          setMessages([]);
        }
      }
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(`删除会话失败: ${msg}`);
    }
  };

  const handleStopGeneration = () => {
    if (abortControllerRef.current) {
      abortControllerRef.current.abort();
      abortControllerRef.current = null;
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
      setIsAnalyzing(true);
      try {
        const report = await api.analyzeTechReport(
          prompt,
          selectedTier,
          selectedModel || undefined
        );
        setStructuredReport(report);
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        setErrorMsg(`结构化分析失败: ${msg}`);
      } finally {
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
        setConversations((prev) => [newConv, ...prev]);
        setActiveId(newConv.id);
        currentConvId = newConv.id;
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        setErrorMsg(`创建会话失败: ${msg}`);
        return;
      }
    }

    const optimisticUserMessage: Message = {
      id: "temp-user-" + Date.now(),
      conversation_id: currentConvId,
      role: "user",
      content: prompt,
      token_count: null,
      created_at: new Date().toISOString(),
    };

    const optimisticAssistantMessage: Message = {
      id: "temp-assistant-" + Date.now(),
      conversation_id: currentConvId,
      role: "assistant",
      content: "",
      token_count: null,
      created_at: new Date().toISOString(),
    };

    setMessages((prev) => [...prev, optimisticUserMessage, optimisticAssistantMessage]);
    setIsStreaming(true);

    const controller = new AbortController();
    abortControllerRef.current = controller;

    try {
      await streamConversationChat(
        currentConvId,
        prompt,
        {
          tier: selectedTier,
          model: selectedModel || undefined,
          signal: controller.signal,
        },
        {
          onDelta: (chunk) => {
            setMessages((prev) => {
              const next = [...prev];
              const last = next[next.length - 1];
              if (last && last.role === "assistant") {
                next[next.length - 1] = {
                  ...last,
                  content: last.content + chunk,
                };
              }
              return next;
            });
          },
          onDone: (messageId) => {
            setIsStreaming(false);
            abortControllerRef.current = null;
            if (messageId) {
              setMessages((prev) => {
                const next = [...prev];
                const last = next[next.length - 1];
                if (last && last.role === "assistant") {
                  next[next.length - 1] = {
                    ...last,
                    id: messageId,
                  };
                }
                return next;
              });
            }
          },
          onError: (err) => {
            setIsStreaming(false);
            abortControllerRef.current = null;
            setErrorMsg(`流式输出异常: ${err.message}`);
          },
        }
      );
    } catch {
      setIsStreaming(false);
      abortControllerRef.current = null;
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

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
                    onClick={() => setActiveId(conv.id)}
                    className={`group flex cursor-pointer items-center justify-between rounded-md px-3 py-2 text-xs transition-colors ${
                      isActive
                        ? "bg-accent text-accent-foreground font-medium"
                        : "hover:bg-muted/80 text-muted-foreground"
                    }`}
                  >
                    <div className="flex items-center gap-2 truncate">
                      <MessageSquare className="h-3.5 w-3.5 shrink-0 opacity-70" />
                      <span className="truncate">
                        {conv.title || "新对话"}
                      </span>
                    </div>
                    <Button
                      size="icon"
                      variant="ghost"
                      className="h-6 w-6 opacity-0 transition-opacity group-hover:opacity-100"
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
              <div className="flex items-center gap-2 rounded-lg border border-destructive/20 bg-destructive/10 p-3 text-xs text-destructive">
                <AlertCircle className="h-4 w-4 shrink-0" />
                <span>{errorMsg}</span>
              </div>
            )}

            {messages.length === 0 && !structuredReport ? (
              <div className="flex flex-col items-center justify-center py-20 text-center">
                <Bot className="h-10 w-10 text-muted-foreground/40 mb-3" />
                <h3 className="text-sm font-semibold text-foreground">
                  欢迎使用 Rust + Next.js 全栈 AI 工作台
                </h3>
                <p className="mt-1 text-xs text-muted-foreground max-w-sm">
                  支持毫秒级 SSE Token 流式打字机输出、即时断开与中止保护，以及强类型 JSON Schema 结构化卡片分析。
                </p>
              </div>
            ) : null}

            {messages.map((msg) => {
              const isUser = msg.role === "user";
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
                      <MarkdownMessage content={msg.content || (isStreaming ? "▍" : "")} />
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
