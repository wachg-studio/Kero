import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Bot, ChevronDown, CircleAlert, Copy, Download, Globe2, ImagePlus, Languages, Maximize2, MessageSquarePlus, Minimize2, Minus, MousePointer2, Paperclip, RefreshCw, Send, SlidersHorizontal, Sparkles, WandSparkles, X } from "lucide-react";
import { ChangeEvent, FormEvent, KeyboardEvent, PointerEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { flushSync } from "react-dom";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ChatAttachment, ChatMessage, GeneratedImage, ImageGenerationConfig, Provider } from "../types";

const welcome: ChatMessage = {
  id: "welcome",
  role: "assistant",
  content: "你好，我是 Kero。配置一个模型服务后，我可以开始协助你。",
};

function createMessage(role: ChatMessage["role"], content: string): ChatMessage {
  return { id: crypto.randomUUID(), role, content };
}

type ScreenTranslationStatus = {
  active: boolean;
  loading: boolean;
  error?: string | null;
};

function looksLikeComputerTask(text: string) {
  const compact = text.trim().toLowerCase();
  const action = "打开|启动|运行|点击|双击|输入|填写|发送|回复|关闭|切换|滚动|搜索联系人|帮我发|替我发|操作电脑|操作软件";
  return new RegExp(`^(请|帮我|替我|给我|现在|去)?\\s*(${action})`).test(compact)
    || new RegExp(`(帮我|替我|请你|你去).{0,24}(${action})`).test(compact);
}

async function shouldRunComputerTask(text: string) {
  const enabled = await invoke<boolean>("get_computer_control_enabled").catch(() => false);
  if (!enabled) return false;
  return invoke<boolean>("computer_control_intent", { text }).catch(() => looksLikeComputerTask(text));
}

const MAX_CHAT_ATTACHMENTS = 4;
const MAX_REFERENCE_IMAGES = 4;
const MAX_IMAGE_BYTES = 8 * 1024 * 1024;
const MAX_TEXT_FILE_BYTES = 2 * 1024 * 1024;

function fileDataUrl(file: File) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`无法读取 ${file.name}`));
    reader.onload = () => resolve(String(reader.result));
    reader.readAsDataURL(file);
  });
}

function fileText(file: File) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`无法读取 ${file.name}`));
    reader.onload = () => resolve(String(reader.result));
    reader.readAsText(file);
  });
}

function isTextFile(file: File) {
  const extension = file.name.split(".").pop()?.toLowerCase() ?? "";
  return file.type.startsWith("text/") || ["csv", "json", "md", "txt", "log", "js", "ts", "tsx", "jsx", "py", "rs", "java", "c", "cpp", "h", "html", "css", "xml", "yml", "yaml", "toml", "ini", "sql", "sh"].includes(extension);
}

export function ChatWindow() {
  const [messages, setMessages] = useState<ChatMessage[]>([welcome]);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [providerId, setProviderId] = useState<string>();
  const [draft, setDraft] = useState("");
  const [isSending, setIsSending] = useState(false);
  const [isParsing, setIsParsing] = useState(false);
  const [isLeaving, setIsLeaving] = useState(false);
  const [isMaximized, setIsMaximized] = useState(false);
  const [showQuickControls, setShowQuickControls] = useState(false);
  const [contextEnabled, setContextEnabled] = useState(true);
  const [webSearchEnabled, setWebSearchEnabled] = useState(false);
  const [computerControlEnabled, setComputerControlEnabled] = useState(false);
  const [translationStatus, setTranslationStatus] = useState<ScreenTranslationStatus>({ active: false, loading: false });
  const [imageConfig, setImageConfig] = useState<ImageGenerationConfig | null>(null);
  const [showImageGenerator, setShowImageGenerator] = useState(false);
  const [imagePrompt, setImagePrompt] = useState("");
  const [imageAspectRatio, setImageAspectRatio] = useState("1:1");
  const [imageCount, setImageCount] = useState(1);
  const [referenceImages, setReferenceImages] = useState<ChatAttachment[]>([]);
  const [generatedImageReferences, setGeneratedImageReferences] = useState<Record<string, ChatAttachment[]>>({});
  const [isGeneratingImage, setIsGeneratingImage] = useState(false);
  const [isOptimizingImagePrompt, setIsOptimizingImagePrompt] = useState(false);
  const [imageNotice, setImageNotice] = useState("");
  const [downloadingImagePath, setDownloadingImagePath] = useState<string | null>(null);
  const [downloadNoticePath, setDownloadNoticePath] = useState<string | null>(null);
  const [quickNotice, setQuickNotice] = useState("");
  const [chatAttachments, setChatAttachments] = useState<ChatAttachment[]>([]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const surfaceRef = useRef<HTMLElement>(null);
  const chatAttachmentInputRef = useRef<HTMLInputElement>(null);
  const referenceImageInputRef = useRef<HTMLInputElement>(null);
  const dragOrigin = useRef<{ x: number; y: number } | null>(null);
  const dragging = useRef(false);

  const selectedProvider = useMemo(() => providers.find((provider) => provider.id === providerId), [providerId, providers]);

  const loadProviders = useCallback(async () => {
    const list = await invoke<Provider[]>("list_providers");
    setProviders(list);
    setProviderId((current) => current && list.some((provider) => provider.id === current) ? current : list.find((provider) => provider.isDefault)?.id ?? list[0]?.id);
  }, []);

  useEffect(() => {
    void loadProviders();
  }, [loadProviders]);

  const loadImageConfig = useCallback(async () => {
    const config = await invoke<ImageGenerationConfig>("get_image_generation_config");
    setImageConfig(config);
    return config;
  }, []);

  useEffect(() => {
    void loadImageConfig().catch(() => setImageConfig(null));
  }, [loadImageConfig]);

  useEffect(() => {
    void Promise.all([
      invoke<boolean>("get_context_enabled").catch(() => true),
      invoke<boolean>("get_web_search_enabled").catch(() => false),
      invoke<boolean>("get_computer_control_enabled").catch(() => false),
      getCurrentWindow().isMaximized().catch(() => false),
    ]).then(([context, webSearch, computerControl, maximized]) => {
      setContextEnabled(context);
      setWebSearchEnabled(webSearch);
      setComputerControlEnabled(computerControl);
      setIsMaximized(maximized);
    });
  }, []);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [isParsing, isSending, messages]);

  useEffect(() => {
    if (isMaximized) return;
    const longestReply = Math.max(0, ...messages.filter((message) => message.role === "assistant").map((message) => message.content.length));
    const growth = Math.min(1, Math.max(0, (longestReply - 900) / 4000));
    void invoke("set_chat_size", { width: 610 + growth * 260, height: 720 + growth * 200 });
  }, [isMaximized, messages]);

  const dimEdge = useCallback(() => {
    void emitTo("edge", "kero:edge-state", { active: false, energy: 0 });
    window.setTimeout(() => void invoke("hide_edge"), 420);
  }, []);

  const send = useCallback(async (rawMessage: string) => {
    const content = rawMessage.trim();
    if ((!content && chatAttachments.length === 0) || isSending || isParsing) return;
    setIsParsing(true);

    let computerTask = false;
    try {
      computerTask = await shouldRunComputerTask(content);
    } catch {
      computerTask = false;
    } finally {
      setIsParsing(false);
    }

    if (computerTask && chatAttachments.length === 0) {
      await invoke("show_main");
      await emitTo("main", "kero:computer-task", { text: content });
      await invoke("hide_window", { label: "chat" });
      return;
    }
    const textAttachments = chatAttachments.filter((attachment) => attachment.kind === "text" && attachment.text);
    const modelContent = `${content || "请分析附件。"}${textAttachments.map((attachment) => `\n\n[附件：${attachment.name}]\n${attachment.text}`).join("")}`;
    const userMessage: ChatMessage = {
      ...createMessage("user", content || "已附加文件"),
      modelContent,
      attachments: chatAttachments,
    };
    const imageAttachments = chatAttachments
      .filter((attachment) => attachment.kind === "image" && attachment.dataUrl)
      .map((attachment) => ({ name: attachment.name, mimeType: attachment.mimeType, dataUrl: attachment.dataUrl }));
    const nextMessages = [...messages, userMessage];
    setMessages(nextMessages);
    setDraft("");
    setChatAttachments([]);
    setIsSending(true);
    void emitTo("edge", "kero:edge-state", { active: true, energy: 0.16 });

    try {
      if (!providerId) throw new Error("请先在设置中添加并启用一个模型服务");
      const webSearch = await invoke<boolean>("get_web_search_enabled").catch(() => false);
      const response = await invoke<string>("chat_completion", {
        request: {
          providerId,
          messages: nextMessages.filter((message) => message.id !== "welcome").map(({ role, content: itemContent, modelContent: itemModelContent }) => ({ role, content: itemModelContent ?? itemContent })),
          attachments: imageAttachments,
          webSearch,
        },
      });
      setMessages((current) => [...current, createMessage("assistant", response)]);
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      setMessages((current) => [...current, createMessage("assistant", `无法完成这次请求：${detail}`)]);
    } finally {
      setIsSending(false);
      dimEdge();
    }
  }, [chatAttachments, dimEdge, isParsing, isSending, messages, providerId]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<{ text: string }>("kero:prompt", ({ payload }) => void send(payload.text)).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, [send]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<{ providerId: string }>("kero:providers-changed", ({ payload }) => {
      setProviderId(payload.providerId);
      void loadProviders();
    }).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, [loadProviders]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<{ messages: ChatMessage[] }>("kero:inline-history", ({ payload }) => {
      setMessages(payload.messages.length ? payload.messages : [welcome]);
    }).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ScreenTranslationStatus>("kero:screen-translation-status", ({ payload }) => {
      setTranslationStatus(payload);
      if (payload.error) setQuickNotice(payload.error);
    }).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, []);

  const close = () => {
    setIsLeaving(true);
    window.setTimeout(() => {
      // Reset before hide because a hidden WebView may stop running JavaScript.
      flushSync(() => setIsLeaving(false));
      void invoke("hide_window", { label: "chat" });
    }, 190);
  };

  const toggleMaximize = useCallback(async () => {
    const maximized = await invoke<boolean>("toggle_chat_maximize");
    setIsMaximized(maximized);
  }, []);

  const toggleContext = useCallback(async () => {
    const enabled = !contextEnabled;
    await invoke("set_context_enabled", { contextEnabled: enabled });
    setContextEnabled(enabled);
    setQuickNotice(enabled ? "上下文已开启" : "上下文已关闭");
  }, [contextEnabled]);

  const toggleWebSearch = useCallback(async () => {
    if (webSearchEnabled) {
      await invoke("set_web_search_enabled", { webSearchEnabled: false });
      setWebSearchEnabled(false);
      setQuickNotice("联网搜索已关闭");
      return;
    }
    const availability = await invoke<{ supported: boolean; reason: string }>("web_search_availability", { providerId });
    if (!availability.supported) {
      setQuickNotice(`联网搜索不可用：${availability.reason}`);
      return;
    }
    await invoke("set_web_search_enabled", { webSearchEnabled: true });
    setWebSearchEnabled(true);
    setQuickNotice("联网搜索已开启");
  }, [providerId, webSearchEnabled]);

  const toggleComputerControl = useCallback(async () => {
    const enabled = !computerControlEnabled;
    await invoke("set_computer_control_enabled", { computerControlEnabled: enabled });
    setComputerControlEnabled(enabled);
    setQuickNotice(enabled ? "AI 操控电脑已开启" : "AI 操控电脑已关闭");
  }, [computerControlEnabled]);

  const toggleTranslation = useCallback(async () => {
    if (translationStatus.active || translationStatus.loading) {
      await invoke("stop_screen_translation");
      setTranslationStatus({ active: false, loading: false });
      setQuickNotice("屏幕翻译已关闭");
      return;
    }
    setQuickNotice("");
    await invoke("start_screen_translation");
    setTranslationStatus({ active: true, loading: true });
  }, [translationStatus.active, translationStatus.loading]);

  const openImageGenerator = useCallback(async () => {
    setImageNotice("");
    setShowImageGenerator((open) => !open);
    try {
      await loadImageConfig();
    } catch (error) {
      setImageNotice(error instanceof Error ? error.message : String(error));
    }
  }, [loadImageConfig]);

  const addChatAttachments = useCallback(async (event: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files ?? []);
    event.target.value = "";
    if (!files.length) return;
    const available = Math.max(0, MAX_CHAT_ATTACHMENTS - chatAttachments.length);
    if (available === 0) {
      setQuickNotice("一次对话最多附加 4 个文件");
      return;
    }
    const next: ChatAttachment[] = [];
    const notices: string[] = [];
    for (const file of files.slice(0, available)) {
      try {
        if (file.type.startsWith("image/")) {
          if (file.size > MAX_IMAGE_BYTES) throw new Error(`${file.name} 超过 8 MB 图片限制`);
          next.push({ id: crypto.randomUUID(), name: file.name, mimeType: file.type || "image/png", kind: "image", dataUrl: await fileDataUrl(file) });
        } else if (isTextFile(file)) {
          if (file.size > MAX_TEXT_FILE_BYTES) throw new Error(`${file.name} 超过 2 MB 文本文件限制`);
          next.push({ id: crypto.randomUUID(), name: file.name, mimeType: file.type || "text/plain", kind: "text", text: await fileText(file) });
        } else {
          throw new Error(`${file.name} 暂不支持。可附加图片或常见文本、代码、JSON、CSV 文件`);
        }
      } catch (error) {
        notices.push(error instanceof Error ? error.message : String(error));
      }
    }
    if (next.length) setChatAttachments((current) => [...current, ...next]);
    if (notices.length) setQuickNotice(notices.join("；"));
  }, [chatAttachments.length]);

  const addReferenceImages = useCallback(async (event: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files ?? []);
    event.target.value = "";
    const available = Math.max(0, MAX_REFERENCE_IMAGES - referenceImages.length);
    if (available === 0) {
      setImageNotice("最多使用 4 张参考图片");
      return;
    }
    const next: ChatAttachment[] = [];
    for (const file of files.slice(0, available)) {
      if (!file.type.startsWith("image/")) {
        setImageNotice(`${file.name} 不是图片文件`);
        continue;
      }
      if (file.size > MAX_IMAGE_BYTES) {
        setImageNotice(`${file.name} 超过 8 MB 图片限制`);
        continue;
      }
      next.push({ id: crypto.randomUUID(), name: file.name, mimeType: file.type || "image/png", kind: "image", dataUrl: await fileDataUrl(file) });
    }
    if (next.length) setReferenceImages((current) => [...current, ...next]);
  }, [referenceImages.length]);

  const generateImage = useCallback(async (prompt = imagePrompt, materials = referenceImages) => {
    const normalizedPrompt = prompt.trim();
    if (!normalizedPrompt || isGeneratingImage) return;
    setImageNotice("");
    setIsGeneratingImage(true);
    try {
      const images = await invoke<GeneratedImage[]>("generate_image", {
        request: {
          prompt: normalizedPrompt,
          aspectRatio: imageAspectRatio,
          count: imageCount,
          referenceImages: materials.flatMap((image) => image.dataUrl ? [image.dataUrl] : []),
        },
      });
      if (materials.length) {
        const materialSnapshot = [...materials];
        setGeneratedImageReferences((current) => ({
          ...current,
          ...Object.fromEntries(images.map((image) => [image.path, materialSnapshot])),
        }));
      }
      setMessages((current) => [
        ...current,
        ...images.map((image) => ({
          id: crypto.randomUUID(),
          role: "assistant" as const,
          kind: "image" as const,
          content: image.prompt,
          imagePath: image.path,
        })),
      ]);
      setImagePrompt("");
      setReferenceImages([]);
      setShowImageGenerator(false);
      void loadImageConfig();
    } catch (error) {
      setImageNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setIsGeneratingImage(false);
    }
  }, [imageAspectRatio, imageCount, imagePrompt, isGeneratingImage, loadImageConfig, referenceImages]);

  const optimizeImagePrompt = useCallback(async () => {
    const prompt = imagePrompt.trim();
    if (!prompt || !providerId || isOptimizingImagePrompt || isGeneratingImage) return;
    setImageNotice("");
    setIsOptimizingImagePrompt(true);
    try {
      const optimized = await invoke<string>("optimize_image_prompt", {
        request: {
          providerId,
          prompt,
          referenceImages: referenceImages.flatMap((image) => image.dataUrl ? [image.dataUrl] : []),
        },
      });
      if (optimized.trim()) setImagePrompt(optimized.trim());
    } catch (error) {
      setImageNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setIsOptimizingImagePrompt(false);
    }
  }, [imagePrompt, isGeneratingImage, isOptimizingImagePrompt, providerId, referenceImages]);

  const downloadGeneratedImage = useCallback(async (path: string) => {
    if (downloadingImagePath) return;
    setDownloadingImagePath(path);
    try {
      await invoke<string>("download_generated_image", { path });
      setDownloadNoticePath(path);
    } catch (error) {
      setImageNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setDownloadingImagePath(null);
    }
  }, [downloadingImagePath]);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    void send(draft);
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void send(draft);
    }
  };

  const startDragging = (event: PointerEvent<HTMLElement>) => {
    const target = event.target as HTMLElement;
    if (event.button === 0 && !target.closest("button, input, select, textarea")) {
      dragOrigin.current = { x: event.clientX, y: event.clientY };
      dragging.current = false;
    }
  };

  const continueDragging = (event: PointerEvent<HTMLElement>) => {
    if (!dragOrigin.current || dragging.current || !(event.buttons & 1)) return;
    const distance = Math.hypot(event.clientX - dragOrigin.current.x, event.clientY - dragOrigin.current.y);
    if (distance > 4) {
      dragging.current = true;
      void getCurrentWindow().startDragging().catch(console.error);
    }
  };

  const finishDragging = () => {
    dragOrigin.current = null;
    dragging.current = false;
  };

  const updateGlass = (event: PointerEvent<HTMLElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect();
    surfaceRef.current?.style.setProperty("--glass-x", `${((event.clientX - bounds.left) / bounds.width) * 100}%`);
    surfaceRef.current?.style.setProperty("--glass-y", `${((event.clientY - bounds.top) / bounds.height) * 100}%`);
  };

  return (
    <main ref={surfaceRef} className={`surface-window chat-window ${isMaximized ? "is-maximized" : ""} ${isLeaving ? "is-leaving" : ""}`} onPointerMove={updateGlass}>
      <header className="window-titlebar" onDoubleClick={(event) => {
        if (!(event.target as HTMLElement).closest("button")) void toggleMaximize();
      }} onPointerDown={startDragging} onPointerMove={continueDragging} onPointerUp={finishDragging}>
        <div className="brand-line" data-tauri-drag-region>
          <span className="small-orb"><Sparkles size={14} /></span>
          <span>Kero</span>
          <span className="quiet-label">对话</span>
        </div>
        <div className="window-controls">
          <button type="button" title="最小化到任务栏" onClick={() => void invoke("minimize_window", { label: "chat" })}><Minus size={16} /></button>
          <button type="button" title={isMaximized ? "还原窗口" : "最大化"} onClick={() => void toggleMaximize()}>{isMaximized ? <Minimize2 size={15} /> : <Maximize2 size={15} />}</button>
          <button type="button" title="关闭对话" className="close" onClick={close}><X size={16} /></button>
        </div>
      </header>

      <div className="chat-toolbar">
        <div className="provider-select-wrap">
          <select value={providerId ?? ""} onChange={(event) => setProviderId(event.target.value)} aria-label="选择模型服务">
            {providers.length === 0 && <option value="">尚未配置模型</option>}
            {providers.map((provider) => <option key={provider.id} value={provider.id}>{provider.name} · {provider.model}</option>)}
          </select>
          <ChevronDown size={15} />
        </div>
        <div className="chat-toolbar-actions">
          <button type="button" className={`image-generator-toggle ${showImageGenerator ? "is-open" : ""}`} title="图片生成" aria-expanded={showImageGenerator} onClick={() => void openImageGenerator()}><ImagePlus size={16} /></button>
          <button type="button" className={`quick-controls-toggle ${showQuickControls ? "is-open" : ""}`} title="快速控制" aria-expanded={showQuickControls} onClick={() => setShowQuickControls((open) => !open)}><SlidersHorizontal size={16} /></button>
          <button type="button" className="new-chat" title="新建对话" onClick={() => setMessages([welcome])}><MessageSquarePlus size={16} /></button>
        </div>
      </div>

      {showImageGenerator && <section className="image-generator-panel" aria-label="图片生成">
        <div className="image-generator-heading">
          <div><b>图片生成</b><small>{imageConfig?.model ? `模型：${imageConfig.model}` : "请先完成图片生成配置"}</small></div>
          <button type="button" title="打开图片生成设置" onClick={() => void invoke("open_settings")}>前往设置</button>
        </div>
        <textarea value={imagePrompt} onChange={(event) => setImagePrompt(event.target.value)} placeholder="描述你想生成的画面" rows={2} maxLength={6000} disabled={isGeneratingImage || isOptimizingImagePrompt} />
        <input ref={referenceImageInputRef} className="file-input-hidden" type="file" accept="image/*" multiple onChange={(event) => void addReferenceImages(event)} />
        <div className="reference-image-row">
          <button type="button" className="add-reference-image" disabled={isGeneratingImage || isOptimizingImagePrompt || referenceImages.length >= MAX_REFERENCE_IMAGES} onClick={() => referenceImageInputRef.current?.click()}><ImagePlus size={15} /> 导入图片素材</button>
          {referenceImages.map((image) => <div className="reference-image-chip" key={image.id}><img src={image.dataUrl} alt={image.name} /><span>{image.name}</span><button type="button" title="移除图片素材" onClick={() => setReferenceImages((current) => current.filter((item) => item.id !== image.id))}><X size={12} /></button></div>)}
        </div>
        <p className="reference-image-hint">最多 4 张。可以作为参考，也可以在描述中要求融合多张素材。</p>
        <div className="image-generator-options">
          <div className="image-option"><span>比例</span><div className="image-size-picker" role="group" aria-label="图片比例">
            {["1:1", "16:9", "9:16"].map((ratio) => <button type="button" key={ratio} className={imageAspectRatio === ratio ? "selected" : ""} onClick={() => setImageAspectRatio(ratio)} disabled={isGeneratingImage || isOptimizingImagePrompt}>{ratio}</button>)}
          </div></div>
          <div className="image-option"><span>数量</span><div className="image-size-picker" role="group" aria-label="生成数量">
            {[1, 2, 3, 4].map((count) => <button type="button" key={count} className={imageCount === count ? "selected" : ""} onClick={() => setImageCount(count)} disabled={isGeneratingImage || isOptimizingImagePrompt}>{count}</button>)}
          </div></div>
        </div>
        <div className="image-generator-footer">
          <button type="button" className="optimize-image-prompt" title="使用当前语言模型优化提示词" disabled={!imagePrompt.trim() || !providerId || isGeneratingImage || isOptimizingImagePrompt} onClick={() => void optimizeImagePrompt()}>{isOptimizingImagePrompt ? <RefreshCw className="is-spinning" size={15} /> : <WandSparkles size={15} />}<span>{isOptimizingImagePrompt ? "正在优化" : "优化提示词"}</span></button>
          <button type="button" className="generate-image-button" title="生成图片" disabled={!imagePrompt.trim() || !imageConfig?.model || !imageConfig?.hasKey || isGeneratingImage || isOptimizingImagePrompt} onClick={() => void generateImage()}>{isGeneratingImage ? <RefreshCw className="is-spinning" size={16} /> : <Sparkles size={16} />}<span>{isGeneratingImage ? "正在生成" : "生成"}</span></button>
        </div>
        {imageNotice && <p className="image-generator-notice">{imageNotice}</p>}
      </section>}

      {showQuickControls && <section className="chat-quick-controls" aria-label="快速控制">
        <button type="button" className={contextEnabled ? "is-on" : ""} onClick={() => void toggleContext()}><Sparkles size={15} /><span>上下文</span><i aria-hidden="true" /></button>
        <button type="button" className={webSearchEnabled ? "is-on" : ""} onClick={() => void toggleWebSearch()}><Globe2 size={15} /><span>联网搜索</span><i aria-hidden="true" /></button>
        <button type="button" className={computerControlEnabled ? "is-on" : ""} onClick={() => void toggleComputerControl()}><MousePointer2 size={15} /><span>电脑操控</span><i aria-hidden="true" /></button>
        <button type="button" className={translationStatus.active || translationStatus.loading ? "is-on" : ""} onClick={() => void toggleTranslation()}><Languages size={15} /><span>{translationStatus.loading ? "翻译中" : "全屏翻译"}</span><i aria-hidden="true" /></button>
        {quickNotice && <p className="quick-control-notice">{quickNotice}</p>}
      </section>}

      <section className="message-list" ref={scrollRef} aria-live="polite">
        {messages.map((message) => (
          <article className={`message ${message.role}`} key={message.id}>
            {message.role === "assistant" && <span className="message-avatar"><Bot size={15} /></span>}
            <div className="message-content">
              {message.kind === "image" && message.imagePath
                ? <div className="generated-image-message">
                  <img src={convertFileSrc(message.imagePath)} alt={message.content} />
                  <p>{message.content}</p>
                  <div className="generated-image-actions">
                    <button type="button" title="保存到下载文件夹" disabled={downloadingImagePath === message.imagePath} onClick={() => void downloadGeneratedImage(message.imagePath!)}><Download size={14} /> {downloadingImagePath === message.imagePath ? "正在保存" : "下载"}</button>
                    <button type="button" title="重新生成" disabled={isGeneratingImage} onClick={() => void generateImage(message.content, generatedImageReferences[message.imagePath!] ?? [])}><RefreshCw size={14} /> 重做</button>
                  </div>
                  {downloadNoticePath === message.imagePath && <small className="generated-image-notice">已保存到下载文件夹</small>}
                </div>
                : message.role === "assistant"
                ? <div className="markdown-content"><ReactMarkdown remarkPlugins={[remarkGfm]} components={{
                  a: ({ href, children }) => <a href={href} onClick={(event) => {
                    event.preventDefault();
                    if (href && /^https?:\/\//i.test(href)) void openUrl(href).catch(console.error);
                  }}>{children}</a>,
                }}>{message.content}</ReactMarkdown></div>
                : <>{message.attachments?.length ? <div className="message-attachments">{message.attachments.map((attachment) => attachment.kind === "image" && attachment.dataUrl
                  ? <img key={attachment.id} src={attachment.dataUrl} alt={attachment.name} title={attachment.name} />
                  : <span key={attachment.id}><Paperclip size={12} />{attachment.name}</span>)}</div> : null}<p>{message.content}</p></>}
              {message.role === "assistant" && message.id !== "welcome" && message.kind !== "image" && (
                <button className="copy-message" type="button" title="复制" onClick={() => void navigator.clipboard.writeText(message.content)}><Copy size={13} /></button>
              )}
            </div>
          </article>
        ))}
        {isParsing && (
          <article className="message assistant is-parsing">
            <span className="message-avatar"><Sparkles size={14} /></span>
            <div className="message-content parsing-status"><span>正在解析请求</span><i /><i /><i /></div>
          </article>
        )}
        {isSending && (
          <article className="message assistant is-thinking">
            <span className="message-avatar"><Bot size={15} /></span>
            <div className="message-content thinking-dots"><i /><i /><i /></div>
          </article>
        )}
        {isGeneratingImage && (
          <article className="message assistant is-thinking">
            <span className="message-avatar"><ImagePlus size={15} /></span>
            <div className="message-content parsing-status"><span>正在生成图片</span><i /><i /><i /></div>
          </article>
        )}
      </section>

      {!selectedProvider && <div className="setup-reminder"><CircleAlert size={15} /> 请先在设置中配置模型服务</div>}

      <form className="chat-composer" onSubmit={submit}>
        <input ref={chatAttachmentInputRef} className="file-input-hidden" type="file" multiple onChange={(event) => void addChatAttachments(event)} />
        {chatAttachments.length > 0 && <div className="chat-attachment-list">{chatAttachments.map((attachment) => <span key={attachment.id}>{attachment.kind === "image" ? <ImagePlus size={12} /> : <Paperclip size={12} />}{attachment.name}<button type="button" title="移除附件" onClick={() => setChatAttachments((current) => current.filter((item) => item.id !== attachment.id))}><X size={12} /></button></span>)}</div>}
        <button className="chat-attachment-button" type="button" title="添加图片或文件" onClick={() => chatAttachmentInputRef.current?.click()} disabled={isSending || isParsing || chatAttachments.length >= MAX_CHAT_ATTACHMENTS}><Paperclip size={16} /></button>
        <textarea value={draft} onChange={(event) => setDraft(event.target.value)} onKeyDown={handleKeyDown} placeholder="输入消息" rows={1} aria-label="消息输入框" />
        <button type="submit" title="发送消息" disabled={(!draft.trim() && chatAttachments.length === 0) || isSending || isParsing}><Send size={17} /></button>
      </form>
    </main>
  );
}
