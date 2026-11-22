import type {
  ApiErrorResponse,
  ChunkDocumentRequest,
  ChunkDocumentResponse,
  Conversation,
  Message,
  ModelsInfo,
  ModelTier,
  ParseBatchResponse,
  ParseDocumentResponse,
  StreamDonePayload,
  TechReport,
} from "@/types/api";

const API_BASE =
  process.env.NEXT_PUBLIC_API_BASE?.replace(/\/$/, "") || "http://127.0.0.1:3000";

export type ModelRoutingOption =
  | { tier: ModelTier; model?: never }
  | { model: string; tier?: never }
  | { tier?: undefined; model?: undefined };

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init?.headers || {}),
    },
  });

  if (!res.ok) {
    let message = `Request failed: ${res.status}`;
    try {
      const err = (await res.json()) as Partial<ApiErrorResponse>;
      if (err?.error?.message) {
        message = err.error.message;
      }
    } catch {
      // fallback to status code message
    }
    throw new Error(message);
  }

  if (res.status === 204) {
    return undefined as unknown as T;
  }

  return res.json();
}

export const api = {
  getModels: (signal?: AbortSignal) => request<ModelsInfo>("/api/models", { signal }),

  listConversations: (signal?: AbortSignal) =>
    request<Conversation[]>("/api/conversations", { signal }),

  createConversation: (title?: string) =>
    request<Conversation>("/api/conversations", {
      method: "POST",
      body: JSON.stringify({ title: title?.trim() || undefined }),
    }),

  deleteConversation: (id: string) =>
    request<void>(`/api/conversations/${id}`, {
      method: "DELETE",
    }),

  listMessages: (conversationId: string, limit = 50, signal?: AbortSignal) =>
    request<Message[]>(`/api/conversations/${conversationId}/messages?limit=${limit}`, {
      signal,
    }),

  analyzeTechReport: (
    prompt: string,
    routing?: ModelRoutingOption,
    signal?: AbortSignal
  ) =>
    request<TechReport>("/api/analyze", {
      method: "POST",
      body: JSON.stringify({
        prompt,
        ...(routing?.tier ? { tier: routing.tier } : {}),
        ...(routing?.model ? { model: routing.model } : {}),
      }),
      signal,
    }),

  parseDocument: (filename: string, content: string, signal?: AbortSignal) =>
    request<ParseDocumentResponse>("/api/documents/parse", {
      method: "POST",
      body: JSON.stringify({ filename, content }),
      signal,
    }),

  parseDocumentsBatch: (
    files: Array<{ filename: string; content: string }>,
    signal?: AbortSignal
  ) =>
    request<ParseBatchResponse>("/api/documents/parse-batch", {
      method: "POST",
      body: JSON.stringify({ files }),
      signal,
    }),

  chunkDocument: (payload: ChunkDocumentRequest, signal?: AbortSignal) =>
    request<ChunkDocumentResponse>("/api/documents/chunk", {
      method: "POST",
      body: JSON.stringify(payload),
      signal,
    }),
};

export interface StreamChatCallbacks {
  onDelta: (delta: string) => void;
  onDone: (messageId: string) => void;
  onError: (error: Error) => void;
}

export type StreamChatOptions = ModelRoutingOption & {
  signal?: AbortSignal;
};

function parseSseBlock(eventBlock: string): { eventType: string; data: string } | null {
  if (!eventBlock.trim()) return null;

  let eventType = "message";
  const dataLines: string[] = [];

  for (const line of eventBlock.split("\n")) {
    if (line.startsWith(":")) {
      // SSE comment / keep-alive
      continue;
    }
    if (line.startsWith("event:")) {
      eventType = line.slice(6).trim();
    } else if (line.startsWith("data:")) {
      const value = line.startsWith("data: ") ? line.slice(6) : line.slice(5);
      dataLines.push(value);
    }
  }

  if (dataLines.length === 0) return null;
  return { eventType, data: dataLines.join("\n") };
}

export async function streamConversationChat(
  conversationId: string,
  prompt: string,
  options: StreamChatOptions,
  callbacks: StreamChatCallbacks
): Promise<void> {
  const url = `${API_BASE}/api/conversations/${conversationId}/chat/stream`;

  let response: Response;
  try {
    response = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        prompt,
        ...(options.tier ? { tier: options.tier } : {}),
        ...(options.model ? { model: options.model } : {}),
      }),
      signal: options.signal,
    });
  } catch (err: unknown) {
    if (options.signal?.aborted) return;
    const error = err instanceof Error ? err : new Error(String(err));
    callbacks.onError(error);
    throw error;
  }

  if (!response.ok) {
    let message = `Stream request failed: ${response.status}`;
    try {
      const err = (await response.json()) as Partial<ApiErrorResponse>;
      if (err?.error?.message) message = err.error.message;
    } catch {
      // ignore
    }
    const error = new Error(message);
    callbacks.onError(error);
    throw error;
  }

  const reader = response.body?.getReader();
  if (!reader) {
    const error = new Error("No readable stream in response");
    callbacks.onError(error);
    throw error;
  }

  const decoder = new TextDecoder();
  let buffer = "";
  let receivedTerminalEvent = false;

  const dispatchBlock = (eventBlock: string): boolean => {
    const parsed = parseSseBlock(eventBlock);
    if (!parsed) return false;

    const { eventType, data } = parsed;
    if (eventType === "delta") {
      try {
        const chunk = JSON.parse(data);
        callbacks.onDelta(typeof chunk === "string" ? chunk : String(chunk));
      } catch {
        callbacks.onDelta(data);
      }
      return false;
    }

    if (eventType === "done") {
      receivedTerminalEvent = true;
      try {
        const payload = JSON.parse(data) as Partial<StreamDonePayload>;
        callbacks.onDone(payload.message_id ?? "");
      } catch {
        callbacks.onDone("");
      }
      return true;
    }

    if (eventType === "error") {
      receivedTerminalEvent = true;
      let errorMsg = "Streaming error occurred";
      try {
        const payload = JSON.parse(data) as Partial<ApiErrorResponse>;
        if (payload?.error?.message) errorMsg = payload.error.message;
      } catch {
        // ignore
      }
      callbacks.onError(new Error(errorMsg));
      return true;
    }

    return false;
  };

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        buffer += decoder.decode().replace(/\r\n?/g, "\n");
        if (buffer.trim()) {
          dispatchBlock(buffer);
        }
        break;
      }

      buffer += decoder.decode(value, { stream: true }).replace(/\r\n?/g, "\n");
      const events = buffer.split("\n\n");
      buffer = events.pop() ?? "";

      for (const eventBlock of events) {
        if (dispatchBlock(eventBlock)) {
          return;
        }
      }
    }

    if (!receivedTerminalEvent && !options.signal?.aborted) {
      callbacks.onError(new Error("Stream ended unexpectedly before completion"));
    }
  } catch (err: unknown) {
    if (options.signal?.aborted) return;
    const error = err instanceof Error ? err : new Error(String(err));
    callbacks.onError(error);
    throw error;
  } finally {
    reader.releaseLock();
  }
}
