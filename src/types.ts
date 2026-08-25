export type ProviderKind = "openai" | "anthropic" | "google" | "compatible";

export type Provider = {
  id: string;
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  model: string;
  hasKey: boolean;
  isDefault: boolean;
};

export type ProviderDraft = {
  id?: string;
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  model: string;
  apiKey?: string;
};

export type ChatMessage = {
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  modelContent?: string;
  kind?: "text" | "image";
  imagePath?: string;
  attachments?: ChatAttachment[];
};

export type ChatAttachment = {
  id: string;
  name: string;
  mimeType: string;
  kind: "image" | "text";
  dataUrl?: string;
  text?: string;
};

export type ImageGenerationConfig = {
  provider: "openai" | "compatible";
  baseUrl: string;
  model: string;
  hasKey: boolean;
};

export type GeneratedImage = {
  path: string;
  prompt: string;
};

export type EdgeState = {
  energy: number;
  active: boolean;
  colorMode: "rainbow" | "blue";
};
