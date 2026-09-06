import { emitTo, listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp, AudioLines, BrainCircuit, Check, ChevronUp, Copy, Droplets, EyeOff, LockKeyhole, MessageCircleMore, Minimize2, MonitorUp, MousePointer2, Power, RotateCcw, SendHorizontal, Settings2, SlidersHorizontal, Square, Waves, X } from "lucide-react";
import { CSSProperties, FormEvent, MouseEvent as ReactMouseEvent, PointerEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ChatMessage } from "../types";

type SpeechAlternative = { transcript: string; confidence?: number };
type SpeechResult = { readonly length: number; [index: number]: SpeechAlternative; isFinal: boolean };

type SpeechRecognitionLike = {
  continuous: boolean;
  interimResults: boolean;
  maxAlternatives: number;
  lang: string;
  start: () => void;
  stop: () => void;
  abort: () => void;
  phrases?: Array<{ phrase: string; boost: number }>;
  onresult: ((event: { resultIndex: number; results: ArrayLike<SpeechResult> }) => void) | null;
  onend: (() => void) | null;
  onerror: (() => void) | null;
};

type SpeechConstructor = new () => SpeechRecognitionLike;
type SpeechPhraseConstructor = new (phrase: string, boost?: number) => { phrase: string; boost: number };
type ListeningMode = "chat" | "dictation" | "realtime";

type VoiceResources = {
  stream: MediaStream;
  context?: AudioContext;
  animationFrame: number;
};

type RealtimeDictationCapture = {
  context: AudioContext;
  processor: AudioWorkletNode;
  source: MediaStreamAudioSourceNode;
  silentGain: GainNode;
  sessionId: string;
  sendQueue: Promise<void>;
  sampleRate: number;
};

type RealtimeDictationEvent = {
  sessionId: string;
  text?: string | null;
  fullText?: string | null;
  done: boolean;
  error?: string | null;
};

type DictationPhase = "idle" | "listening" | "finishing" | "recognizing" | "polishing" | "inserting" | "error";

// 灵动听写条：单行高度，文字从中间向两侧拉长，宽度封顶后内部横向滚动。
const dictationPillHeight = 48;
const dictationPillMinWidth = 168;
const dictationPillMaxWidth = 720;
// 中间实时声波的 22 根细条：静默时收敛成小点，说话时按麦克风能量拉成波形。
const dictationWaveFactors = [0.4, 0.72, 0.5, 0.92, 0.62, 1, 0.7, 0.86, 0.52, 0.78, 0.96, 0.6, 0.82, 0.46, 0.9, 0.66, 1, 0.72, 0.56, 0.88, 0.5, 0.64];

function estimateDictationTextWidth(text: string) {
  let width = 0;
  for (const char of text) {
    width += /[\u2e80-\u9fff\uac00-\ud7af\uff00-\uffef\u3000-\u303f]/.test(char) ? 15 : 8;
  }
  return width;
}

type ContextMenuState = { x: number; above: boolean; origin?: WindowPosition } | null;
type CapsuleSize = "compact" | "standard" | "wide";
type EdgeColorMode = "rainbow" | "blue";
type DictationRecognitionMode = "native" | "ai";
type InlineMessage = Pick<ChatMessage, "id" | "role" | "content">;
type StreamPayload = { requestId: string; delta?: string; done: boolean; error?: string };
type ScreenTranslationStatus = { active: boolean; loading: boolean; error?: string | null };
type WindowPosition = { x: number; y: number };
type WorkArea = { x: number; y: number; width: number; height: number };
type ContextMenuPlacement = { above: boolean; targetY?: number };
type ComputerAction = {
  action: string;
  x?: number | null;
  y?: number | null;
  endX?: number | null;
  endY?: number | null;
  text?: string | null;
  target?: string | null;
  uiTarget?: string | null;
  uiAction?: string | null;
  planStepId?: string | null;
  stepComplete?: boolean;
  desktopTarget?: string | null;
  windowTarget?: string | null;
  key?: string | null;
  keys?: string[] | null;
  amount?: number | null;
  duration?: number | null;
  description?: string | null;
  confidence?: number | null;
  expectedOutcome?: string | null;
  retryEvidence?: string | null;
  finalEvidence?: string | null;
  requiresConfirmation: boolean;
  message?: string | null;
  observationSequence?: number | null;
};
type ComputerConfirmation = { action: ComputerAction; description: string };
type McpDecision = {
  active: boolean;
  phase: "observe" | "decide" | "act" | "verify" | "complete";
  summary: string;
  detail?: string | null;
  nextAction?: string | null;
};
type ComputerTaskPlan = {
  summary: string;
  finalOutcome?: string;
  steps: Array<{ id?: string; title: string; expectedOutcome: string; completionHint?: string; maxAttempts?: number }>;
};

const contextMenuHeight = 760;
const menuAboveOffset = contextMenuHeight - 72;
const mcpPhaseLabels: Record<McpDecision["phase"], string> = {
  observe: "观察",
  decide: "判断",
  act: "执行",
  verify: "复查",
  complete: "完成",
};

type Appearance = {
  opacity: number;
  glassEnabled: boolean;
  edgeEnabled: boolean;
  autoSend: boolean;
  dictationCorrection: boolean;
  dictationMemory: boolean;
  dictationEdgeEnabled: boolean;
  dictationMode: DictationRecognitionMode;
  aiDictationPolish: boolean;
  realtimeEdgeEnabled: boolean;
  clickThrough: boolean;
  positionLocked: boolean;
  size: CapsuleSize;
  edgeColor: EdgeColorMode;
};

const sizeWidths: Record<CapsuleSize, number> = {
  compact: 360,
  standard: 420,
  wide: 500,
};

const sizeLabels: Record<CapsuleSize, string> = {
  compact: "紧凑",
  standard: "标准",
  wide: "宽阔",
};

const defaultAppearance: Appearance = {
  opacity: 78,
  glassEnabled: true,
  edgeEnabled: true,
  autoSend: true,
  dictationCorrection: true,
  dictationMemory: true,
  dictationEdgeEnabled: true,
  dictationMode: "ai",
  aiDictationPolish: true,
  realtimeEdgeEnabled: true,
  clickThrough: false,
  positionLocked: false,
  size: "standard",
  edgeColor: "rainbow",
};

function browserSpeechConstructor() {
  const candidate = window as Window & {
    SpeechRecognition?: SpeechConstructor;
    webkitSpeechRecognition?: SpeechConstructor;
  };
  return candidate.SpeechRecognition ?? candidate.webkitSpeechRecognition;
}

function speechPhraseConstructor() {
  return (window as Window & { SpeechRecognitionPhrase?: SpeechPhraseConstructor }).SpeechRecognitionPhrase;
}

function readDictationVocabulary() {
  return (window.localStorage.getItem("kero-dictation-vocabulary") ?? "")
    .split(/[\n,，、;；]+/)
    .map((term) => term.trim())
    .filter((term, index, terms) => term.length >= 2 && term.length <= 40 && terms.indexOf(term) === index)
    .slice(0, 80);
}

const dictationMemoryKey = "kero-dictation-memory";
const maxDictationMemoryEntries = 6;
const maxDictationMemoryEntryLength = 360;

function readDictationMemory() {
  try {
    const stored = JSON.parse(window.localStorage.getItem(dictationMemoryKey) ?? "[]");
    if (!Array.isArray(stored)) return [];
    return stored
      .filter((item): item is string => typeof item === "string")
      .map((item) => item.trim())
      .filter(Boolean)
      .slice(-maxDictationMemoryEntries);
  } catch {
    return [];
  }
}

function rememberDictation(text: string) {
  const normalized = text.trim().slice(0, maxDictationMemoryEntryLength);
  if (!normalized) return;
  const memory = readDictationMemory().filter((item) => item !== normalized);
  memory.push(normalized);
  window.localStorage.setItem(dictationMemoryKey, JSON.stringify(memory.slice(-maxDictationMemoryEntries)));
}

function appendSpeechSegment(current: string, incoming: string) {
  const base = current.trim();
  const segment = incoming.trim();
  if (!segment) return base;
  if (!base) return segment;
  if (base.endsWith(segment)) return base;
  if (segment.startsWith(base)) return segment;
  const overlapLimit = Math.min(base.length, segment.length, 48);
  for (let length = overlapLimit; length >= 2; length -= 1) {
    if (base.slice(-length) === segment.slice(0, length)) return base + segment.slice(length);
  }
  const needsSpace = /[A-Za-z0-9]$/.test(base) && /^[A-Za-z0-9]/.test(segment);
  return `${base}${needsSpace ? " " : ""}${segment}`;
}

function chooseSpeechAlternative(result: SpeechResult, vocabulary: string[]) {
  let best = result[0]?.transcript ?? "";
  let bestScore = result[0]?.confidence ?? 0;
  for (let index = 0; index < Math.min(result.length, 3); index += 1) {
    const alternative = result[index];
    if (!alternative?.transcript) continue;
    const vocabularyHits = vocabulary.reduce((count, term) => count + (alternative.transcript.includes(term) ? 1 : 0), 0);
    const score = (alternative.confidence ?? 0) + vocabularyHits * 0.22;
    if (score > bestScore) {
      best = alternative.transcript;
      bestScore = score;
    }
  }
  return best;
}

async function blobToBase64(blob: Blob) {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  const chunkSize = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return btoa(binary);
}

function dictationRecorderMimeType() {
  const candidates = ["audio/webm;codecs=opus", "audio/webm", "audio/mp4"];
  return candidates.find((mimeType) => MediaRecorder.isTypeSupported(mimeType)) ?? "";
}

function isStreamDictationModelName(model: string) {
  const value = model.trim().toLowerCase();
  return value.includes("realtime") || value.includes("streaming");
}

const KERO_PCM_WORKLET_SRC = `
class KeroPcmCapture extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const outputRate = (options && options.processorOptions && options.processorOptions.outputRate) || 16000;
    this.ratio = sampleRate / outputRate;
    this.pending = [];
    this.output = [];
    this.target = Math.max(320, Math.floor(outputRate / 10));
  }
  process(inputs) {
    const input = inputs[0];
    const channel = input && input[0];
    if (channel && channel.length > 0) {
      for (let index = 0; index < channel.length; index += 1) this.pending.push(channel[index]);
      const ratio = this.ratio;
      const outputCount = Math.floor(this.pending.length / ratio);
      const consumed = Math.floor(outputCount * ratio);
      for (let index = 0; index < outputCount; index += 1) {
        const start = Math.floor(index * ratio);
        const end = Math.min(this.pending.length, Math.floor((index + 1) * ratio));
        let total = 0;
        for (let cursor = start; cursor < Math.max(start + 1, end); cursor += 1) total += this.pending[cursor];
        const value = Math.max(-1, Math.min(1, total / Math.max(1, end - start)));
        this.output.push(value < 0 ? value * 0x8000 : value * 0x7fff);
      }
      if (consumed > 0) this.pending = this.pending.slice(consumed);
      if (this.output.length >= this.target) {
        const buffer = new Int16Array(this.output);
        this.output = [];
        this.port.postMessage(buffer.buffer, [buffer.buffer]);
      }
    }
    return true;
  }
}
registerProcessor("kero-pcm-capture", KeroPcmCapture);
`;

async function createPcmWorkletNode(context: AudioContext, outputRate: number) {
  const moduleUrl = URL.createObjectURL(new Blob([KERO_PCM_WORKLET_SRC], { type: "application/javascript" }));
  try {
    await context.audioWorklet.addModule(moduleUrl);
  } finally {
    URL.revokeObjectURL(moduleUrl);
  }
  return new AudioWorkletNode(context, "kero-pcm-capture", {
    numberOfInputs: 1,
    numberOfOutputs: 1,
    outputChannelCount: [1],
    processorOptions: { outputRate },
  });
}

function pcmToBase64(chunks: Int16Array[]) {
  const sampleCount = chunks.reduce((total, chunk) => total + chunk.length, 0);
  const bytes = new Uint8Array(sampleCount * 2);
  let offset = 0;
  for (const chunk of chunks) {
    for (const sample of chunk) {
      bytes[offset++] = sample & 0xff;
      bytes[offset++] = (sample >> 8) & 0xff;
    }
  }
  let binary = "";
  const chunkSize = 0x8000;
  for (let index = 0; index < bytes.length; index += chunkSize) binary += String.fromCharCode(...bytes.subarray(index, index + chunkSize));
  return btoa(binary);
}

function normalizedAppearance(value: Partial<Appearance>): Appearance {
  const opacity = Number(value.opacity);
  return {
    opacity: Number.isFinite(opacity) ? Math.min(100, Math.max(25, opacity)) : defaultAppearance.opacity,
    glassEnabled: value.glassEnabled !== false,
    edgeEnabled: value.edgeEnabled !== false,
    autoSend: value.autoSend !== false,
  dictationCorrection: value.dictationCorrection !== false,
  dictationMemory: value.dictationMemory !== false,
  dictationEdgeEnabled: value.dictationEdgeEnabled !== false,
  dictationMode: value.dictationMode === "native" ? "native" : "ai",
  aiDictationPolish: value.aiDictationPolish !== false,
    realtimeEdgeEnabled: value.realtimeEdgeEnabled !== false,
    clickThrough: value.clickThrough === true,
    positionLocked: value.positionLocked === true,
    size: value.size === "compact" || value.size === "wide" ? value.size : "standard",
    edgeColor: value.edgeColor === "blue" ? "blue" : "rainbow",
  };
}

function readAppearance(): Appearance {
  try {
    const stored = window.localStorage.getItem("kero-appearance");
    if (stored) {
      const appearance = normalizedAppearance(JSON.parse(stored) as Partial<Appearance>);
      if (appearance.opacity === 88 && window.localStorage.getItem("kero-opacity-migrated") !== "true") {
        appearance.opacity = defaultAppearance.opacity;
        saveAppearance(appearance);
        window.localStorage.setItem("kero-opacity-migrated", "true");
      }
      return appearance;
    }
  } catch {
    // Fall through to settings saved by older Kero builds.
  }
  return normalizedAppearance({
    ...defaultAppearance,
    opacity: Number(window.localStorage.getItem("kero-capsule-opacity")) || defaultAppearance.opacity,
    glassEnabled: window.localStorage.getItem("kero-glass-enabled") !== "false",
  });
}

function saveAppearance(appearance: Appearance) {
  window.localStorage.setItem("kero-appearance", JSON.stringify(appearance));
  window.localStorage.setItem("kero-capsule-opacity", String(appearance.opacity));
  window.localStorage.setItem("kero-glass-enabled", String(appearance.glassEnabled));
}

function createInlineMessage(role: InlineMessage["role"], content: string): InlineMessage {
  return { id: crypto.randomUUID(), role, content };
}

function unusableRealtimeReply(content: string) {
  const compact = content.replace(/\s/g, "");
  return compact.length > 420 && new Set(Array.from(compact)).size < Math.min(40, compact.length * 0.11);
}

function mergeStreamText(current: string, incoming: string) {
  if (!current || !incoming) return current + incoming;
  // Some compatible providers send the accumulated reply in every SSE event.
  if (incoming.startsWith(current)) return incoming;
  if (current.endsWith(incoming)) return current;
  const limit = Math.min(current.length, incoming.length, 512);
  for (let length = limit; length > 0; length -= 1) {
    if (current.slice(-length) === incoming.slice(0, length)) return current + incoming.slice(length);
  }
  return current + incoming;
}

function screenTranslationCommand(text: string): "start" | "stop" | null {
  const compact = text.trim().replace(/[\s，。！？、,.!?]/g, "");
  if (/^(关闭|停止|结束|取消)全屏翻译$/.test(compact)) return "stop";
  if (/^(请|帮我|给我|开始|开启|启动|进行|一下|请帮我|帮我开启|帮我进行)*全屏翻译(一下)?$/.test(compact)) return "start";
  return null;
}

const computerControlCallMarker = "[[KERO_COMPUTER_CONTROL]]";

async function shouldRunComputerTask(text: string) {
  if (!text.trim() || text.length > 1200) return false;
  return invoke<boolean>("computer_control_intent", { text }).catch(() => false);
}

export function Capsule() {
  const [draft, setDraft] = useState("");
  const [listening, setListening] = useState(false);
  const [voiceEnergy, setVoiceEnergy] = useState(0);
  const [isDictating, setIsDictating] = useState(false);
  const [dictationStatus, setDictationStatus] = useState("");
  const [dictationPhase, setDictationPhase] = useState<DictationPhase>("idle");
  const [dictationTranscript, setDictationTranscript] = useState("");
  const [dictationError, setDictationError] = useState("");
  const [realtimeActive, setRealtimeActive] = useState(false);
  const [realtimePermissionOpen, setRealtimePermissionOpen] = useState(false);
  const [contextMenu, setContextMenu] = useState<ContextMenuState>(null);
  const [appearance, setAppearance] = useState<Appearance>(readAppearance);
  const [messages, setMessages] = useState<InlineMessage[]>([]);
  const [isStreaming, setIsStreaming] = useState(false);
  const [computerRunning, setComputerRunning] = useState(false);
  const [computerStatus, setComputerStatus] = useState("");
  const [mcpActive, setMcpActive] = useState(false);
  const [mcpDecision, setMcpDecision] = useState<McpDecision | null>(null);
  const [computerConfirmation, setComputerConfirmation] = useState<ComputerConfirmation | null>(null);
  const [screenTranslationActive, setScreenTranslationActive] = useState(false);
  const [screenTranslationReady, setScreenTranslationReady] = useState(false);
  const [conversationOpen, setConversationOpen] = useState(false);
  const [collapseTarget, setCollapseTarget] = useState<WindowPosition | null>(null);
  const voiceResources = useRef<VoiceResources | null>(null);
  const realtimeActiveRef = useRef(false);
  const screenTranslationActiveRef = useRef(false);
  const isStreamingRef = useRef(false);
  const dictationHeldRef = useRef(false);
  const dictationOpenedFromTray = useRef(false);
  const dictationSessionId = useRef(0);
  const trayDictationSessionId = useRef<number | null>(null);
  const dictationStopTimer = useRef<number | null>(null);
  const dictationLastResultAt = useRef(0);
  const dictationRecorder = useRef<MediaRecorder | null>(null);
  const dictationAudioChunks = useRef<Blob[]>([]);
  const realtimeDictationCapture = useRef<RealtimeDictationCapture | null>(null);
  const realtimeDictationSessionId = useRef<string | null>(null);
  const realtimeDictationText = useRef("");
  const realtimeDictationWritten = useRef("");
  const realtimeDictationInputFailed = useRef(false);
  const realtimeDictationInsertQueue = useRef(Promise.resolve());
  const realtimeReplaceTimer = useRef<number | null>(null);
  const realtimePendingWritten = useRef("");
  const dictationStopRequested = useRef(false);
  const dictationFlowId = useRef(0);
  const dictationFinalTextRef = useRef("");
  const dictationTranscriptRef = useRef<HTMLDivElement | null>(null);
  const realtimeFallbackRecorder = useRef<MediaRecorder | null>(null);
  const realtimeFallbackChunks = useRef<Blob[]>([]);
  const dictationGrowOrigin = useRef<WindowPosition | null>(null);
  const dictationStartedHidden = useRef(false);
  const stopListeningRef = useRef<((submit?: boolean) => void) | null>(null);
  const startListeningRef = useRef<((mode?: ListeningMode) => Promise<void>) | null>(null);
  const recognition = useRef<SpeechRecognitionLike | null>(null);
  const listeningRef = useRef(false);
  const listeningMode = useRef<ListeningMode>("chat");
  const transcriptRef = useRef("");
  const finalTranscriptRef = useRef("");
  const capsuleRef = useRef<HTMLElement>(null);
  const dragOrigin = useRef<{ x: number; y: number } | null>(null);
  const activeRequestId = useRef<string | undefined>(undefined);
  const realtimeRequestId = useRef<string | undefined>(undefined);
  const realtimeOutputRejected = useRef(false);
  const assistantMessageId = useRef<string | undefined>(undefined);
  const streamContentRef = useRef("");
  const streamFallbackTaskRef = useRef("");
  const conversationOrigin = useRef<WindowPosition | null>(null);
  const computerCancelled = useRef(false);
  const computerConfirmationResolver = useRef<((approved: boolean) => void) | null>(null);
  const computerAssistantId = useRef<string | undefined>(undefined);
  const computerRunningRef = useRef(false);
  const computerTaskRef = useRef("");
  const computerTaskFromRealtimeRef = useRef(false);
  const computerRevisionRef = useRef<{ task: string; fromRealtime: boolean } | null>(null);
  const computerMarkPendingRef = useRef(false);
  const computerMarkSequenceRef = useRef(0);
  const runComputerTaskRef = useRef<((task: string, fromRealtime?: boolean, addUserMessage?: boolean) => Promise<void>) | null>(null);
  const mcpCompletionTimer = useRef<number | null>(null);
  const translationAssistantId = useRef<string | undefined>(undefined);

  isStreamingRef.current = isStreaming;
  screenTranslationActiveRef.current = screenTranslationActive;

  useEffect(() => {
    let cancelled = false;
    const frame = window.requestAnimationFrame(() => {
      void invoke<boolean>("should_start_hidden")
        .catch(() => false)
        .then((startHidden) => {
          if (!startHidden && !cancelled) void invoke("show_main_passive").catch(console.error);
        });
    });
    return () => {
      cancelled = true;
      window.cancelAnimationFrame(frame);
    };
  }, []);

  const conversationVisible = conversationOpen && (messages.length > 0 || mcpActive);
  const menuOpen = contextMenu !== null;
  const dictationProcessing = Boolean(dictationStatus);
  const currentWidth = sizeWidths[appearance.size];
  const dictationPanel = isDictating || dictationPhase !== "idle";
  // 文字从中间向两侧拉长；到达宽度上限后窗口不再变宽，转写区内部横向滚动。
  const dictationWidth = !dictationPanel
    ? currentWidth
    : dictationPhase === "error"
      ? Math.min(dictationPillMaxWidth, 480)
      : Math.min(
          dictationPillMaxWidth,
          Math.max(dictationPillMinWidth, 104 + (dictationTranscript ? estimateDictationTextWidth(dictationTranscript) : 0)),
        );
  const recentMessages = useMemo(() => messages.slice(-2), [messages]);
  const inlineReplyLength = useMemo(() => Math.max(
    mcpDecision ? mcpDecision.summary.length + (mcpDecision.detail?.length ?? 0) + (mcpDecision.nextAction?.length ?? 0) : 0,
    ...recentMessages.filter((message) => message.role === "assistant").map((message) => message.content.length),
  ), [mcpDecision, recentMessages]);
  const inlineHeight = Math.min(470, Math.max(250, 250 + Math.ceil(Math.max(0, inlineReplyLength - 420) / 120) * 18 + (computerConfirmation ? 82 : 0)));

  const emitEdge = useCallback((active: boolean, energy = 0) => {
    void emitTo("edge", "kero:edge-state", { active, energy, colorMode: appearance.edgeColor });
  }, [appearance.edgeColor]);

  const beginEdge = useCallback(async (energy = 0.16) => {
    if (!appearance.edgeEnabled) return;
    await invoke("activate_assistant");
    emitEdge(true, energy);
  }, [appearance.edgeEnabled, emitEdge]);

  const endEdge = useCallback(() => {
    emitEdge(false, 0);
    window.setTimeout(() => {
      if (!screenTranslationActiveRef.current) void invoke("hide_edge");
    }, 420);
  }, [emitEdge]);

  const hideAfterTrayDictation = useCallback(() => {
    const sessionId = trayDictationSessionId.current;
    if (!dictationOpenedFromTray.current || sessionId === null) return;
    dictationOpenedFromTray.current = false;
    trayDictationSessionId.current = null;
    window.setTimeout(() => {
      // Do not let an older recognition request hide a newly started one.
      if (dictationSessionId.current !== sessionId || listeningRef.current || listeningMode.current === "dictation") return;
      void invoke("hide_window", { label: "main" });
    }, 180);
  }, []);

  const releaseVoice = useCallback(() => {
    const recorder = dictationRecorder.current;
    if (recorder) {
      recorder.ondataavailable = null;
      recorder.onstop = null;
      if (recorder.state !== "inactive") {
        try { recorder.stop(); } catch { /* Recorder is already closing. */ }
      }
      dictationRecorder.current = null;
      dictationAudioChunks.current = [];
    }
    const fallbackRecorder = realtimeFallbackRecorder.current;
    if (fallbackRecorder) {
      fallbackRecorder.ondataavailable = null;
      fallbackRecorder.onstop = null;
      if (fallbackRecorder.state !== "inactive") {
        try { fallbackRecorder.stop(); } catch { /* Recorder is already closing. */ }
      }
      realtimeFallbackRecorder.current = null;
      realtimeFallbackChunks.current = [];
    }
    const realtimeCapture = realtimeDictationCapture.current;
    if (realtimeCapture) {
      realtimeCapture.processor.disconnect();
      realtimeCapture.source.disconnect();
      realtimeCapture.silentGain.disconnect();
      void realtimeCapture.context.close();
      realtimeDictationCapture.current = null;
    }
    const resources = voiceResources.current;
    if (resources) {
      cancelAnimationFrame(resources.animationFrame);
      resources.stream.getTracks().forEach((track) => track.stop());
      void resources.context?.close();
      voiceResources.current = null;
    }
    if (recognition.current) {
      recognition.current.onend = null;
      recognition.current.abort();
      recognition.current = null;
    }
    setVoiceEnergy(0);
  }, []);

  // 取出并停掉流式会话并行录制的兜底音频；成功路径丢弃，失败路径用于整段识别降级。
  const takeRealtimeFallbackBlob = useCallback(() => {
    const recorder = realtimeFallbackRecorder.current;
    realtimeFallbackRecorder.current = null;
    if (!recorder) return null;
    const chunks = realtimeFallbackChunks.current;
    realtimeFallbackChunks.current = [];
    recorder.ondataavailable = null;
    recorder.onstop = null;
    if (recorder.state !== "inactive") {
      try { recorder.stop(); } catch { /* Recorder is already closing. */ }
    }
    if (!chunks.length) return null;
    return new Blob(chunks, { type: recorder.mimeType || dictationRecorderMimeType() });
  }, []);

  // 把最新流式文本打入目标输入框；Rust 侧做差量替换（只回退重打有差异的尾巴）。
  const applyRealtimeReplace = useCallback(() => {
    const incoming = realtimePendingWritten.current;
    if (!incoming || dictationStopRequested.current || incoming === realtimeDictationWritten.current) return;
    const previousWritten = realtimeDictationWritten.current;
    realtimeDictationWritten.current = incoming;
    realtimeDictationInsertQueue.current = realtimeDictationInsertQueue.current
      .then(() => invoke("replace_realtime_dictation_text", { previous: previousWritten, text: incoming }).then(() => undefined))
      .catch(() => {
        realtimeDictationWritten.current = previousWritten;
        realtimeDictationInputFailed.current = true;
      });
  }, []);

  const clearRealtimeReplaceTimer = useCallback(() => {
    if (realtimeReplaceTimer.current !== null) {
      window.clearTimeout(realtimeReplaceTimer.current);
      realtimeReplaceTimer.current = null;
    }
  }, []);

  const closeDictationPanel = useCallback(() => {
    takeRealtimeFallbackBlob();
    setDictationPhase("idle");
    setDictationTranscript("");
    setDictationError("");
    dictationFinalTextRef.current = "";
    realtimeDictationInputFailed.current = false;
    void invoke("clear_dictation_focus_target");
    hideAfterTrayDictation();
  }, [hideAfterTrayDictation, takeRealtimeFallbackBlob]);

  const cancelDictation = useCallback(() => {
    dictationFlowId.current += 1;
    dictationHeldRef.current = false;
    const span = realtimeDictationWritten.current;
    realtimeDictationWritten.current = "";
    const sessionId = realtimeDictationSessionId.current;
    realtimeDictationSessionId.current = null;
    realtimeDictationInsertQueue.current = realtimeDictationInsertQueue.current.then(async () => {
      if (span) {
        // 已经实时打入目标输入框的文字随取消一起清掉。
        await invoke("replace_realtime_dictation_text", { previous: span, text: "" }).catch(() => undefined);
      }
    });
    clearRealtimeReplaceTimer();
    releaseVoice();
    if (sessionId) void invoke("finish_realtime_dictation", { sessionId }).catch(() => undefined);
    listeningRef.current = false;
    listeningMode.current = "chat";
    setListening(false);
    setIsDictating(false);
    closeDictationPanel();
  }, [clearRealtimeReplaceTimer, closeDictationPanel, releaseVoice]);

  const retryDictationInsert = useCallback(() => {
    const text = dictationFinalTextRef.current.trim();
    if (!text) {
      closeDictationPanel();
      return;
    }
    const span = realtimeDictationWritten.current;
    setDictationPhase("inserting");
    realtimeDictationInsertQueue.current = realtimeDictationInsertQueue.current.then(async () => {
      try {
        if (span) {
          await invoke("replace_realtime_dictation_text", { previous: span, text });
        } else {
          await invoke("insert_text_to_active", { text });
        }
        realtimeDictationWritten.current = "";
        closeDictationPanel();
      } catch (error) {
        setDictationError(error instanceof Error ? error.message : "无法写入原输入框，请重试或复制内容");
        setDictationPhase("error");
      }
    });
  }, [closeDictationPanel]);

  const copyDictationText = useCallback(async () => {
    const text = dictationFinalTextRef.current.trim() || dictationTranscript.trim();
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      closeDictationPanel();
    } catch {
      setDictationError("复制失败，请重试");
      setDictationPhase("error");
    }
  }, [closeDictationPanel, dictationTranscript]);

  useEffect(() => {
    const node = dictationTranscriptRef.current;
    if (node) node.scrollLeft = node.scrollWidth;
  }, [dictationTranscript, dictationPhase]);

  const closeContextMenu = useCallback(() => {
    if (contextMenu?.above && contextMenu.origin) {
      void invoke("set_main_position", {
        x: contextMenu.origin.x,
        y: contextMenu.origin.y,
        updateLockedPosition: false,
      }).finally(() => setContextMenu(null));
      return;
    }
    setContextMenu(null);
  }, [contextMenu]);

  const updateAppearance = useCallback((patch: Partial<Appearance>, notifySettings = true) => {
    setAppearance((current) => {
      const next = normalizedAppearance({ ...current, ...patch });
      saveAppearance(next);
      if (notifySettings) void emitTo("settings", "kero:appearance-changed", next);
      return next;
    });
  }, []);

  const updateComputerMessage = useCallback((content: string) => {
    const id = computerAssistantId.current;
    if (!id) return;
    setMessages((current) => current.map((message) => message.id === id ? { ...message, content } : message));
  }, []);

  const cancelComputerTask = useCallback(() => {
    computerCancelled.current = true;
    void invoke("stop_computer_control");
    void emitTo("edge", "kero:computer-mark", { active: false });
    computerConfirmationResolver.current?.(false);
    computerConfirmationResolver.current = null;
    setComputerConfirmation(null);
    setComputerStatus("电脑操控已停止");
    updateComputerMessage("电脑操控已停止。");
  }, [updateComputerMessage]);

  const waitForComputerConfirmation = useCallback((action: ComputerAction) => new Promise<boolean>((resolve) => {
    computerConfirmationResolver.current = resolve;
    setComputerConfirmation({ action, description: action.description || "确认执行这项操作？" });
  }), []);

  const runComputerTask = useCallback(async (task: string, fromRealtime = false, addUserMessage = true) => {
    if (computerRunningRef.current || isStreamingRef.current) return;
    releaseVoice();
    listeningRef.current = false;
    setListening(false);
    setDraft("");
    closeContextMenu();
    const userMessage = createInlineMessage("user", task);
    const assistantMessage = createInlineMessage("assistant", "正在准备电脑操控...");
    computerAssistantId.current = assistantMessage.id;
    setMessages((current) => [...current, ...(addUserMessage ? [userMessage] : []), assistantMessage].slice(-14));
    setConversationOpen(true);
    computerRunningRef.current = true;
    isStreamingRef.current = true;
    computerTaskRef.current = task;
    computerTaskFromRealtimeRef.current = fromRealtime;
    computerMarkPendingRef.current = false;
    setComputerRunning(true);
    setIsStreaming(true);
    setComputerStatus("正在观察屏幕");
    computerCancelled.current = false;
    const history: string[] = [];
    let recoverableFailures = 0;
    // A missed click can be worth retrying, but an unlimited retry loop is
    // indistinguishable from a stuck agent. Keep the limit per live target.
    const pointerAttempts = new Map<string, number>();
    const maxPointerAttempts = 10;
    const maxTaskSteps = 80;
    let computerEdgePulse: number | undefined;
    try {
      await invoke("start_computer_control");
      await invoke("activate_assistant");
      // The edge WebView can need a frame after being restored from its hidden state.
      // Keep sending the control state so that its first listener cannot miss the effect.
      await new Promise((resolve) => window.setTimeout(resolve, 50));
      emitEdge(true, 0.72);
      computerEdgePulse = window.setInterval(() => emitEdge(true, 0.72), 420);
      const riskMode = await invoke<boolean>("get_computer_control_risk_mode").catch(() => false);
      const executionTask = `${task}\n\n[ADAPTIVE EXECUTION] Work directly from the newest screenshot and execution history. Do not create a fixed plan. Re-evaluate the task after every action, adapt immediately when the visible result differs from the expectation, and return done only when the user's requested visible result is present.`;
      setComputerStatus("正在观察屏幕");
      updateComputerMessage("正在观察当前屏幕并直接执行任务...");
      for (let step = 0; ; step += 1) {
        if (step >= maxTaskSteps) {
          throw new Error("已达到本次任务的操作上限。请补充目标或使用 K 标记指出需要修正的位置。");
        }
        if (computerCancelled.current) throw new Error("电脑操控已停止。");
        const action = await invoke<ComputerAction>("computer_next_action", { task: executionTask, history });
        const staleAfterMark = typeof action.observationSequence === "number"
          && action.observationSequence < computerMarkSequenceRef.current;
        if (staleAfterMark) {
          computerMarkPendingRef.current = false;
          history.push(`${step + 1}. USER MARK OVERRIDE: a K mark arrived after this decision began, so the stale action=${action.action} was discarded without execution. Capture a new screenshot containing the mark and choose a corrected action.`);
          setComputerStatus("正在依据标记重新观察");
          updateComputerMessage("已收到标记，已取消旧决策并重新观察标记区域");
          continue;
        }
        if (computerMarkPendingRef.current) {
          computerMarkPendingRef.current = false;
          history.push(`${step + 1}. USER MARK INCLUDED: this decision was generated from the screenshot containing the latest K mark. Execute only the corrected action, then verify the result.`);
          setComputerStatus("正在依据标记纠正");
          updateComputerMessage("已看到标记，正在根据标记区域纠正操作");
        }
        if (action.action === "done") {
          if (history.length > 0 && !action.finalEvidence?.trim()) {
            history.push(`${step + 1}. FINAL VERIFICATION REQUIRED: the model reported done without finalEvidence. Re-observe the newest screenshot, compare it with the user's requested visible result, and only return done with finalEvidence that names what is visibly complete. If the result is not visible, continue with a corrective action.`);
            setComputerStatus("正在复查最终结果");
            updateComputerMessage("正在复查最终画面，确认结果确实已经完成");
            continue;
          }
          const result = action.message || "电脑操作已经完成。";
          setComputerStatus("任务完成");
          updateComputerMessage(result);
          return;
        }
        if (action.action === "step_complete" || action.stepComplete) {
          history.push(`${step + 1}. ADAPTIVE REVIEW: ${action.message || "the current visible state was verified"}. Continue from the newest screen.`);
          setComputerStatus("正在继续观察");
          continue;
        }
        const description = action.description || `正在执行 ${action.action}`;
        const isPointerAction = ["click", "double_click", "right_click", "long_press", "drag"].includes(action.action);
        const pointerSignature = isPointerAction
          ? action.uiTarget
            ? `ui:${action.uiTarget}`
            : action.desktopTarget
              ? `desktop:${action.desktopTarget.toLocaleLowerCase()}`
              : typeof action.x === "number" && typeof action.y === "number"
                ? `region:${action.action}:${Math.round(action.x * 40)}:${Math.round(action.y * 40)}:${typeof action.endX === "number" ? Math.round(action.endX * 40) : ""}:${typeof action.endY === "number" ? Math.round(action.endY * 40) : ""}`
                : null
          : null;
        const pointerAttempt = pointerSignature ? pointerAttempts.get(pointerSignature) || 0 : 0;
        if (pointerSignature && pointerAttempt >= maxPointerAttempts) {
          history.push(`${step + 1}. RETRY LIMIT: blocked pointer action for ${pointerSignature}; it has already been attempted ${maxPointerAttempts} times in this task. Do not retry it again. Re-observe the current screen and choose a different target, a semantic control, or another method.`);
          setComputerStatus("同一目标重试已达上限，正在换一种方法");
          updateComputerMessage("同一目标已重试十次仍未生效，正在重新观察并改用其他操作");
          await new Promise((resolve) => window.setTimeout(resolve, 90));
          continue;
        }
        if (pointerSignature && pointerAttempt > 0 && !action.retryEvidence?.trim()) {
          history.push(`${step + 1}. RETRY EVIDENCE REQUIRED: the same pointer target ${pointerSignature} was already used ${pointerAttempt} time(s). The action was not executed. Re-observe the newest screenshot and either select another method or return the retry with retryEvidence describing exactly what visibly proves the previous attempt did not take effect.`);
          setComputerStatus("正在复查上一步是否生效");
          updateComputerMessage("正在复查上一步的结果，确认未生效后才会重试");
          await new Promise((resolve) => window.setTimeout(resolve, 90));
          continue;
        }
        if (pointerSignature && pointerAttempt > 0) {
          history.push(`${step + 1}. CONTROLLED RETRY ${pointerAttempt + 1}/${maxPointerAttempts}: retryEvidence=${action.retryEvidence?.trim()}. Execute once, then verify the expected visible outcome before considering any further retry.`);
        }
        if (isPointerAction && typeof action.confidence === "number" && action.confidence < 0.68) {
          recoverableFailures += 1;
          history.push(`${step + 1}. planning rejected action=${action.action}; target confidence=${action.confidence.toFixed(2)} is below 0.68. Re-observe the newest screenshot and use a distinct, clearly visible target or a non-destructive observation action.`);
          setComputerStatus("目标不够确定，正在重新观察");
          updateComputerMessage("目标不够确定，正在重新观察屏幕后再选择操作");
          await new Promise((resolve) => window.setTimeout(resolve, 90));
          continue;
        }
        setComputerStatus(description);
        updateComputerMessage(description);
        let approved = riskMode || !action.requiresConfirmation;
        if (!approved) {
          setComputerStatus("等待你的确认");
          updateComputerMessage(`等待确认：${description}`);
          approved = await waitForComputerConfirmation(action);
          computerConfirmationResolver.current = null;
          setComputerConfirmation(null);
          if (!approved) throw new Error("你取消了这项操作。");
        }
        try {
          const executionRoute = await invoke<string>("computer_execute_action", { action, confirmed: approved });
          if (executionRoute) history.push(`${step + 1}. EXECUTION ROUTE: ${executionRoute}`);
        } catch (error) {
          const detail = error instanceof Error ? error.message : String(error);
          if (detail.includes("USER_MARK_REOBSERVE_REQUIRED")) {
            computerMarkPendingRef.current = false;
            history.push(`${step + 1}. USER MARK OVERRIDE: the backend rejected a stale action because a newer K mark arrived. Re-observe before doing anything else.`);
            setComputerStatus("标记已更新，正在重新观察");
            updateComputerMessage("已阻止标记前的旧操作，正在重新读取屏幕");
            continue;
          }
          if (detail.includes("POINTER_RECOVERY_REQUIRED")) {
            const actionTarget = action.desktopTarget ? ` desktopTarget=${action.desktopTarget}` : "";
            const actionPoint = typeof action.x === "number" && typeof action.y === "number"
              ? ` point=(${action.x.toFixed(4)},${action.y.toFixed(4)})`
              : "";
            history.push(`${step + 1}. action=${action.action}${actionTarget}${actionPoint}; pointer drifted before execution. Re-observe the current screen and choose a corrected target; do not repeat this click blindly.`);
            setComputerStatus("鼠标位置偏移，正在重新观察并校正");
            updateComputerMessage("鼠标位置偏移，正在重新观察屏幕并校正目标");
            await new Promise((resolve) => window.setTimeout(resolve, 120));
            continue;
          }
          recoverableFailures += 1;
          history.push(`${step + 1}. execution failed action=${action.action}; error=${detail}. Expected outcome: ${action.expectedOutcome || "not provided"}. Re-observe the current screen, diagnose the failure, and choose a different method or target rather than repeating this action.`);
          setComputerStatus(`操作失败：${detail}`);
          updateComputerMessage(`操作失败：${detail}\n正在重新观察屏幕并调整下一步`);
          if (recoverableFailures >= 3) throw error;
          await new Promise((resolve) => window.setTimeout(resolve, 160));
          continue;
        }
        recoverableFailures = 0;
        if (pointerSignature) {
          pointerAttempts.set(pointerSignature, pointerAttempt + 1);
        } else if (["activate_window", "maximize_window"].includes(action.action)) {
          pointerAttempts.clear();
        }
        const actionTarget = action.desktopTarget
          ? ` desktopTarget=${action.desktopTarget}`
          : action.windowTarget
            ? ` windowTarget=${action.windowTarget}`
            : "";
        const actionPoint = typeof action.x === "number" && typeof action.y === "number"
          ? ` point=(${action.x.toFixed(4)},${action.y.toFixed(4)})`
          : "";
        history.push(`${step + 1}. action=${action.action}${actionTarget}${actionPoint}; ${description}; expectedOutcome=${action.expectedOutcome || "inspect the visible result before continuing"}. On the next screenshot, verify this expectation before selecting another action.`);
        const verificationDelay = ["open_app", "activate_window", "maximize_window"].includes(action.action)
          ? 900
          : ["click", "double_click", "right_click"].includes(action.action)
            ? 480
            : 260;
        await new Promise((resolve) => window.setTimeout(resolve, verificationDelay));
      }
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      setComputerStatus(detail);
      updateComputerMessage(detail);
    } finally {
      if (computerEdgePulse !== undefined) window.clearInterval(computerEdgePulse);
      const revision = computerRevisionRef.current;
      computerRevisionRef.current = null;
      void invoke("stop_computer_control");
      computerRunningRef.current = false;
      isStreamingRef.current = false;
      setComputerRunning(false);
      setIsStreaming(false);
      setComputerConfirmation(null);
      computerConfirmationResolver.current = null;
      emitEdge(false, 0);
      void emitTo("edge", "kero:computer-pointer", { x: 0.5, y: 0.5, active: false, click: false });
      void emitTo("edge", "kero:computer-mark", { active: false });
      window.setTimeout(() => {
        if (!screenTranslationActiveRef.current) void invoke("hide_edge");
      }, 420);
      if (revision) {
        window.setTimeout(() => void runComputerTaskRef.current?.(revision.task, revision.fromRealtime, false), 0);
      } else if (fromRealtime && realtimeActiveRef.current && !computerCancelled.current) {
        window.setTimeout(() => void startListeningRef.current?.("realtime"), 260);
      }
    }
  }, [closeContextMenu, emitEdge, releaseVoice, updateComputerMessage, waitForComputerConfirmation]);

  runComputerTaskRef.current = runComputerTask;

  const handleScreenTranslationCommand = useCallback(async (command: "start" | "stop", originalText: string) => {
    setDraft("");
    releaseVoice();
    listeningRef.current = false;
    setListening(false);
    closeContextMenu();
    if (command === "stop") {
      await invoke("stop_screen_translation").catch(console.error);
      setScreenTranslationActive(false);
      setScreenTranslationReady(false);
      setDictationStatus("");
      const assistant = createInlineMessage("assistant", "全屏翻译已关闭");
      setMessages((current) => [...current, createInlineMessage("user", originalText), assistant].slice(-14));
      setConversationOpen(true);
      return;
    }

    const pending = createInlineMessage("assistant", "翻译中...");
    translationAssistantId.current = pending.id;
    setMessages((current) => [...current, createInlineMessage("user", originalText), pending].slice(-14));
    setConversationOpen(true);
    setScreenTranslationActive(true);
    setScreenTranslationReady(false);
    setDictationStatus("翻译中");
    try {
      await invoke("start_screen_translation");
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      setScreenTranslationActive(false);
      setScreenTranslationReady(false);
      setDictationStatus("");
      setMessages((current) => current.map((message) => message.id === pending.id
        ? { ...message, content: `无法开启全屏翻译：${detail}` }
        : message));
    }
  }, [closeContextMenu, releaseVoice]);

  const sendPrompt = useCallback(async (text: string) => {
    const content = text.trim();
    if (!content) return;
    if (computerRunningRef.current) {
      const activeTask = computerRevisionRef.current?.task || computerTaskRef.current || "完成当前电脑任务";
      const revisedTask = `${activeTask}\n\n用户补充或修正：${content}`;
      computerRevisionRef.current = { task: revisedTask, fromRealtime: computerTaskFromRealtimeRef.current };
      setMessages((current) => [...current, createInlineMessage("user", content)].slice(-14));
      setDraft("");
      computerCancelled.current = true;
      void invoke("stop_computer_control");
      computerConfirmationResolver.current?.(false);
      computerConfirmationResolver.current = null;
      setComputerConfirmation(null);
      setComputerStatus("正在应用你的补充...");
      updateComputerMessage("正在根据你的补充重新规划操作...");
      return;
    }
    const translationCommand = screenTranslationCommand(content);
    if (translationCommand) {
      await handleScreenTranslationCommand(translationCommand, content);
      return;
    }
    if (isStreaming) return;
    setDraft("");
    setDictationStatus("正在理解请求");
    const shouldControlComputer = await shouldRunComputerTask(content);
    setDictationStatus("");
    if (shouldControlComputer) {
      await runComputerTask(content);
      return;
    }
    releaseVoice();
    listeningRef.current = false;
    setListening(false);
    closeContextMenu();

    const userMessage = createInlineMessage("user", content);
    const pendingReply = createInlineMessage("assistant", "");
    const requestId = crypto.randomUUID();
    const origin = await invoke<WindowPosition>("get_main_position").catch(() => null);
    if (origin) conversationOrigin.current = origin;
    activeRequestId.current = requestId;
    realtimeRequestId.current = undefined;
    realtimeOutputRejected.current = false;
    assistantMessageId.current = pendingReply.id;
    streamContentRef.current = "";
    streamFallbackTaskRef.current = content;
    setMessages((current) => [...current, userMessage, pendingReply].slice(-14));
    setConversationOpen(true);
    setIsStreaming(true);
    void beginEdge(0.17);

    try {
      const requestMessages = [...messages, userMessage]
        .filter((message) => message.content.trim())
        .slice(-12)
        .map(({ role, content: messageContent }) => ({ role, content: messageContent }));
      const webSearch = await invoke<boolean>("get_web_search_enabled").catch(() => false);
      await invoke("chat_stream", { requestId, request: { messages: requestMessages, webSearch } });
    } catch (error) {
      if (activeRequestId.current !== requestId) return;
      const detail = error instanceof Error ? error.message : String(error);
      setMessages((current) => current.map((message) => message.id === pendingReply.id
        ? { ...message, content: `无法完成这次请求：${detail}` }
        : message));
      setIsStreaming(false);
      activeRequestId.current = undefined;
      endEdge();
    }
  }, [beginEdge, closeContextMenu, endEdge, handleScreenTranslationCommand, isStreaming, messages, releaseVoice, runComputerTask]);

  const finishDictation = useCallback(async (text: string, useTextOptimization = true) => {
    const flowId = dictationFlowId.current;
    const raw = text.trim();
    if (!raw) {
      closeDictationPanel();
      return;
    }
    let optimized = raw;
    if (useTextOptimization) {
      setDictationPhase("polishing");
      const response = await invoke<string>("optimize_dictation", {
        text: raw,
        correctTypos: appearance.dictationCorrection,
        vocabulary: readDictationVocabulary().join("\n"),
        memory: appearance.dictationMemory ? readDictationMemory().join("\n") : undefined,
      }).catch(() => raw);
      if (dictationFlowId.current !== flowId) return;
      optimized = response.trim() || raw;
    }
    dictationFinalTextRef.current = optimized;
    if (appearance.dictationMemory) rememberDictation(optimized);
    setDictationPhase("inserting");
    try {
      await invoke("insert_text_to_active", { text: optimized });
      if (dictationFlowId.current !== flowId) return;
      closeDictationPanel();
    } catch (error) {
      if (dictationFlowId.current !== flowId) return;
      setDictationError(error instanceof Error ? error.message : "无法写入原输入框，可重试或复制内容");
      setDictationPhase("error");
    }
  }, [appearance.dictationCorrection, appearance.dictationMemory, closeDictationPanel]);

  const finishAiDictation = useCallback(async (blob: Blob) => {
    const flowId = dictationFlowId.current;
    setDictationPhase("recognizing");
    try {
      const audioBase64 = await blobToBase64(blob);
      const text = await invoke<string>("transcribe_dictation_audio", {
        audioBase64,
        mimeType: blob.type || "audio/webm",
      });
      if (dictationFlowId.current !== flowId) return;
      const transcript = text.trim();
      if (!transcript) throw new Error("AI 语音识别没有听到有效内容");
      transcriptRef.current = transcript;
      finalTranscriptRef.current = transcript;
      setDictationTranscript(transcript);
      releaseVoice();
      listeningRef.current = false;
      listeningMode.current = "chat";
      setListening(false);
      setIsDictating(false);
      await finishDictation(transcript, appearance.aiDictationPolish);
    } catch (error) {
      if (dictationFlowId.current !== flowId) return;
      void invoke("clear_dictation_focus_target");
      releaseVoice();
      listeningRef.current = false;
      listeningMode.current = "chat";
      setListening(false);
      setIsDictating(false);
      setDictationError(error instanceof Error ? error.message : "AI 语音识别失败");
      setDictationPhase("error");
    }
  }, [appearance.aiDictationPolish, finishDictation, releaseVoice]);

  const finishRealtimeAiDictation = useCallback(async (sessionId: string) => {
    setDictationPhase("finishing");
    try {
      await invoke("finish_realtime_dictation", { sessionId });
    } catch (error) {
      releaseVoice();
      listeningRef.current = false;
      listeningMode.current = "chat";
      setListening(false);
      setIsDictating(false);
      setDictationError(error instanceof Error ? error.message : "AI 实时语音识别失败");
      setDictationPhase("error");
    }
  }, [releaseVoice]);

  const captureScreenImage = useCallback(() => invoke<string>("capture_primary_screen"), []);

  const showRealtimeFailure = useCallback((message: string) => {
    setDraft("");
    setMessages((current) => [...current, createInlineMessage("assistant", `实时通话不可用：${message}`)].slice(-14));
    setConversationOpen(true);
  }, []);

  const stopRealtimeCall = useCallback(() => {
    realtimeActiveRef.current = false;
    setRealtimeActive(false);
    setRealtimePermissionOpen(false);
    if (listeningMode.current === "realtime") stopListeningRef.current?.(false);
    if (appearance.realtimeEdgeEnabled) endEdge();
  }, [appearance.realtimeEdgeEnabled, endEdge]);

  const sendRealtimePrompt = useCallback(async (text: string) => {
    const content = text.trim();
    if (!content || isStreamingRef.current || !realtimeActiveRef.current) return;
    const translationCommand = screenTranslationCommand(content);
    if (translationCommand) {
      await handleScreenTranslationCommand(translationCommand, content);
      return;
    }
    if (await shouldRunComputerTask(content)) {
      await runComputerTask(content, true);
      return;
    }
    setDraft("");
    let screenImage: string;
    try {
      screenImage = await captureScreenImage();
    } catch (error) {
      setDictationStatus(error instanceof Error ? error.message : "屏幕画面采集失败");
      return;
    }
    const userMessage = createInlineMessage("user", content);
    const pendingReply = createInlineMessage("assistant", "");
    const requestId = crypto.randomUUID();
    activeRequestId.current = requestId;
    realtimeRequestId.current = requestId;
    realtimeOutputRejected.current = false;
    assistantMessageId.current = pendingReply.id;
    streamContentRef.current = "";
    streamFallbackTaskRef.current = content;
    setMessages((current) => [...current, userMessage, pendingReply].slice(-14));
    setConversationOpen(true);
    setIsStreaming(true);
    setDictationStatus("正在由 AI 查看屏幕并回答");
    if (appearance.realtimeEdgeEnabled) void beginEdge(0.25);
    try {
      const requestMessages = [...messages, userMessage]
        .filter((message) => message.content.trim())
        .filter((message) => message.content.length <= 4000)
        .slice(-12)
        .map(({ role, content: messageContent }) => ({ role, content: messageContent }));
      const webSearch = await invoke<boolean>("get_web_search_enabled").catch(() => false);
      await invoke("chat_stream", { requestId, request: { messages: requestMessages, screenImage, webSearch } });
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      setMessages((current) => current.map((message) => message.id === pendingReply.id ? { ...message, content: `无法完成这次请求：${detail}` } : message));
      setIsStreaming(false);
      setDictationStatus("");
    }
  }, [appearance.realtimeEdgeEnabled, beginEdge, captureScreenImage, handleScreenTranslationCommand, messages, runComputerTask]);

  const stopListening = useCallback((submit = true) => {
    if (dictationStopTimer.current !== null) {
      window.clearTimeout(dictationStopTimer.current);
      dictationStopTimer.current = null;
    }
    dictationStopRequested.current = false;
    const capturedText = transcriptRef.current.trim();
    const mode = listeningMode.current;
    listeningMode.current = "chat";
    releaseVoice();
    listeningRef.current = false;
    setListening(false);
    setIsDictating(false);
    transcriptRef.current = "";
    if (mode === "dictation" && capturedText) {
      void finishDictation(capturedText);
      return;
    }
    if (mode === "dictation") {
      closeDictationPanel();
      return;
    }
    if (mode === "realtime") {
      if (submit && capturedText && realtimeActiveRef.current) {
        void sendRealtimePrompt(capturedText);
        return;
      }
      if (!realtimeActiveRef.current && appearance.realtimeEdgeEnabled) endEdge();
      return;
    }
    if (submit && capturedText) {
      void sendPrompt(capturedText);
      return;
    }
    endEdge();
  }, [appearance.realtimeEdgeEnabled, closeDictationPanel, endEdge, finishDictation, releaseVoice, sendPrompt, sendRealtimePrompt]);

  stopListeningRef.current = stopListening;

  const requestDictationStop = useCallback(() => {
    if (!listeningRef.current || listeningMode.current !== "dictation") {
      stopListeningRef.current?.(true);
      return;
    }
    if (dictationStopRequested.current) return;
    dictationStopRequested.current = true;
    dictationHeldRef.current = false;
    clearRealtimeReplaceTimer();
    setDictationPhase("finishing");
    const recorder = dictationRecorder.current;
    const realtimeCapture = realtimeDictationCapture.current;
    if (realtimeCapture) {
      realtimeDictationCapture.current = null;
      realtimeCapture.processor.disconnect();
      realtimeCapture.source.disconnect();
      realtimeCapture.silentGain.disconnect();
      void realtimeCapture.context.close();
      void realtimeCapture.sendQueue.finally(() => finishRealtimeAiDictation(realtimeCapture.sessionId));
      return;
    }
    if (recorder && recorder.state !== "inactive") {
      try {
        recorder.stop();
      } catch {
        stopListeningRef.current?.(false);
      }
      return;
    }
    // Recognition normally flushes its final result before onend. Keep this
    // fallback behind that path so releasing Alt does not cut off the ending.
    dictationStopTimer.current = window.setTimeout(() => {
      dictationStopTimer.current = null;
      stopListeningRef.current?.(true);
    }, 900);
    try {
      recognition.current?.stop();
    } catch {
      stopListeningRef.current?.(true);
      return;
    }
  }, [clearRealtimeReplaceTimer, finishRealtimeAiDictation]);

  // 听写条确认按钮：聆听中点击等于松开 Alt（收尾→润色→写入）；出错时点击重试写入。
  const confirmDictation = useCallback(() => {
    if (dictationPhase === "error") {
      retryDictationInsert();
      return;
    }
    if (listeningRef.current && listeningMode.current === "dictation") {
      requestDictationStop();
    }
  }, [dictationPhase, requestDictationStop, retryDictationInsert]);

  const startListening = useCallback(async (mode: ListeningMode = "chat") => {
    if (isStreamingRef.current || listeningRef.current) return;
    dictationFlowId.current += 1;
    const flowId = dictationFlowId.current;
    transcriptRef.current = "";
    finalTranscriptRef.current = "";
    dictationStopRequested.current = false;
    setListening(true);
    setIsDictating(mode === "dictation");
    setDictationStatus("");
    setDictationTranscript("");
    setDictationError("");
    if (mode === "dictation") setDictationPhase("listening");
    listeningRef.current = true;
    listeningMode.current = mode;
    if (mode === "dictation") dictationStartedHidden.current = false;
    closeContextMenu();
    if (mode === "dictation") {
      void invoke("warm_dictation_service").catch(() => undefined);
    }
    // 听写不再点亮屏幕边缘光：只显示独立的听写胶囊。
    const shouldShowEdge = mode === "realtime" ? appearance.realtimeEdgeEnabled : mode === "chat" ? appearance.edgeEnabled : false;
    if (shouldShowEdge) void beginEdge(0.2);
    const useAiDictation = mode === "dictation" && appearance.dictationMode === "ai";
    let microphoneStream: MediaStream | null = null;
    if (!useAiDictation) {
      const Constructor = browserSpeechConstructor();
      if (!Constructor) {
        stopListening(false);
        return;
      }
      const engine = new Constructor();
      engine.lang = "zh-CN";
      engine.continuous = true;
      engine.interimResults = true;
      engine.maxAlternatives = 3;
      const vocabulary = readDictationVocabulary();
      const Phrase = speechPhraseConstructor();
      if (Phrase && vocabulary.length > 0 && "phrases" in engine) {
      try {
        engine.phrases = vocabulary.map((term) => new Phrase(term, 5));
      } catch {
        // Contextual bias is experimental in WebView2; AI correction still uses the vocabulary.
      }
      }
      engine.onresult = (event) => {
      let interim = "";
      for (let index = event.resultIndex; index < event.results.length; index += 1) {
        const result = event.results[index];
        const segment = chooseSpeechAlternative(result, vocabulary);
        if (result.isFinal) finalTranscriptRef.current = appendSpeechSegment(finalTranscriptRef.current, segment);
        else interim = appendSpeechSegment(interim, segment);
      }
      const text = appendSpeechSegment(finalTranscriptRef.current, interim);
      transcriptRef.current = text;
      dictationLastResultAt.current = performance.now();
      if (mode === "dictation") setDictationTranscript(text);
      else setDraft(text);
      };
      const resumeDictationIfHeld = () => {
      if (!listeningRef.current || listeningMode.current !== "dictation" || !dictationHeldRef.current) return false;
      window.setTimeout(() => {
        if (!listeningRef.current || listeningMode.current !== "dictation" || !dictationHeldRef.current) return;
        try { engine.start(); } catch { /* The next keyboard-state poll will stop an unrecoverable session. */ }
      }, 110);
      return true;
      };
      engine.onerror = () => {
      if (dictationStopRequested.current) {
        stopListeningRef.current?.(true);
        return;
      }
      if (!resumeDictationIfHeld()) stopListening(false);
      };
      engine.onend = () => {
      if (dictationStopRequested.current) {
        const settleDelay = Math.max(0, 110 - (performance.now() - dictationLastResultAt.current));
        window.setTimeout(() => stopListeningRef.current?.(true), settleDelay);
        return;
      }
      if (listeningRef.current && listeningMode.current === "realtime" && realtimeActiveRef.current && !isStreamingRef.current) {
        try { engine.start(); } catch { /* A new recognizer will start after the next state change. */ }
        return;
      }
      if (resumeDictationIfHeld()) return;
      if (listeningRef.current && transcriptRef.current.trim()) stopListening(true);
      };
      recognition.current = engine;
      try {
        engine.start();
      } catch {
        stopListening(false);
        return;
      }
    }

    const startBatchRecorder = (mediaStream: MediaStream) => {
      const mimeType = dictationRecorderMimeType();
      if (!mimeType) throw new Error("当前系统不支持 AI 语音识别所需的音频录制格式");
      const recorder = new MediaRecorder(mediaStream, { mimeType, audioBitsPerSecond: 48_000 });
      dictationAudioChunks.current = [];
      recorder.ondataavailable = (event) => {
        if (event.data.size > 0) dictationAudioChunks.current.push(event.data);
      };
      recorder.onstop = () => {
        const chunks = dictationAudioChunks.current;
        dictationAudioChunks.current = [];
        dictationRecorder.current = null;
        const audio = new Blob(chunks, { type: recorder.mimeType || mimeType });
        if (audio.size > 0) void finishAiDictation(audio);
        else stopListeningRef.current?.(false);
      };
      recorder.onerror = () => stopListeningRef.current?.(false);
      dictationRecorder.current = recorder;
      recorder.start();
    };

    try {
      let configuredModel = "";
      if (useAiDictation) {
        try {
          const config = await invoke<{ model: string }>("get_dictation_asr_config");
          configuredModel = config.model;
        } catch {
          configuredModel = "";
        }
      }
      const useRealtime = useAiDictation && isStreamDictationModelName(configuredModel);
      // 麦克风与流式识别会话并行启动，压缩按下 Alt 到开始识别的延迟。
      const streamPromise = navigator.mediaDevices.getUserMedia({
        audio: {
          autoGainControl: true,
          noiseSuppression: true,
          echoCancellation: true,
          channelCount: { ideal: 1 },
          sampleRate: { ideal: 48000 },
        },
      }).catch((error) => {
        throw error;
      });
      let realtimeSession: { sessionId: string; sampleRate: number } | null = null;
      if (useRealtime) {
        try {
          realtimeSession = await invoke<{ sessionId: string; sampleRate: number }>("start_realtime_dictation");
          if (realtimeSession) realtimeDictationSessionId.current = realtimeSession.sessionId;
        } catch (error) {
          console.warn("流式语音识别启动失败，回退整段识别", error);
          realtimeSession = null;
        }
      }
      const stream = await streamPromise;
      microphoneStream = stream;
      if (dictationFlowId.current !== flowId || !listeningRef.current || listeningMode.current !== mode || (useAiDictation && dictationStopRequested.current)) {
        if (realtimeSession) void invoke("finish_realtime_dictation", { sessionId: realtimeSession.sessionId }).catch(() => undefined);
        realtimeDictationSessionId.current = null;
        stream.getTracks().forEach((track) => track.stop());
        return;
      }
      if (useAiDictation) {
        setDictationPhase("listening");
        let captureActive = false;
        if (realtimeSession) {
          const outputRate = realtimeSession.sampleRate;
          const captureContext = new AudioContext({ latencyHint: "interactive" });
          if (captureContext.state === "suspended") await captureContext.resume().catch(() => undefined);
          const captureSource = captureContext.createMediaStreamSource(stream);
          const silentGain = captureContext.createGain();
          silentGain.gain.value = 0;
          try {
            const worklet = await createPcmWorkletNode(captureContext, outputRate);
            const capture: RealtimeDictationCapture = {
              context: captureContext,
              processor: worklet,
              source: captureSource,
              silentGain,
              sessionId: realtimeSession.sessionId,
              sendQueue: Promise.resolve(),
              sampleRate: outputRate,
            };
            realtimeDictationText.current = "";
            realtimeDictationWritten.current = "";
            realtimeDictationInputFailed.current = false;
            worklet.port.onmessage = (event) => {
              const pcm = new Int16Array(event.data as ArrayBuffer);
              capture.sendQueue = capture.sendQueue
                .then(() => invoke("push_realtime_dictation_audio", { sessionId: capture.sessionId, pcmBase64: pcmToBase64([pcm]) }).then(() => undefined))
                .catch(() => undefined);
            };
            captureSource.connect(worklet);
            worklet.connect(silentGain);
            silentGain.connect(captureContext.destination);
            realtimeDictationCapture.current = capture;
            realtimeDictationSessionId.current = capture.sessionId;
            captureActive = true;
            // 并行录制整段音频：流式会话中途失败且没有转写时，用它走批量识别兜底。
            const fallbackMime = dictationRecorderMimeType();
            if (fallbackMime) {
              try {
                const fallbackRecorder = new MediaRecorder(stream, { mimeType: fallbackMime, audioBitsPerSecond: 48_000 });
                realtimeFallbackChunks.current = [];
                fallbackRecorder.ondataavailable = (event) => {
                  if (event.data.size > 0) realtimeFallbackChunks.current.push(event.data);
                };
                realtimeFallbackRecorder.current = fallbackRecorder;
                fallbackRecorder.start();
              } catch {
                // 兜底录音不可用只损失降级能力，不影响流式识别。
              }
            }
          } catch (error) {
            console.warn("流式音频采集不可用，回退整段识别", error);
            void invoke("finish_realtime_dictation", { sessionId: realtimeSession.sessionId }).catch(() => undefined);
            realtimeDictationSessionId.current = null;
            captureContext.close().catch(() => undefined);
            captureActive = false;
          }
        }
        if (!captureActive) startBatchRecorder(stream);
      }
      const context = new AudioContext({ latencyHint: "interactive" });
      if (context.state === "suspended") await context.resume();
      const source = context.createMediaStreamSource(stream);
      const analyser = context.createAnalyser();
      analyser.fftSize = 512;
      analyser.smoothingTimeConstant = 0.82;
      source.connect(analyser);
      const samples = new Uint8Array(analyser.fftSize);
      let lastEmission = 0;
      let heardVoice = false;
      let silenceStartedAt = 0;
      const animate = (now: number) => {
        analyser.getByteTimeDomainData(samples);
        let total = 0;
        for (const sample of samples) {
          const normalized = (sample - 128) / 128;
          total += normalized * normalized;
        }
        const level = Math.min(1, Math.sqrt(total / samples.length) * 4.8);
        if (now - lastEmission > 74) {
          if (shouldShowEdge) emitEdge(true, 0.24 + level * 0.76);
          setVoiceEnergy(level);
          lastEmission = now;
        }

        if (level > 0.045) {
          heardVoice = true;
          silenceStartedAt = 0;
        } else if ((mode === "realtime" || (mode === "chat" && appearance.autoSend)) && heardVoice && transcriptRef.current.trim()) {
          silenceStartedAt ||= now;
          if (now - silenceStartedAt > 1250) {
            stopListening(true);
            return;
          }
        }
        if (voiceResources.current) voiceResources.current.animationFrame = requestAnimationFrame(animate);
      };
      voiceResources.current = { stream, context, animationFrame: requestAnimationFrame(animate) };
    } catch (error) {
      // The recorder can already be running when the optional AudioContext used
      // for the wave animation fails to initialize. Do not discard a valid AI
      // dictation recording just because its visual meter is unavailable.
      if (useAiDictation && dictationRecorder.current?.state === "recording") {
        console.warn("AI dictation level meter unavailable; keeping audio recording active", error);
        // Keep ownership of the microphone stream so finishAiDictation can
        // release it after the recorder submits its audio.
        if (microphoneStream) voiceResources.current = { stream: microphoneStream, animationFrame: 0 };
        return;
      }
      // 麦克风或采集链路失败时，先启动的流式会话必须收掉，避免悬挂的后台任务。
      if (realtimeDictationSessionId.current && !realtimeDictationCapture.current) {
        const orphanSessionId = realtimeDictationSessionId.current;
        realtimeDictationSessionId.current = null;
        void invoke("finish_realtime_dictation", { sessionId: orphanSessionId }).catch(() => undefined);
      }
      stopListening(false);
    }
  }, [appearance.autoSend, appearance.dictationMode, appearance.edgeEnabled, appearance.realtimeEdgeEnabled, beginEdge, closeContextMenu, emitEdge, finishAiDictation, requestDictationStop, stopListening]);

  startListeningRef.current = startListening;

  const startRealtimeCall = useCallback(async () => {
    if (realtimeActive || isStreaming) return;
    try {
      const supported = await invoke<boolean>("realtime_model_supported", { request: { messages: [] } });
      if (!supported) {
        showRealtimeFailure("当前模型不支持图片识别，请在设置中选择支持视觉的模型。");
        return;
      }
      await captureScreenImage();
      realtimeActiveRef.current = true;
      setRealtimeActive(true);
      setRealtimePermissionOpen(false);
      setDictationStatus("实时通话已开启，开始说话即可提问");
      await startListening("realtime");
    } catch (error) {
      showRealtimeFailure(error instanceof Error ? error.message : "无法开启屏幕共享");
      stopRealtimeCall();
    }
  }, [captureScreenImage, isStreaming, realtimeActive, showRealtimeFailure, startListening, stopRealtimeCall]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen("kero:computer-control-stop", () => cancelComputerTask()).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, [cancelComputerTask]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<{ kind?: "target" | "mistake"; sequence?: number }>("kero:computer-marked", ({ payload }) => {
      if (!computerRunningRef.current) return;
      computerMarkPendingRef.current = true;
      if (typeof payload?.sequence === "number") {
        computerMarkSequenceRef.current = Math.max(computerMarkSequenceRef.current, payload.sequence);
      }
      const isMistake = payload?.kind === "mistake";
      setComputerStatus(isMistake ? "已记录避开标记，正在重新规划" : "已记录目标标记，正在据此纠正");
      updateComputerMessage(isMistake ? "已记录这个位置不应继续操作，下一次观察会避开它并选择新的路径。" : "已记录正确目标位置，下一次观察会重点检查该区域并纠正操作。");
    }).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, [updateComputerMessage]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<{ text: string }>("kero:computer-task", ({ payload }) => {
      if (payload.text.trim()) void runComputerTask(payload.text.trim());
    }).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, [runComputerTask]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<McpDecision>("kero:mcp-decision", ({ payload }) => {
      if (mcpCompletionTimer.current !== null) {
        window.clearTimeout(mcpCompletionTimer.current);
        mcpCompletionTimer.current = null;
      }
      setMcpDecision(payload);
      setMcpActive(true);
      setComputerStatus(payload.summary);
      setConversationOpen(true);
      void invoke("show_main_passive").catch(console.error);
      if (!payload.active) {
        mcpCompletionTimer.current = window.setTimeout(() => {
          setMcpActive(false);
          setMcpDecision(null);
          mcpCompletionTimer.current = null;
        }, 1800);
      }
    }).then((listener) => {
      unlisten = listener;
    });
    return () => {
      unlisten?.();
      if (mcpCompletionTimer.current !== null) window.clearTimeout(mcpCompletionTimer.current);
    };
  }, []);

  useEffect(() => {
    if (appearance.clickThrough) void invoke("set_main_click_through", { enabled: true });
  }, []);

  useEffect(() => {
    if (!appearance.positionLocked) return;
    let saved: WindowPosition | undefined;
    try {
      const raw = window.localStorage.getItem("kero-locked-position");
      if (raw) saved = JSON.parse(raw) as WindowPosition;
    } catch {
      saved = undefined;
    }
    void invoke("set_main_position_lock", { enabled: true, x: saved?.x, y: saved?.y });
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<StreamPayload>("kero:stream", ({ payload }) => {
      if (payload.requestId !== activeRequestId.current) return;
      const pendingId = assistantMessageId.current;
      const delta = payload.delta;
      if (delta && pendingId) {
        setMessages((current) => current.map((message) => {
          if (message.id !== pendingId || realtimeOutputRejected.current) return message;
          const content = mergeStreamText(streamContentRef.current, delta);
          streamContentRef.current = content;
          if (content.includes(computerControlCallMarker)) {
            return { ...message, content: "正在切换到电脑操控..." };
          }
          if (payload.requestId === realtimeRequestId.current && (content.length > 1800 || unusableRealtimeReply(content))) {
            realtimeOutputRejected.current = true;
            return { ...message, content: "模型返回了不可读的内容。请更换支持视觉的模型后重试。" };
          }
          return { ...message, content };
        }));
      }
      if (payload.done) {
        const fallbackTask = streamContentRef.current.includes(computerControlCallMarker)
          ? streamFallbackTaskRef.current.trim()
          : "";
        const fromRealtime = payload.requestId === realtimeRequestId.current;
        if (payload.error && pendingId) {
          setMessages((current) => current.map((message) => message.id === pendingId
            ? { ...message, content: `无法完成这次请求：${payload.error}` }
            : message));
        } else if (pendingId) {
          setMessages((current) => current.map((message) => message.id === pendingId && !message.content.trim()
            ? { ...message, content: "模型没有返回可显示的内容。" }
            : message));
        }
        setIsStreaming(false);
        isStreamingRef.current = false;
        activeRequestId.current = undefined;
        if (payload.requestId === realtimeRequestId.current) realtimeRequestId.current = undefined;
        setDictationStatus("");
        streamContentRef.current = "";
        streamFallbackTaskRef.current = "";
        if (fallbackTask && !payload.error) {
          if (pendingId) {
            setMessages((current) => current.filter((message) => message.id !== pendingId));
          }
          endEdge();
          window.setTimeout(() => void runComputerTaskRef.current?.(fallbackTask, fromRealtime, false), 0);
        } else if (realtimeActiveRef.current) {
          window.setTimeout(() => void startListening("realtime"), 180);
        } else {
          endEdge();
        }
      }
    }).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, [endEdge, startListening]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<RealtimeDictationEvent>("kero:realtime-dictation", ({ payload }) => {
      if (!payload || payload.sessionId !== realtimeDictationSessionId.current) return;
      const incoming = (payload.fullText ?? payload.text ?? "").trim();
      if (incoming) {
        realtimeDictationText.current = incoming;
        transcriptRef.current = incoming;
        finalTranscriptRef.current = incoming;
        setDictationTranscript(incoming);
        // 实时打入目标输入框：250ms 节流合并，避免连续结果导致"删了打、打了删"；
        // 松开 Alt 之后停止实时打字，只等最终结果（差量补打 + 润色替换）。
        realtimePendingWritten.current = incoming;
        if (!dictationStopRequested.current && incoming !== realtimeDictationWritten.current && realtimeReplaceTimer.current === null) {
          realtimeReplaceTimer.current = window.setTimeout(() => {
            realtimeReplaceTimer.current = null;
            applyRealtimeReplace();
          }, 250);
        }
      }
      if (!payload.done) return;
      realtimeDictationSessionId.current = null;
      const flowId = dictationFlowId.current;
      const transcript = (incoming || realtimeDictationText.current).trim();
      realtimeDictationText.current = "";
      const resetToListeningOff = () => {
        releaseVoice();
        listeningRef.current = false;
        listeningMode.current = "chat";
        setListening(false);
        setIsDictating(false);
      };
      if (payload.error) {
        resetToListeningOff();
        const fallbackBlob = takeRealtimeFallbackBlob();
        if (!transcript && fallbackBlob) {
          // 流式会话失败且没有任何转写 → 用并行录制的整段音频走批量识别。
          void finishAiDictation(fallbackBlob);
          return;
        }
        void invoke("clear_dictation_focus_target");
        if (transcript) dictationFinalTextRef.current = transcript;
        setDictationError(payload.error);
        setDictationPhase("error");
        return;
      }
      if (!transcript) {
        resetToListeningOff();
        void invoke("clear_dictation_focus_target");
        setDictationError("没有听到有效内容，再试一次吧");
        setDictationPhase("error");
        return;
      }
      setDictationTranscript(transcript);
      if (appearance.aiDictationPolish) {
        resetToListeningOff();
        setDictationPhase("polishing");
        const beforePolish = realtimeDictationWritten.current;
        void invoke<string>("optimize_dictation", {
          text: transcript,
          correctTypos: appearance.dictationCorrection,
          vocabulary: readDictationVocabulary().join("\n"),
          memory: appearance.dictationMemory ? readDictationMemory().join("\n") : undefined,
        }).catch(() => transcript).then(async (optimized) => {
          if (dictationFlowId.current !== flowId) return;
          const text = optimized.trim() || transcript;
          dictationFinalTextRef.current = text;
          if (appearance.dictationMemory) rememberDictation(text);
          setDictationPhase("inserting");
          try {
            await realtimeDictationInsertQueue.current;
            await invoke("replace_realtime_dictation_text", { previous: beforePolish, text });
            if (dictationFlowId.current !== flowId) return;
            realtimeDictationWritten.current = "";
            closeDictationPanel();
          } catch {
            realtimeDictationInputFailed.current = true;
            setDictationError("无法写入原输入框，可重试或复制内容");
            setDictationPhase("error");
          }
        });
      } else {
        resetToListeningOff();
        // 收尾补打：松开 Alt 后被节流掉的尾部在这里一次性写入（差量，内容一致时无击键）。
        void realtimeDictationInsertQueue.current.then(async () => {
          if (dictationFlowId.current !== flowId) return;
          try {
            await invoke("replace_realtime_dictation_text", { previous: realtimeDictationWritten.current, text: transcript });
            if (dictationFlowId.current !== flowId) return;
            realtimeDictationWritten.current = "";
            closeDictationPanel();
          } catch {
            dictationFinalTextRef.current = transcript;
            setDictationError("无法写入原输入框, 可重试或复制内容");
            setDictationPhase("error");
          }
        });
      }
    }).then((listener) => { unlisten = listener; });
    return () => unlisten?.();
  }, [appearance.aiDictationPolish, appearance.dictationCorrection, appearance.dictationMemory, applyRealtimeReplace, closeDictationPanel, finishAiDictation, releaseVoice, takeRealtimeFallbackBlob]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ScreenTranslationStatus>("kero:screen-translation-status", ({ payload }) => {
      setScreenTranslationActive(payload.active);
      if (!payload.active) setScreenTranslationReady(false);
      if (payload.active && !payload.loading) setScreenTranslationReady(true);
      setDictationStatus(payload.active && payload.loading ? "翻译中" : "");
      if (payload.error) void invoke("hide_edge").catch(() => undefined);
      const assistantId = translationAssistantId.current;
      if (!assistantId) return;
      const content = payload.error
        ? `全屏翻译已停止：${payload.error}`
        : payload.active && payload.loading
          ? "翻译中..."
          : payload.active
            ? "全屏翻译已开启，屏幕内容变化后会自动更新"
            : "全屏翻译已关闭";
      setMessages((current) => current.map((message) => message.id === assistantId ? { ...message, content } : message));
      if (!payload.active) translationAssistantId.current = undefined;
    }).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<Partial<Appearance>>("kero:appearance-request", ({ payload }) => {
      updateAppearance(payload, false);
    }).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, [updateAppearance]);

  useEffect(() => {
    if (dictationPanel) {
      // 听写条：直接出现在屏幕底部、任务栏上方、水平居中，不做从胶囊形变过来的动画。
      let cancelled = false;
      void (async () => {
        const win = getCurrentWindow();
        const [position, scale, workArea] = await Promise.all([
          win.outerPosition().catch(() => null),
          win.scaleFactor().catch(() => 1),
          invoke<WorkArea>("get_work_area").catch(() => null),
        ]);
        // origin 为空说明这一轮听写条还没定位过：直接切换出现，之后的长宽变化才走动画。
        const firstPlacement = position !== null && !dictationGrowOrigin.current;
        if (position && !dictationGrowOrigin.current) {
          dictationGrowOrigin.current = { x: position.x, y: position.y };
        }
        let restoreX: number | undefined;
        let restoreY: number | undefined;
        if (workArea) {
          const scaleValue = scale || 1;
          const pillPhysicalWidth = Math.round(dictationWidth * scaleValue);
          const pillPhysicalHeight = Math.round(dictationPillHeight * scaleValue);
          const margin = Math.round(16 * scaleValue);
          restoreX = workArea.x + Math.round((workArea.width - pillPhysicalWidth) / 2);
          restoreY = workArea.y + workArea.height - pillPhysicalHeight - margin;
        }
        if (cancelled) return;
        if (firstPlacement) {
          await invoke("hide_window", { label: "main" }).catch(() => undefined);
          await invoke("set_main_size", { width: dictationWidth, height: dictationPillHeight, restoreX, restoreY, animate: false }).catch(() => undefined);
          if (cancelled) return;
          await invoke("show_main_passive").catch(() => undefined);
        } else {
          await invoke("set_main_size", { width: dictationWidth, height: dictationPillHeight, restoreX, restoreY }).catch(() => undefined);
        }
      })();
      return () => {
        cancelled = true;
      };
    }
    const height = menuOpen ? contextMenuHeight : realtimePermissionOpen ? 174 : conversationVisible ? inlineHeight : 72;
    const restore = conversationVisible ? null : collapseTarget;
    if (dictationGrowOrigin.current && !conversationVisible && !menuOpen && !realtimePermissionOpen) {
      // 听写结束：胶囊直接在原位恢复，不做收回去的动画；从托盘唤起的保持隐藏。
      const origin = dictationGrowOrigin.current;
      dictationGrowOrigin.current = null;
      const restoreShow = !dictationStartedHidden.current;
      void (async () => {
        await invoke("hide_window", { label: "main" }).catch(() => undefined);
        await invoke("set_main_size", { width: currentWidth, height, restoreX: origin.x, restoreY: origin.y, animate: false }).catch(() => undefined);
        if (restoreShow) await invoke("show_main_passive").catch(() => undefined);
      })();
      return undefined;
    }
    void invoke("set_main_size", { width: currentWidth, height, restoreX: restore?.x, restoreY: restore?.y });
    if (restore) {
      const timer = window.setTimeout(() => setCollapseTarget(null), 240);
      return () => window.clearTimeout(timer);
    }
    return undefined;
  }, [collapseTarget, conversationVisible, currentWidth, dictationPillHeight, dictationPanel, dictationWidth, inlineHeight, menuOpen, realtimePermissionOpen]);

  useEffect(() => {
    window.localStorage.setItem("kero-inline-history", JSON.stringify(messages));
  }, [messages]);

  useEffect(() => {
    if (!appearance.edgeEnabled) endEdge();
  }, [appearance.edgeEnabled, endEdge]);

  useEffect(() => {
    let stopClickThrough: (() => void) | undefined;
    let stopDictationStart: (() => void) | undefined;
    let stopDictationEnd: (() => void) | undefined;
    void listen<boolean>("kero:click-through-changed", ({ payload }) => updateAppearance({ clickThrough: payload }, false))
      .then((unlisten) => { stopClickThrough = unlisten; });
    void listen("kero:shortcut-dictation-start", () => {
      const sessionId = ++dictationSessionId.current;
      dictationHeldRef.current = true;
      dictationOpenedFromTray.current = false;
      trayDictationSessionId.current = null;
      void (async () => {
        try {
          const wasHidden = await invoke<boolean>("show_main_passive");
          if (dictationSessionId.current !== sessionId) return;
          dictationOpenedFromTray.current = wasHidden;
          trayDictationSessionId.current = wasHidden ? sessionId : null;
          if (!dictationHeldRef.current) {
            hideAfterTrayDictation();
            return;
          }
          await startListening("dictation");
          dictationStartedHidden.current = wasHidden;
          // A blocked microphone or an interrupted startup must not leave a tray-only capsule visible.
          if (wasHidden && !listeningRef.current) hideAfterTrayDictation();
        } catch (error) {
          dictationOpenedFromTray.current = false;
          console.error(error);
        }
      })();
    }).then((unlisten) => { stopDictationStart = unlisten; });
    void listen("kero:shortcut-dictation-stop", () => {
      requestDictationStop();
    }).then((unlisten) => { stopDictationEnd = unlisten; });
    let stopDictationCancel: (() => void) | undefined;
    void listen("kero:shortcut-dictation-cancel", () => {
      cancelDictation();
    }).then((unlisten) => { stopDictationCancel = unlisten; });
    return () => {
      stopClickThrough?.();
      stopDictationStart?.();
      stopDictationEnd?.();
      stopDictationCancel?.();
    };
  }, [cancelDictation, hideAfterTrayDictation, requestDictationStop, startListening, updateAppearance]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeContextMenu();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [closeContextMenu]);

  useEffect(() => () => {
    releaseVoice();
    if (dictationStopTimer.current !== null) window.clearTimeout(dictationStopTimer.current);
    if (realtimeReplaceTimer.current !== null) window.clearTimeout(realtimeReplaceTimer.current);
  }, [releaseVoice]);

  const openContextMenu = (event: ReactMouseEvent<HTMLElement>) => {
    event.preventDefault();
    const menuWidth = 274;
    const x = Math.max(8, Math.min(event.clientX - 24, currentWidth - menuWidth - 8));
    void (async () => {
      const [placement, origin] = await Promise.all([
        invoke<ContextMenuPlacement>("get_context_menu_placement", { menuHeight: contextMenuHeight }),
        invoke<WindowPosition>("get_main_position"),
      ]);
      if (placement.above) {
        await invoke("set_main_position", {
          x: origin.x,
          y: placement.targetY ?? origin.y - menuAboveOffset,
          updateLockedPosition: false,
        });
      }
      setContextMenu({ x, above: placement.above, origin: placement.above ? origin : undefined });
    })().catch(console.error);
  };

  const collapseConversation = () => {
    if (conversationOrigin.current) setCollapseTarget(conversationOrigin.current);
    setConversationOpen(false);
  };

  const toggleClickThrough = async () => {
    const enabled = !appearance.clickThrough;
    await invoke("set_main_click_through", { enabled });
    updateAppearance({ clickThrough: enabled });
    if (enabled) closeContextMenu();
  };

  const togglePositionLock = async () => {
    const enabled = !appearance.positionLocked;
    if (enabled) {
      const position = await invoke<WindowPosition>("get_main_position");
      window.localStorage.setItem("kero-locked-position", JSON.stringify(position));
      await invoke("set_main_position_lock", { enabled: true, x: position.x, y: position.y });
    } else {
      window.localStorage.removeItem("kero-locked-position");
      await invoke("set_main_position_lock", { enabled: false });
    }
    updateAppearance({ positionLocked: enabled });
  };

  const nudgeCapsule = async (dx: number, dy: number) => {
    const menu = contextMenu;
    const lockedPosition = menu?.above && menu.origin
      ? { x: menu.origin.x + dx, y: menu.origin.y + dy }
      : undefined;
    const position = await invoke<WindowPosition>("move_main_by", {
      dx,
      dy,
      lockedX: lockedPosition?.x,
      lockedY: lockedPosition?.y,
    });
    if (appearance.positionLocked) {
      window.localStorage.setItem("kero-locked-position", JSON.stringify(lockedPosition ?? position));
    }
    if (lockedPosition) {
      setContextMenu((current) => current ? { ...current, origin: lockedPosition } : null);
    }
  };

  const onPointerDown = (event: PointerEvent<HTMLElement>) => {
    const target = event.target as HTMLElement;
    if (appearance.clickThrough || appearance.positionLocked || event.button !== 0 || target.closest("button, .capsule-context-menu")) return;
    if (target.closest("input, form")) {
      dragOrigin.current = { x: event.clientX, y: event.clientY };
    } else {
      void getCurrentWindow().startDragging().catch(console.error);
    }
  };

  const onPointerMove = (event: PointerEvent<HTMLElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect();
    capsuleRef.current?.style.setProperty("--glass-x", `${((event.clientX - bounds.left) / bounds.width) * 100}%`);
    capsuleRef.current?.style.setProperty("--glass-y", `${((event.clientY - bounds.top) / bounds.height) * 100}%`);
    if (dragOrigin.current && (event.buttons & 1) && Math.hypot(event.clientX - dragOrigin.current.x, event.clientY - dragOrigin.current.y) > 4) {
      dragOrigin.current = null;
      void getCurrentWindow().startDragging().catch(console.error);
    }
  };

  const openFullConversation = async () => {
    closeContextMenu();
    await emitTo("chat", "kero:inline-history", { messages });
    await invoke("open_chat");
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    void sendPrompt(draft);
  };

  const shellStyle = {
    "--capsule-opacity": String(appearance.opacity / 100),
    "--glass-layer-opacity": String(Math.max(0.08, (appearance.opacity / 100) * 0.72)),
    "--solid-layer-opacity": String(Math.max(0.04, (appearance.opacity / 100) * 0.32)),
    "--cursor-glow-opacity": String(Math.max(0.72, 0.98 - (appearance.opacity / 100) * 0.22)),
  } as CSSProperties;
  const visibleComputerControl = computerRunning || mcpActive;

  return (
    <main
      className={`capsule-shell ${menuOpen ? "has-context-menu" : ""} ${contextMenu?.above ? "menu-above" : ""} ${conversationVisible ? "has-conversation" : ""} ${realtimePermissionOpen ? "has-realtime-permission" : ""}`}
      style={shellStyle}
      onPointerDown={(event) => {
        if (contextMenu && event.target === event.currentTarget) closeContextMenu();
      }}
    >
      <section
        className={`capsule reference-capsule ${dictationPanel ? "is-dictation-pill" : appearance.glassEnabled ? "is-live-glass" : "is-solid"}`}
        ref={capsuleRef}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={() => { dragOrigin.current = null; }}
        onContextMenu={openContextMenu}
      >
        {dictationPanel && (
          <div className="dictation-pill">
            <button type="button" className="dictation-end" title="取消听写 (Esc)" onClick={cancelDictation}><X size={13} /></button>
            <div
              className={`dictation-middle ${dictationPhase === "polishing" ? "is-polishing" : ""}`}
              ref={dictationTranscriptRef}
              title={dictationPhase === "error" ? (dictationError || "无法写入原输入框") : undefined}
            >
              {dictationPhase === "error" ? (
                <span className="dictation-error-msg">{dictationError || "无法写入原输入框"}</span>
              ) : dictationTranscript ? (
                <>
                  <span className="dictation-text-content">{dictationTranscript}</span>
                  {dictationPhase === "listening" && (
                    <span className="dictation-live-wave" aria-hidden="true">
                      {[0.62, 0.92, 1, 0.78].map((factor, index) => (
                        <i key={index} style={{ height: `${Math.max(3, (5 + voiceEnergy * 13) * factor)}px` }} />
                      ))}
                    </span>
                  )}
                </>
              ) : dictationPhase === "listening" ? (
                <span className="dictation-live-wave is-strip" aria-hidden="true">
                  {dictationWaveFactors.map((factor, index) => (
                    <i key={index} style={{ height: `${Math.max(3, (4 + voiceEnergy * 16) * factor)}px` }} />
                  ))}
                </span>
              ) : (
                <span className="dictation-live-wave is-strip is-working" aria-hidden="true">
                  {dictationWaveFactors.map((factor, index) => <i key={index} />)}
                </span>
              )}
            </div>
            {dictationPhase === "error" && (
              <button type="button" className="dictation-end" title="复制内容" onClick={() => void copyDictationText()}><Copy size={12} /></button>
            )}
            <button
              type="button"
              className="dictation-end is-confirm"
              title={dictationPhase === "error" ? "重试写入" : "结束并写入"}
              disabled={dictationPhase !== "listening" && dictationPhase !== "error"}
              onClick={confirmDictation}
            >{dictationPhase === "error" ? <RotateCcw size={13} /> : <Check size={14} />}</button>
          </div>
        )}
        {!dictationPanel && conversationVisible && (
          <div className="inline-chat" aria-live="polite">
            <div className="inline-chat-head">
              <span>{mcpActive ? <BrainCircuit size={14} /> : computerRunning ? <MousePointer2 size={14} /> : <Waves size={14} />} {visibleComputerControl ? computerStatus || "正在操控电脑" : "Kero"}</span>
              {visibleComputerControl
                ? <button type="button" title="停止电脑操控" onClick={() => {
                  if (mcpActive) {
                    void invoke("stop_computer_control");
                    setMcpActive(false);
                    setMcpDecision(null);
                  } else {
                    cancelComputerTask();
                  }
                }}><Square size={14} /></button>
                : <button type="button" title="收起对话" onClick={collapseConversation} disabled={isStreaming}><ChevronUp size={16} /></button>}
            </div>
            <div className="inline-message-list">
              {mcpActive && mcpDecision ? (
                <section className={`mcp-decision phase-${mcpDecision.phase}`}>
                  <header><span>{mcpPhaseLabels[mcpDecision.phase]}</span><b>{mcpDecision.summary}</b></header>
                  {mcpDecision.detail && <p>{mcpDecision.detail}</p>}
                  {mcpDecision.nextAction && <footer><ArrowRight size={13} /><span>{mcpDecision.nextAction}</span></footer>}
                </section>
              ) : recentMessages.map((message) => (
                <article className={`inline-message ${message.role}`} key={message.id}>
                  <span>{message.role === "user" ? "你" : "Kero"}</span>
                  {message.role === "assistant"
                    ? <div className="markdown-content">{message.content ? <ReactMarkdown remarkPlugins={[remarkGfm]} components={{
                      a: ({ href, children }) => <a href={href} onClick={(event) => {
                        event.preventDefault();
                        if (href && /^https?:\/\//i.test(href)) void openUrl(href).catch(console.error);
                      }}>{children}</a>,
                    }}>{message.content}</ReactMarkdown> : <i className="inline-thinking"><b /><b /><b /></i>}</div>
                    : <p>{message.content}</p>}
                </article>
              ))}
            </div>
            {computerConfirmation && (
              <div className="computer-confirmation" role="dialog" aria-label="确认电脑操作">
                <p>{computerConfirmation.description}</p>
                <div>
                  <button type="button" className="cancel" onClick={() => {
                    computerConfirmationResolver.current?.(false);
                    computerConfirmationResolver.current = null;
                    setComputerConfirmation(null);
                  }}><X size={14} /> 取消</button>
                  <button type="button" className="confirm" onClick={() => {
                    computerConfirmationResolver.current?.(true);
                    computerConfirmationResolver.current = null;
                    setComputerConfirmation(null);
                  }}><Check size={14} /> 确认</button>
                </div>
              </div>
            )}
          </div>
        )}
        {!dictationPanel && realtimePermissionOpen && (
          <div className="realtime-permission" role="dialog" aria-label="实时通话屏幕访问确认">
            <div><MonitorUp size={18} /><span><b>允许 AI 查看主屏幕？</b><small>仅在你说完问题时发送一帧画面。</small></span></div>
            <section>
              <button type="button" onClick={() => setRealtimePermissionOpen(false)}>暂不允许</button>
              <button type="button" className="allow" onClick={() => void startRealtimeCall()}>允许并开启</button>
            </section>
          </div>
        )}
        {!dictationPanel && (
          <div className="capsule-composer">
            <button
              className={`voice-disc ${listening ? "is-listening" : ""}`}
              type="button"
              title={listening ? "结束语音输入" : "语音输入"}
              onClick={() => (listening ? stopListening(true) : void startListening())}
              disabled={isStreaming}
            >
              {listening ? <Square size={15} fill="currentColor" /> : <AudioLines size={24} strokeWidth={2.2} />}
            </button>
            {(listening || dictationProcessing || realtimeActive || screenTranslationActive) && (
              <div className={`voice-wave ${isDictating ? "is-dictating" : ""} ${dictationProcessing ? "is-processing" : ""}`} aria-label={dictationStatus || (isDictating ? "正在听写" : "正在聆听")}>
                {[0.58, 0.82, 1, 0.82, 0.58].map((factor, index) => <i key={index} style={{ transform: `scaleY(${0.34 + voiceEnergy * (1.2 + factor)})` }} />)}
                <small>{dictationStatus || (screenTranslationActive ? "实时翻译中" : realtimeActive ? "实时通话中" : isDictating ? "听写中" : "聆听中")}</small>
              </div>
            )}
            <form className="reference-form" onSubmit={submit}>
              <input
                className="reference-input"
                value={draft}
                onChange={(event) => setDraft(event.target.value)}
                placeholder={mcpActive ? "MCP 操控中，可在 Codex 中补充要求" : computerRunning ? "补充或修正当前电脑操作" : listening ? "正在聆听，停顿后自动发送" : isStreaming ? "Kero 正在回复" : "和 Kero 对话"}
                aria-label="发送给 Kero 的消息"
                disabled={mcpActive || (isStreaming && !computerRunning)}
              />
            </form>
            {draft.trim()
              ? <button
                  className="composer-send"
                  type="button"
                  title="发送消息"
                  aria-label="发送消息"
                  onClick={() => void sendPrompt(draft)}
                  disabled={mcpActive || (isStreaming && !computerRunning)}
                ><SendHorizontal size={18} strokeWidth={2.25} /></button>
              : screenTranslationReady
                ? <button
                    className="realtime-disc translation-close"
                    type="button"
                    title="关闭全屏翻译"
                    aria-label="关闭全屏翻译"
                    onClick={() => void handleScreenTranslationCommand("stop", "关闭全屏翻译")}
                  ><X size={19} strokeWidth={2.3} /></button>
                : <button
                  className={`realtime-disc ${realtimeActive ? "is-active" : ""}`}
                  type="button"
                  title={screenTranslationActive ? "翻译中" : realtimeActive ? "结束实时通话" : "开启屏幕感知实时通话"}
                  onClick={() => { if (realtimeActive) stopRealtimeCall(); else setRealtimePermissionOpen(true); }}
                  disabled={screenTranslationActive || (isStreaming && !realtimeActive)}
                >
                  {realtimeActive ? <Square size={14} fill="currentColor" /> : <MonitorUp size={19} />}
                </button>}
          </div>
        )}
      </section>

      {contextMenu && (
        <aside className={`capsule-context-menu ${contextMenu.above ? "opens-above" : ""}`} style={{ left: contextMenu.x }} onPointerDown={(event) => event.stopPropagation()}>
          <div className="context-heading"><SlidersHorizontal size={15} /> 胶囊控制</div>
          <button className="context-item open-conversation" type="button" onClick={() => void openFullConversation()}>
            <span><MessageCircleMore size={16} /> 打开完整对话</span>
          </button>
          <button className="context-item" type="button" onClick={() => { closeContextMenu(); void invoke("open_settings"); }}>
            <span><Settings2 size={16} /> 打开设置</span>
          </button>
          <button className="context-item" type="button" onClick={closeContextMenu}>
            <span><X size={16} /> 关闭右键菜单</span>
          </button>
          <div className="context-divider" />
          <div className="context-size">
            <span>胶囊大小</span>
            <div role="group" aria-label="胶囊大小">
              {(Object.keys(sizeWidths) as CapsuleSize[]).map((size) => (
                <button key={size} className={appearance.size === size ? "selected" : ""} type="button" onClick={() => updateAppearance({ size })}>{sizeLabels[size]}</button>
              ))}
            </div>
          </div>
          <label className="opacity-control">
            <span>透明度</span><b>{appearance.opacity}%</b>
            <input type="range" min="25" max="100" value={appearance.opacity} onChange={(event) => updateAppearance({ opacity: Number(event.target.value) })} />
          </label>
          <button className="context-item" type="button" onClick={() => updateAppearance({ glassEnabled: !appearance.glassEnabled })}>
            <span><Droplets size={16} /> 实时玻璃</span>
            <i className={appearance.glassEnabled ? "is-on" : ""} aria-hidden="true"><em /></i>
          </button>
          <button className="context-item" type="button" onClick={() => updateAppearance({ edgeEnabled: !appearance.edgeEnabled })}>
            <span><Waves size={16} /> 唤起边缘光</span>
            <i className={appearance.edgeEnabled ? "is-on" : ""} aria-hidden="true"><em /></i>
          </button>
          <button className="context-item" type="button" onClick={() => void toggleClickThrough()}>
            <span><MousePointer2 size={16} /> 鼠标穿透</span>
            <i className={appearance.clickThrough ? "is-on" : ""} aria-hidden="true"><em /></i>
          </button>
          <button className="context-item" type="button" onClick={() => void togglePositionLock()}>
            <span><LockKeyhole size={16} /> 锁定胶囊位置</span>
            <i className={appearance.positionLocked ? "is-on" : ""} aria-hidden="true"><em /></i>
          </button>
          <div className="context-position" aria-label="调整胶囊位置">
            <span>调整位置</span>
            <div className="position-pad">
              <span />
              <button type="button" title="向上移动" onClick={() => void nudgeCapsule(0, -24)}><ArrowUp size={14} /></button>
              <span />
              <button type="button" title="向左移动" onClick={() => void nudgeCapsule(-24, 0)}><ArrowLeft size={14} /></button>
              <span />
              <button type="button" title="向右移动" onClick={() => void nudgeCapsule(24, 0)}><ArrowRight size={14} /></button>
              <span />
              <button type="button" title="向下移动" onClick={() => void nudgeCapsule(0, 24)}><ArrowDown size={14} /></button>
              <span />
            </div>
          </div>
          <div className="context-divider" />
          <button className="context-item hide-capsule" type="button" onClick={() => void invoke("minimize_to_tray")}>
            <span><EyeOff size={16} /> 最小化到托盘</span><Minimize2 size={15} />
          </button>
          <button className="context-item" type="button" onClick={() => void invoke("quit_app")}>
            <span><Power size={16} /> 关闭 Kero</span>
          </button>
        </aside>
      )}
    </main>
  );
}
