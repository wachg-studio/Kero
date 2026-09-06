import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Check, ChevronDown, Cpu, Globe2, ImagePlus, KeyRound, Languages, Laptop, Mic2, Minus, MousePointer2, Plus, Power, Save, ShieldCheck, SlidersHorizontal, Trash2, TriangleAlert, Waves, X } from "lucide-react";
import { FormEvent, PointerEvent, ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import type { ImageGenerationConfig, Provider, ProviderDraft, ProviderKind } from "../types";

const initialDraft: ProviderDraft = {
  name: "",
  kind: "openai",
  baseUrl: "",
  model: "",
  apiKey: "",
};

const providerKinds: { value: ProviderKind; label: string; hint: string }[] = [
  { value: "openai", label: "OpenAI", hint: "GPT 系列或兼容 Chat Completions 接口" },
  { value: "anthropic", label: "Anthropic", hint: "Claude Messages API" },
  { value: "google", label: "Google Gemini", hint: "Gemini GenerateContent API" },
  { value: "compatible", label: "自定义兼容服务", hint: "Ollama、LM Studio、DeepSeek 等" },
];

const defaultUrl: Record<ProviderKind, string> = {
  openai: "https://api.openai.com",
  anthropic: "https://api.anthropic.com",
  google: "https://generativelanguage.googleapis.com",
  compatible: "",
};

type CapsuleSize = "compact" | "standard" | "wide";
type EdgeColorMode = "rainbow" | "blue";
type DictationRecognitionMode = "native" | "ai";

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

type AppearanceSwitchProps = {
  enabled: boolean;
  onToggle: () => void;
  children: ReactNode;
};

type WebSearchAvailability = {
  supported: boolean;
  reason: string;
};

type TranslationSettings = {
  sourceLanguage: string;
  targetLanguage: string;
};

type ImageGenerationDraft = ImageGenerationConfig & { apiKey: string };
type DictationAsrConfig = { baseUrl: string; model: string; hasKey: boolean };
type DictationAsrDraft = DictationAsrConfig & { apiKey: string };

const initialImageGenerationDraft: ImageGenerationDraft = {
  provider: "openai",
  baseUrl: "",
  model: "",
  hasKey: false,
  apiKey: "",
};

const initialDictationAsrDraft: DictationAsrDraft = {
  baseUrl: "",
  model: "",
  hasKey: false,
  apiKey: "",
};

const translationLanguages = [
  { value: "auto", label: "自动检测" },
  { value: "en", label: "英语" },
  { value: "zh-CN", label: "简体中文" },
  { value: "ja", label: "日语" },
  { value: "ko", label: "韩语" },
  { value: "fr", label: "法语" },
  { value: "de", label: "德语" },
  { value: "es", label: "西班牙语" },
  { value: "ru", label: "俄语" },
];

function AppearanceSwitch({ enabled, onToggle, children }: AppearanceSwitchProps) {
  return (
    <button
      className="appearance-switch appearance-switch-button"
      type="button"
      role="switch"
      aria-checked={enabled}
      onClick={onToggle}
    >
      {children}
      <i className={enabled ? "is-on" : ""} aria-hidden="true"><em /></i>
    </button>
  );
}

function readAppearance(): Appearance {
  try {
    const saved = JSON.parse(window.localStorage.getItem("kero-appearance") ?? "{}") as Partial<Appearance>;
    return {
      opacity: Number.isFinite(Number(saved.opacity)) ? Math.min(100, Math.max(25, Number(saved.opacity))) : defaultAppearance.opacity,
      glassEnabled: saved.glassEnabled !== false,
      edgeEnabled: saved.edgeEnabled !== false,
      autoSend: saved.autoSend !== false,
      dictationCorrection: saved.dictationCorrection !== false,
      dictationMemory: saved.dictationMemory !== false,
      dictationEdgeEnabled: saved.dictationEdgeEnabled !== false,
      dictationMode: saved.dictationMode === "native" ? "native" : "ai",
      aiDictationPolish: saved.aiDictationPolish === true,
      realtimeEdgeEnabled: saved.realtimeEdgeEnabled !== false,
      clickThrough: saved.clickThrough === true,
      positionLocked: saved.positionLocked === true,
      size: saved.size === "compact" || saved.size === "wide" ? saved.size : "standard",
      edgeColor: saved.edgeColor === "blue" ? "blue" : "rainbow",
    };
  } catch {
    return defaultAppearance;
  }
}

function kindLabel(kind: ProviderKind) {
  return providerKinds.find((entry) => entry.value === kind)?.label ?? kind;
}

export function SettingsWindow() {
  const [providers, setProviders] = useState<Provider[]>([]);
  const [draft, setDraft] = useState<ProviderDraft>(initialDraft);
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState("");
  const [isLeaving, setIsLeaving] = useState(false);
  const [appearance, setAppearance] = useState<Appearance>(readAppearance);
  const [dictationVocabulary, setDictationVocabulary] = useState(
    () => window.localStorage.getItem("kero-dictation-vocabulary") ?? "",
  );
  const [systemPrompt, setSystemPrompt] = useState("");
  const [contextEnabled, setContextEnabled] = useState(true);
  const [webSearchEnabled, setWebSearchEnabled] = useState(false);
  const [webSearchProxyEnabled, setWebSearchProxyEnabled] = useState(false);
  const [checkingWebSearch, setCheckingWebSearch] = useState(false);
  const [computerControlEnabled, setComputerControlEnabled] = useState(false);
  const [computerRiskMode, setComputerRiskMode] = useState(false);
  const [autostartEnabled, setAutostartEnabled] = useState(false);
  const [savingAutostart, setSavingAutostart] = useState(false);
  const [translationSettings, setTranslationSettings] = useState<TranslationSettings>({ sourceLanguage: "auto", targetLanguage: "zh-CN" });
  const [imageGeneration, setImageGeneration] = useState<ImageGenerationDraft>(initialImageGenerationDraft);
  const [savingImageGeneration, setSavingImageGeneration] = useState(false);
  const [imageGenerationNotice, setImageGenerationNotice] = useState("");
  const [dictationAsr, setDictationAsr] = useState<DictationAsrDraft>(initialDictationAsrDraft);
  const [savingDictationAsr, setSavingDictationAsr] = useState(false);
  const [dictationAsrNotice, setDictationAsrNotice] = useState("");
  const surfaceRef = useRef<HTMLElement>(null);
  const dragOrigin = useRef<{ x: number; y: number } | null>(null);
  const dragging = useRef(false);
  const appearanceRef = useRef(appearance);

  const loadProviders = useCallback(async () => {
    const list = await invoke<Provider[]>("list_providers");
    setProviders(list);
  }, []);

  useEffect(() => {
    void loadProviders();
  }, [loadProviders]);

  useEffect(() => {
    void invoke<string>("get_system_prompt").then(setSystemPrompt).catch(() => setSystemPrompt(""));
    void invoke<boolean>("get_context_enabled").then(setContextEnabled).catch(() => setContextEnabled(true));
    void invoke<boolean>("get_web_search_enabled").then(setWebSearchEnabled).catch(() => setWebSearchEnabled(false));
    void invoke<boolean>("get_web_search_proxy_enabled").then(setWebSearchProxyEnabled).catch(() => setWebSearchProxyEnabled(false));
    void invoke<boolean>("get_computer_control_enabled").then(setComputerControlEnabled).catch(() => setComputerControlEnabled(false));
    void invoke<boolean>("get_computer_control_risk_mode").then(setComputerRiskMode).catch(() => setComputerRiskMode(false));
    void invoke<boolean>("get_autostart_enabled").then(setAutostartEnabled).catch(() => setAutostartEnabled(false));
    void invoke<TranslationSettings>("get_screen_translation_settings")
      .then(setTranslationSettings)
      .catch(() => setTranslationSettings({ sourceLanguage: "auto", targetLanguage: "zh-CN" }));
    void invoke<ImageGenerationConfig>("get_image_generation_config")
      .then((config) => setImageGeneration({ ...config, apiKey: "" }))
      .catch(() => setImageGeneration(initialImageGenerationDraft));
    void invoke<DictationAsrConfig>("get_dictation_asr_config")
      .then((config) => setDictationAsr({ ...config, apiKey: "" }))
      .catch(() => setDictationAsr(initialDictationAsrDraft));
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<Appearance>("kero:appearance-changed", ({ payload }) => {
      appearanceRef.current = payload;
      setAppearance(payload);
    }).then((listener) => {
      unlisten = listener;
    });
    return () => unlisten?.();
  }, []);

  const changeKind = (kind: ProviderKind) => {
    setDraft((current) => ({ ...current, kind, baseUrl: current.id ? current.baseUrl : defaultUrl[kind] }));
  };

  const edit = (provider: Provider) => {
    setNotice("");
    setDraft({ id: provider.id, name: provider.name, kind: provider.kind, baseUrl: provider.baseUrl, model: provider.model, apiKey: "" });
  };

  const resetDraft = () => {
    setNotice("");
    setDraft(initialDraft);
  };

  const save = async (event: FormEvent) => {
    event.preventDefault();
    setSaving(true);
    setNotice("");
    try {
      const saved = await invoke<Provider>("save_provider", { provider: draft });
      if (!saved.hasKey) throw new Error("API Key 未通过保存验证");
      await loadProviders();
      await emitTo("chat", "kero:providers-changed", { providerId: saved.id });
      setNotice("已保存。API Key 已交由 Windows 凭据存储保护。");
      setDraft((current) => ({ ...current, apiKey: "" }));
    } catch (error) {
      setNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (provider: Provider) => {
    if (!window.confirm(`删除“${provider.name}”及其本机密钥？`)) return;
    await invoke("delete_provider", { providerId: provider.id });
    if (draft.id === provider.id) resetDraft();
    await loadProviders();
  };

  const close = () => {
    setIsLeaving(true);
    window.setTimeout(() => {
      // A hidden WebView can suspend timers, so clear the exit state before hiding it.
      flushSync(() => setIsLeaving(false));
      void invoke("hide_window", { label: "settings" });
    }, 190);
  };

  const saveImageGeneration = async () => {
    setSavingImageGeneration(true);
    setImageGenerationNotice("");
    try {
      const saved = await invoke<ImageGenerationConfig>("save_image_generation_config", {
        config: imageGeneration,
      });
      setImageGeneration({ ...saved, apiKey: "" });
      setImageGenerationNotice("图片生成配置已保存，密钥已单独加密保护。");
    } catch (error) {
      setImageGenerationNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setSavingImageGeneration(false);
    }
  };

  const saveDictationAsr = async () => {
    setSavingDictationAsr(true);
    setDictationAsrNotice("");
    try {
      const saved = await invoke<DictationAsrConfig>("save_dictation_asr_config", {
        config: dictationAsr,
      });
      setDictationAsr({ ...saved, apiKey: "" });
      setDictationAsrNotice("AI 语音识别配置已保存，密钥已单独加密保护");
    } catch (error) {
      setDictationAsrNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setSavingDictationAsr(false);
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

  const updateAppearance = (patch: Partial<Appearance>) => {
    const current = appearanceRef.current;
    const next: Appearance = {
      ...current,
      ...patch,
      opacity: Number.isFinite(Number(patch.opacity ?? current.opacity))
        ? Math.min(100, Math.max(25, Number(patch.opacity ?? current.opacity)))
        : current.opacity,
    };
    appearanceRef.current = next;
    window.localStorage.setItem("kero-appearance", JSON.stringify(next));
    window.localStorage.setItem("kero-capsule-opacity", String(next.opacity));
    window.localStorage.setItem("kero-glass-enabled", String(next.glassEnabled));
    setAppearance(next);
    void emitTo("main", "kero:appearance-request", next);
  };

  const updateDictationVocabulary = (value: string) => {
    setDictationVocabulary(value);
    window.localStorage.setItem("kero-dictation-vocabulary", value);
  };

  const clearDictationMemory = () => {
    window.localStorage.removeItem("kero-dictation-memory");
    setNotice("已清除听写记忆");
  };

  const updateGlass = (event: PointerEvent<HTMLElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect();
    surfaceRef.current?.style.setProperty("--glass-x", `${((event.clientX - bounds.left) / bounds.width) * 100}%`);
    surfaceRef.current?.style.setProperty("--glass-y", `${((event.clientY - bounds.top) / bounds.height) * 100}%`);
  };

  const saveSystemPrompt = async () => {
    await invoke("set_system_prompt", { systemPrompt });
    setNotice("系统提示词已保存");
  };

  const toggleContext = async () => {
    const enabled = !contextEnabled;
    await invoke("set_context_enabled", { contextEnabled: enabled });
    setContextEnabled(enabled);
  };

  const toggleWebSearch = async () => {
    if (checkingWebSearch) return;
    if (webSearchEnabled) {
      await invoke("set_web_search_enabled", { webSearchEnabled: false });
      setWebSearchEnabled(false);
      setNotice("联网搜索已关闭");
      return;
    }

    setCheckingWebSearch(true);
    setNotice("");
    try {
      const availability = await invoke<WebSearchAvailability>("web_search_availability");
      if (!availability.supported) {
        await invoke("set_web_search_enabled", { webSearchEnabled: false }).catch(() => undefined);
        setWebSearchEnabled(false);
        setNotice(`联网搜索未开启：${availability.reason}`);
        return;
      }
      await invoke("set_web_search_enabled", { webSearchEnabled: true });
      setWebSearchEnabled(true);
      setNotice("联网搜索已开启，将使用当前默认模型的原生搜索能力。");
    } catch (error) {
      setWebSearchEnabled(false);
      setNotice(`联网搜索未开启：${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setCheckingWebSearch(false);
    }
  };

  const toggleWebSearchProxy = async () => {
    if (checkingWebSearch) return;
    setCheckingWebSearch(true);
    setNotice("");
    try {
      if (webSearchProxyEnabled) {
        await invoke("set_web_search_proxy_enabled", { webSearchProxyEnabled: false });
        setWebSearchProxyEnabled(false);
        setWebSearchEnabled(false);
        setNotice("中转站联网搜索授权已关闭，联网搜索也已同步关闭。");
      } else {
        await invoke("set_web_search_proxy_enabled", { webSearchProxyEnabled: true });
        setWebSearchProxyEnabled(true);
        setNotice("中转站已通过模型原生联网能力验证。现在可开启联网搜索。");
      }
    } catch (error) {
      setWebSearchProxyEnabled(false);
      setNotice(`中转站联网搜索未开启：${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setCheckingWebSearch(false);
    }
  };

  const toggleComputerControl = async () => {
    const enabled = !computerControlEnabled;
    await invoke("set_computer_control_enabled", { computerControlEnabled: enabled });
    setComputerControlEnabled(enabled);
    if (!enabled) setComputerRiskMode(false);
    setNotice(enabled ? "AI 操控电脑已开启。" : "AI 操控电脑已关闭。风险模式已同步关闭。");
  };

  const toggleComputerRiskMode = async () => {
    const enabled = !computerRiskMode;
    try {
      await invoke("set_computer_control_risk_mode", { computerControlRiskMode: enabled });
      setComputerRiskMode(enabled);
      setNotice(enabled ? "风险模式已开启，AI 操作不再等待确认。" : "风险模式已关闭，重要操作会请求确认。");
    } catch (error) {
      setNotice(error instanceof Error ? error.message : String(error));
    }
  };

  const toggleAutostart = async () => {
    if (savingAutostart) return;
    const enabled = !autostartEnabled;
    setSavingAutostart(true);
    try {
      await invoke("set_autostart_enabled", { enabled });
      setAutostartEnabled(enabled);
      setNotice(enabled ? "开机自启已开启，登录 Windows 后 Kero 会静默进入托盘。" : "开机自启已关闭。");
    } catch (error) {
      setNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setSavingAutostart(false);
    }
  };

  const updateTranslationSettings = async (next: TranslationSettings) => {
    try {
      await invoke("set_screen_translation_settings", next);
      setTranslationSettings(next);
      setNotice("屏幕翻译语言已保存");
    } catch (error) {
      setNotice(error instanceof Error ? error.message : String(error));
    }
  };

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  return (
    <main ref={surfaceRef} className={`surface-window settings-window ${isLeaving ? "is-leaving" : ""}`} onPointerMove={updateGlass}>
      <header className="window-titlebar" onPointerDown={startDragging} onPointerMove={continueDragging} onPointerUp={finishDragging}>
        <div className="brand-line" data-tauri-drag-region>
          <span className="small-orb"><Cpu size={14} /></span>
          <span>Kero</span>
          <span className="quiet-label">模型设置</span>
        </div>
        <div className="window-controls">
          <button type="button" title="最小化到任务栏" onClick={() => void invoke("minimize_window", { label: "settings" })}><Minus size={16} /></button>
          <button type="button" title="关闭设置" className="close" onClick={close}><X size={16} /></button>
        </div>
      </header>

      <div className="settings-layout">
        <aside className="provider-sidebar">
          <div className="sidebar-heading"><span>模型服务</span><button type="button" title="添加服务" onClick={resetDraft}><Plus size={16} /></button></div>
          <div className="provider-list">
            {providers.length === 0 && <div className="empty-provider"><Laptop size={20} /><p>从右侧添加第一个服务</p></div>}
            {providers.map((provider) => (
              <button key={provider.id} type="button" className={`provider-item ${draft.id === provider.id ? "selected" : ""}`} onClick={() => edit(provider)}>
                <span className="provider-icon">{provider.name.slice(0, 1).toUpperCase()}</span>
                <span className="provider-copy"><b>{provider.name}</b><small>{provider.model}</small></span>
                {provider.isDefault && <Check size={14} className="default-mark" />}
              </button>
            ))}
          </div>
          <div className="security-note"><ShieldCheck size={15} /> 密钥不会写入普通配置文件</div>
        </aside>

        <section className="provider-editor">
          <div className="editor-heading">
            <div><h1>{draft.id ? "编辑模型服务" : "添加模型服务"}</h1><p>选择协议并填写模型连接信息。</p></div>
            {draft.id && <button className="delete-provider" type="button" onClick={() => { const provider = providers.find((item) => item.id === draft.id); if (provider) void remove(provider); }}><Trash2 size={15} /> 删除</button>}
          </div>

          <form className="provider-form" onSubmit={save}>
            <label>
              <span>连接协议</span>
              <div className="protocol-grid">
                {providerKinds.map((item) => (
                  <button key={item.value} type="button" className={draft.kind === item.value ? "chosen" : ""} onClick={() => changeKind(item.value)}>
                    <b>{item.label}</b><small>{item.hint}</small>
                  </button>
                ))}
              </div>
            </label>
            <div className="field-row">
              <label><span>显示名称</span><input value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} placeholder="例如：我的 GPT" required /></label>
              <label><span>模型名称</span><input value={draft.model} onChange={(event) => setDraft({ ...draft, model: event.target.value })} placeholder="例如：gpt-4.1-mini" required /></label>
            </div>
            <label>
              <span>服务地址</span>
              <input value={draft.baseUrl} onChange={(event) => setDraft({ ...draft, baseUrl: event.target.value })} placeholder={defaultUrl[draft.kind] || "例如：http://127.0.0.1:11434"} />
              <small className="field-help">留空时使用 {kindLabel(draft.kind)} 官方地址；自定义服务请填写基础地址。</small>
            </label>
            <label>
              <span>API Key</span>
              <div className="key-field"><KeyRound size={17} /><input type="password" value={draft.apiKey ?? ""} onChange={(event) => setDraft({ ...draft, apiKey: event.target.value })} placeholder={draft.id ? "留空则保留当前密钥" : "粘贴 API Key"} /></div>
            </label>
            {notice && <p className={`form-notice ${notice.startsWith("已") ? "success" : ""}`}>{notice}</p>}
            <div className="form-actions">
              {draft.id && <button type="button" className="secondary" onClick={resetDraft}>新建服务</button>}
              <button type="submit" className="primary" disabled={saving}><Save size={16} /> {saving ? "正在保存" : "保存服务"}</button>
            </div>
          </form>

          {draft.id && <button type="button" className="make-default" onClick={async () => { await invoke("set_default_provider", { providerId: draft.id }); await loadProviders(); }}>
            <Check size={15} /> 将“{draft.name || "此服务"}”设为默认模型 <ChevronDown size={14} />
          </button>}

          <section className="appearance-settings">
            <section className="image-generation-settings">
              <div className="appearance-heading"><span><ImagePlus size={16} /> 图片生成</span><small>独立于聊天模型配置，使用单独保存的 API Key。</small></div>
              <div className="image-provider-choice" role="group" aria-label="图片生成接口类型">
                <button type="button" className={imageGeneration.provider === "openai" ? "selected" : ""} onClick={() => setImageGeneration((current) => ({ ...current, provider: "openai" }))}>OpenAI Images</button>
                <button type="button" className={imageGeneration.provider === "compatible" ? "selected" : ""} onClick={() => setImageGeneration((current) => ({ ...current, provider: "compatible" }))}>兼容接口</button>
              </div>
              <label>
                <span>服务地址</span>
                <input value={imageGeneration.baseUrl} onChange={(event) => setImageGeneration((current) => ({ ...current, baseUrl: event.target.value }))} placeholder={imageGeneration.provider === "openai" ? "留空使用 OpenAI 官方地址" : "例如：https://example.com/v1"} />
              </label>
              <label>
                <span>图片模型名称</span>
                <input value={imageGeneration.model} onChange={(event) => setImageGeneration((current) => ({ ...current, model: event.target.value }))} placeholder="由你填写，例如 gpt-image-1" />
              </label>
              <label>
                <span>图片生成 API Key</span>
                <div className="key-field"><KeyRound size={17} /><input type="password" value={imageGeneration.apiKey} onChange={(event) => setImageGeneration((current) => ({ ...current, apiKey: event.target.value }))} placeholder={imageGeneration.hasKey ? "留空则保留当前密钥" : "粘贴图片生成 API Key"} /></div>
              </label>
              {imageGenerationNotice && <p className={`form-notice ${imageGenerationNotice.startsWith("图片生成配置") ? "success" : ""}`}>{imageGenerationNotice}</p>}
              <button type="button" className="save-image-generation" disabled={savingImageGeneration || !imageGeneration.model.trim()} onClick={() => void saveImageGeneration()}><Save size={14} /> {savingImageGeneration ? "正在保存" : "保存图片生成配置"}</button>
            </section>
            <section className="dictation-asr-settings">
              <div className="appearance-heading"><span><Mic2 size={16} /> 语音识别</span><small>推荐 AI 语音识别：按住 Alt 说话，文字实时上屏，松开后自动润色。原生模式使用系统内置识别，仅作免配置的备用方案。</small></div>
              <p className="field-help">
                还没有 API Key？前往{' '}
                <a
                  href="https://platform.qianwenai.com/home/"
                  onClick={(event) => {
                    event.preventDefault();
                    openUrl("https://platform.qianwenai.com/home/").catch(console.error);
                  }}
                >千问AI开放平台</a>
                {' '}注册获取 Key——可获得完整使用本应用功能的模型（如实时语音识别 qwen3-asr-flash-realtime）。
              </p>
              <div className="image-provider-choice" role="group" aria-label="语音识别模式">
                <button type="button" className={appearance.dictationMode === "ai" ? "selected" : ""} onClick={() => updateAppearance({ dictationMode: "ai" })}>AI 语音识别（推荐）</button>
                <button type="button" className={appearance.dictationMode === "native" ? "selected" : ""} onClick={() => updateAppearance({ dictationMode: "native" })}>原生语音识别</button>
              </div>
              {appearance.dictationMode === "ai" && <>
                <label>
                  <span>AI 语音识别服务地址</span>
                  <input value={dictationAsr.baseUrl} onChange={(event) => setDictationAsr((current) => ({ ...current, baseUrl: event.target.value }))} placeholder="https://dashscope.aliyuncs.com/compatible-mode/v1" />
                </label>
                <label>
                  <span>AI 语音识别模型</span>
                  <input value={dictationAsr.model} onChange={(event) => setDictationAsr((current) => ({ ...current, model: event.target.value }))} placeholder="qwen3-asr-flash-realtime" />
                  <small>推荐 qwen3-asr-flash-realtime，模型名含 realtime 或 streaming 时走流式识别（边说边出字）。fun-asr 是长音频异步模型，不能用于按住 Alt 听写。</small>
                </label>
                <label>
                  <span>AI 语音识别 API Key</span>
                  <div className="key-field"><KeyRound size={17} /><input type="password" value={dictationAsr.apiKey} onChange={(event) => setDictationAsr((current) => ({ ...current, apiKey: event.target.value }))} placeholder={dictationAsr.hasKey ? "留空则保留当前密钥" : "粘贴 DashScope API Key"} /></div>
                </label>
                <AppearanceSwitch enabled={appearance.aiDictationPolish} onToggle={() => updateAppearance({ aiDictationPolish: !appearance.aiDictationPolish })}>
                  <span><b>AI 润色</b><small>松开按键后自动交给当前语言模型整理标点、语气词和表达，完成原位替换。</small></span>
                </AppearanceSwitch>
                {dictationAsrNotice && <p className={`form-notice ${dictationAsrNotice.startsWith("AI 语音识别配置") ? "success" : ""}`}>{dictationAsrNotice}</p>}
                <button type="button" className="save-image-generation" disabled={savingDictationAsr || !dictationAsr.model.trim()} onClick={() => void saveDictationAsr()}><Save size={14} /> {savingDictationAsr ? "正在保存" : "保存 AI 语音识别配置"}</button>
              </>}
            </section>
            <div className="system-prompt-settings">
              <button
                className="context-switch context-switch-button web-search-switch"
                type="button"
                role="switch"
                aria-checked={autostartEnabled}
                disabled={savingAutostart}
                onClick={() => void toggleAutostart()}
              >
                <span><b>开机自启</b><small>登录 Windows 后静默进入托盘，不自动打开胶囊界面。</small></span>
                <Power size={16} />
                <i className={autostartEnabled ? "is-on" : ""} aria-hidden="true"><em /></i>
              </button>
              <div className="appearance-heading"><span><Cpu size={16} /> 系统提示词</span><small>会在每次新请求时作为系统消息发送给当前模型。</small></div>
              <textarea value={systemPrompt} onChange={(event) => setSystemPrompt(event.target.value)} placeholder="例如：你是 Kero，请使用清晰、准确的中文回答。" rows={4} />
              <button type="button" className="save-system-prompt" onClick={() => void saveSystemPrompt()}><Save size={14} /> 保存系统提示词</button>
              <button
                className="context-switch context-switch-button"
                type="button"
                role="switch"
                aria-checked={contextEnabled}
                onClick={() => void toggleContext()}
              >
                <span><b>携带最近对话上下文</b><small>开启后会向模型发送最近 12 条对话消息。</small></span>
                <i className={contextEnabled ? "is-on" : ""} aria-hidden="true"><em /></i>
              </button>
              <section className="translation-settings">
                <div className="appearance-heading"><span><Languages size={16} /> 屏幕实时翻译</span><small>对 Kero 说“全屏翻译”后，译文会覆盖在原文字附近且不阻挡点击。</small></div>
                <div>
                  <label>
                    <span>源语言</span>
                    <select
                      value={translationSettings.sourceLanguage}
                      onChange={(event) => void updateTranslationSettings({ ...translationSettings, sourceLanguage: event.target.value })}
                    >
                      {translationLanguages.map((language) => <option key={language.value} value={language.value} disabled={language.value === translationSettings.targetLanguage}>{language.label}</option>)}
                    </select>
                  </label>
                  <label>
                    <span>目标语言</span>
                    <select
                      value={translationSettings.targetLanguage}
                      onChange={(event) => void updateTranslationSettings({ ...translationSettings, targetLanguage: event.target.value })}
                    >
                      {translationLanguages.filter((language) => language.value !== "auto").map((language) => <option key={language.value} value={language.value} disabled={language.value === translationSettings.sourceLanguage}>{language.label}</option>)}
                    </select>
                  </label>
                </div>
              </section>
              <button
                className="context-switch context-switch-button web-search-switch"
                type="button"
                role="switch"
                aria-checked={webSearchEnabled}
                disabled={checkingWebSearch}
                onClick={() => void toggleWebSearch()}
              >
                <span><b>模型原生联网搜索</b><small>默认关闭。开启时会检查默认模型，仅 OpenAI 官方兼容模型可用。</small></span>
                <Globe2 size={16} />
                <i className={webSearchEnabled ? "is-on" : ""} aria-hidden="true"><em /></i>
              </button>
              <button
                className="context-switch context-switch-button"
                type="button"
                role="switch"
                aria-checked={webSearchProxyEnabled}
                disabled={checkingWebSearch}
                onClick={() => void toggleWebSearchProxy()}
              >
                <span><b>允许中转站联网搜索（高级）</b><small>开启时会用当前默认中转站实际验证模型原生联网能力。</small></span>
                <i className={webSearchProxyEnabled ? "is-on" : ""} aria-hidden="true"><em /></i>
              </button>
              <button
                className="context-switch context-switch-button web-search-switch"
                type="button"
                role="switch"
                aria-checked={computerControlEnabled}
                onClick={() => void toggleComputerControl()}
              >
                <span><b>AI 操控电脑</b><small>聊天或语音提出操作任务时，自动观察屏幕并操控鼠标键盘。</small></span>
                <MousePointer2 size={16} />
                <i className={computerControlEnabled ? "is-on" : ""} aria-hidden="true"><em /></i>
              </button>
              <button
                className="context-switch context-switch-button web-search-switch risk-mode-switch"
                type="button"
                role="switch"
                aria-checked={computerRiskMode}
                disabled={!computerControlEnabled}
                onClick={() => void toggleComputerRiskMode()}
              >
                <span><b>风险模式</b><small>开启后发送、删除、付款、授权等操作也不会请求确认。</small></span>
                <TriangleAlert size={16} />
                <i className={computerRiskMode ? "is-on" : ""} aria-hidden="true"><em /></i>
              </button>
            </div>
            <div className="appearance-heading"><span><SlidersHorizontal size={16} /> 胶囊与交互</span><small>调整后会立即同步到桌面胶囊。</small></div>
            <AppearanceSwitch enabled={appearance.glassEnabled} onToggle={() => updateAppearance({ glassEnabled: !appearance.glassEnabled })}>
              <span><b>实时白色玻璃</b><small>根据指针位置实时更新表面高光</small></span>
            </AppearanceSwitch>
            <AppearanceSwitch enabled={appearance.dictationCorrection} onToggle={() => updateAppearance({ dictationCorrection: !appearance.dictationCorrection })}>
              <span><b>智能听写整理</b><small>按语义主动恢复标点、删除赘词、纠正错字并重组不通顺表达。</small></span>
            </AppearanceSwitch>
            <div className="dictation-memory-control">
              <AppearanceSwitch enabled={appearance.dictationMemory} onToggle={() => updateAppearance({ dictationMemory: !appearance.dictationMemory })}>
                <span><b>听写上下文记忆</b><small>仅保留最近 6 段已整理内容，用于后续断句、代词和术语识别。</small></span>
              </AppearanceSwitch>
              <button type="button" onClick={clearDictationMemory}>清除记忆</button>
            </div>
            <label className="dictation-vocabulary">
              <span><b>常用词与专有名词</b><small>填写人名、软件名、项目名或术语，每行一个，也可用逗号分隔。</small></span>
              <textarea
                value={dictationVocabulary}
                maxLength={1200}
                placeholder={"Kero\nCodex\nVisual Studio Code"}
                onChange={(event) => updateDictationVocabulary(event.target.value)}
              />
            </label>
            <AppearanceSwitch enabled={appearance.realtimeEdgeEnabled} onToggle={() => updateAppearance({ realtimeEdgeEnabled: !appearance.realtimeEdgeEnabled })}>
              <span><b>实时通话显示边缘光</b><small>屏幕感知实时通话和模型回答期间显示边缘光效。</small></span>
              <Waves size={17} />
            </AppearanceSwitch>
            <AppearanceSwitch enabled={appearance.edgeEnabled} onToggle={() => updateAppearance({ edgeEnabled: !appearance.edgeEnabled })}>
              <span><b>唤起边缘光</b><small>语音输入和模型回复期间显示</small></span>
              <Waves size={17} />
            </AppearanceSwitch>
            <div className="edge-color-choice">
              <span><b>光效颜色</b><small>同步应用于屏幕边缘、指针光晕和点击光效。</small></span>
              <div role="group" aria-label="光效颜色">
                <button type="button" className={appearance.edgeColor === "rainbow" ? "selected rainbow" : "rainbow"} onClick={() => updateAppearance({ edgeColor: "rainbow" })}>彩色</button>
                <button type="button" className={appearance.edgeColor === "blue" ? "selected blue" : "blue"} onClick={() => updateAppearance({ edgeColor: "blue" })}>蓝色</button>
              </div>
            </div>
            <AppearanceSwitch enabled={appearance.autoSend} onToggle={() => updateAppearance({ autoSend: !appearance.autoSend })}>
              <span><b>静默后自动发送</b><small>检测到约一秒停顿后发送语音内容</small></span>
              <Mic2 size={17} />
            </AppearanceSwitch>
            <label className="appearance-opacity">
              <span>胶囊透明度 <b>{appearance.opacity}%</b></span>
              <input type="range" min="25" max="100" value={appearance.opacity} onChange={(event) => updateAppearance({ opacity: Number(event.target.value) })} />
            </label>
            <div className="appearance-size">
              <span>默认胶囊大小</span>
              <div>
                {(["compact", "standard", "wide"] as CapsuleSize[]).map((size) => (
                  <button key={size} type="button" className={appearance.size === size ? "selected" : ""} onClick={() => updateAppearance({ size })}>
                    {{ compact: "紧凑", standard: "标准", wide: "宽阔" }[size]}
                  </button>
                ))}
              </div>
            </div>
          </section>
        </section>
      </div>
    </main>
  );
}
