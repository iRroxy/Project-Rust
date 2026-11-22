import type {
  ChunkDocumentRequest,
  ChunkDocumentResponse,
  Conversation,
  Message,
  ModelsInfo,
  ModelTier,
  ParseBatchResponse,
  ParseDocumentResponse,
  TechReport,
} from "@/types/api";

const API_BASE =
  process.env.NEXT_PUBLIC_API_BASE?.replace(/\/$/, "") || "http://127.0.0.1:3000";

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
      const err = await res.json();
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
  getModels: () => request<ModelsInfo>("/api/models"),

  listConversations: () => request<Conversation[]>("/api/conversations"),

  createConversation: (title?: string) =>
    request<Conversation>("/api/conversations", {
      method: "POST",
      body: JSON.stringify({ title: title?.trim() || undefined }),
    }),

  deleteConversation: (id: string) =>
    request<void>(`/api/conversations/${id}`, {
      method: "DELETE",
    }),

  listMessages: (conversationId: string, limit = 50) =>
    request<Message[]>(`/api/conversations/${conversationId}/messages?limit=${limit}`),

  analyzeTechReport: (prompt: string, tier?: ModelTier, model?: string) =>
    request<TechReport>("/api/analyze", {
      method: "POST",
      body: JSON.stringify({ prompt, tier, model }),
    }),

  parseDocument: (filename: string, content: string) =>
    request<ParseDocumentResponse>("/api/documents/parse", {
      method: "POST",
      body: JSON.stringify({ filename, content }),
    }),

  parseDocumentsBatch: (files: Array<{ filename: string; content: string }>) =>
    request<ParseBatchResponse>("/api/documents/parse-batch", {
      method: "POST",
      body: JSON.stringify({ files }),
    }),

  chunkDocument: (payload: ChunkDocumentRequest) =>
    request<ChunkDocumentResponse>("/api/documents/chunk", {
      method: "POST",
      body: JSON.stringify(payload),
    }),
};

export interface StreamChatCallbacks {
  onDelta: (delta: string) => void;
  onDone: (messageId: string) => void;
  onError: (error: Error) => void;
}

export async function streamConversationChat(
  conversationId: string,
  prompt: string,
  options: {
    tier?: ModelTier;
    model?: string;
    signal?: AbortSignal;
  },
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
        tier: options.tier,
        model: options.model,
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
      const err = await response.json();
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

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      buffer += decoder.decode(value, { stream: true });
      const events = buffer.split("\n\n");
      buffer = events.pop() ?? "";

      for (const eventBlock of events) {
        if (!eventBlock.trim()) continue;

        let eventType = "message";
        let data = "";

        for (const line of eventBlock.split("\n")) {
          if (line.startsWith("event:")) {
            eventType = line.slice(6).trim();
          } else if (line.startsWith("data:")) {
            data = line.slice(5).trim();
          }
        }

        if (eventType === "delta") {
          try {
            const chunk = JSON.parse(data);
            callbacks.onDelta(typeof chunk === "string" ? chunk : String(chunk));
          } catch {
            callbacks.onDelta(data);
          }
        } else if (eventType === "done") {
          try {
            const payload = JSON.parse(data);
            callbacks.onDone(payload.message_id);
          } catch {
            callbacks.onDone("");
          }
        } else if (eventType === "error") {
          let errorMsg = "Streaming error occurred";
          try {
            const payload = JSON.parse(data);
            if (payload?.error?.message) errorMsg = payload.error.message;
          } catch {
            // ignore
          }
          callbacks.onError(new Error(errorMsg));
        }
      }
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
