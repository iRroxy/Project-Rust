export type ModelTier = "fast" | "reasoning";

export interface Conversation {
  id: string;
  title: string | null;
  created_at: string;
}

export interface Message {
  id: string;
  conversation_id: string;
  role: "system" | "user" | "assistant";
  content: string;
  token_count: number | null;
  created_at: string;
}

export interface ModelTiersInfo {
  fast: string;
  reasoning?: string;
}

export interface ModelsInfo {
  default_tier: ModelTier;
  tiers: ModelTiersInfo;
  models: string[];
}

export interface TechReport {
  summary: string;
  pros: string[];
  cons: string[];
  score: number;
}

export interface StreamDonePayload {
  message_id: string;
}

export interface ApiErrorResponse {
  error: {
    code: string;
    message: string;
  };
}
