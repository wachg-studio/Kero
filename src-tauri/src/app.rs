use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use directories::{ProjectDirs, UserDirs};
use futures_util::{SinkExt, StreamExt};
use image::{DynamicImage, ImageOutputFormat, Rgba};
use reqwest::Client;
use screenshots::Screen;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex, OnceLock,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, WindowEvent,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};
use uuid::Uuid;

mod dictation;
#[cfg(windows)]
use dictation::capture_focus_target;

static CLICK_THROUGH: AtomicBool = AtomicBool::new(false);
static ESCAPE_HELD: AtomicBool = AtomicBool::new(false);
static ALT_HELD: AtomicBool = AtomicBool::new(false);
static CTRL_HELD: AtomicBool = AtomicBool::new(false);
static E_HELD: AtomicBool = AtomicBool::new(false);
static ALT_DICTATION_ENGLISH: AtomicBool = AtomicBool::new(false);
static CTRL_F_PREFIX: AtomicBool = AtomicBool::new(false);
static CTRL_F_ACTIVE: AtomicBool = AtomicBool::new(false);
// A bare Alt release activates the foreground app's menu bar on Windows. When
// Alt starts an external dictation session, consume that whole key cycle.
static ALT_DICTATION_SUPPRESSED: AtomicBool = AtomicBool::new(false);
// 从 Alt 按下开始到前端完成/取消听写为止；期间 Esc 用于取消听写而不是落到目标应用。
static DICTATION_ACTIVE: AtomicBool = AtomicBool::new(false);
static COMPUTER_CONTROL_STOPPED: AtomicBool = AtomicBool::new(true);
static POINTER_TRACE_ACTIVE: AtomicBool = AtomicBool::new(false);
static COMPUTER_CURSOR_ACTIVE: AtomicBool = AtomicBool::new(false);
static EDGE_FRONTEND_READY: AtomicBool = AtomicBool::new(false);
static K_MARK_HELD: AtomicBool = AtomicBool::new(false);
static COMPUTER_MARK_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SCREEN_TRANSLATION_ACTIVE: AtomicBool = AtomicBool::new(false);
static SCREEN_TRANSLATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static EDGE_CAPTURE_EXCLUDED: AtomicBool = AtomicBool::new(false);
static STARTED_BY_AUTOSTART: AtomicBool = AtomicBool::new(false);
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static SIZE_ANIMATION_ID: AtomicU64 = AtomicU64::new(0);
static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
static IMAGE_HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
static LOCKED_POSITION: OnceLock<Mutex<Option<WindowPosition>>> = OnceLock::new();
static COMPUTER_MARK: OnceLock<Mutex<Option<ComputerMark>>> = OnceLock::new();
static HOTKEY_APP: OnceLock<AppHandle> = OnceLock::new();
static REALTIME_DICTATION_SESSIONS: OnceLock<Mutex<HashMap<String, RealtimeDictationSession>>> =
    OnceLock::new();

enum RealtimeDictationCommand {
    Audio(Vec<u8>),
    Finish,
}

// Alt 按下即预连接的实时识别会话：前端麦克风就绪后直接认领，省掉 200~500ms 建连时间。
struct PreconnectedRealtime {
    socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    created_at: std::time::Instant,
    fingerprint: String,
}

static REALTIME_PRECONNECT: std::sync::Mutex<Option<PreconnectedRealtime>> = std::sync::Mutex::new(None);
static REALTIME_PRECONNECT_GENERATION: AtomicU64 = AtomicU64::new(0);
static REALTIME_PRECONNECT_CONNECTING: AtomicBool = AtomicBool::new(false);
// 前端告知当前是否使用流式识别模型；避免非流式用户每次按 Alt 都白建一条连接。
static REALTIME_PRECONNECT_HINT: AtomicBool = AtomicBool::new(false);
static REALTIME_VOCABULARY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn realtime_preconnect_vocabulary() -> Option<String> {
    REALTIME_VOCABULARY
        .lock()
        .ok()
        .and_then(|value| value.clone())
        .filter(|value| !value.trim().is_empty())
}

fn realtime_preconnect_fingerprint(
    origin: &str,
    model: &str,
    sample_rate: u32,
    vocabulary: Option<&str>,
) -> String {
    format!("{origin}|{}|{sample_rate}|{}", model.trim().to_ascii_lowercase(), vocabulary.unwrap_or_default())
}

fn invalidate_realtime_preconnect() {
    REALTIME_PRECONNECT_GENERATION.fetch_add(1, Ordering::SeqCst);
    REALTIME_PRECONNECT_CONNECTING.store(false, Ordering::SeqCst);
    if let Ok(mut pool) = REALTIME_PRECONNECT.lock() {
        *pool = None;
    }
}

/// 把底层连接/接口错误翻译成用户能采取行动的提示。
fn friendly_realtime_error(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("unauthorized") || lower.contains("invalid api") || lower.contains("invalid_api") {
        "语音识别 API Key 无效或未授权，请在设置中检查密钥".to_string()
    } else if lower.contains("throttl") || lower.contains("rate limit") || lower.contains("arrear") {
        "请求过于频繁或账户额度不足，请稍后再试".to_string()
    } else if lower.contains("not found") || lower.contains("invalid parameter") && lower.contains("model") {
        "语音识别模型名称不存在或当前账号无权限使用该模型".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "连接语音识别服务超时，请检查网络后重试".to_string()
    } else {
        message.to_string()
    }
}

/// 建立 Qwen3-ASR-Realtime 连接（含认证与协议头），握手由调用方负责。
async fn connect_qwen_realtime_socket(
    origin: &str,
    model: &str,
    key: &str,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, String> {
    let mut request = format!("{origin}/api-ws/v1/realtime?model={model}")
        .into_client_request()
        .map_err(|error| friendly_realtime_error(&format!("无法准备实时语音识别请求: {error}")))?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {key}")
            .parse()
            .map_err(|error| format!("无法准备实时语音识别认证: {error}"))?,
    );
    request.headers_mut().insert(
        "OpenAI-Beta",
        "realtime=v1"
            .parse()
            .map_err(|error| format!("无法准备实时语音识别协议头: {error}"))?,
    );
    connect_async(request)
        .await
        .map(|(socket, _)| socket)
        .map_err(|error| friendly_realtime_error(&format!("无法连接实时语音识别服务: {error}")))
}

/// 发送 session.update 并等待确认；携带热词上下文被拒时自动降级为无上下文重试一次。
async fn qwen_realtime_handshake(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    sample_rate: u32,
    context: Option<&str>,
    timeout_secs: u64,
) -> Result<(), String> {
    let session_update = |context: Option<&str>| {
        let transcription = if context.is_some() {
            json!({ "language": "zh", "context": context })
        } else {
            json!({ "language": "zh" })
        };
        json!({
            "type": "session.update",
            "session": {
                "input_audio_format": "pcm",
                "sample_rate": sample_rate,
                "input_audio_transcription": transcription,
                "turn_detection": { "type": "server_vad", "silence_duration_ms": 500 }
            }
        })
        .to_string()
    };
    let mut with_context = context.is_some();
    loop {
        socket
            .send(Message::Text(session_update(if with_context { context } else { None }).into()))
            .await
            .map_err(|error| format!("无法配置实时语音识别会话: {error}"))?;
        let confirmed = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), socket.next())
            .await
            .map_err(|_| "实时语音识别启动超时".to_string())?
            .ok_or_else(|| "实时语音识别服务提前断开".to_string())?
            .map_err(|error| friendly_realtime_error(&format!("实时语音识别启动失败: {error}")))?;
        let Message::Text(message) = confirmed else {
            return Err("实时语音识别返回了无效启动响应".to_string());
        };
        let value: Value = serde_json::from_str(&message)
            .map_err(|_| "实时语音识别返回了无效启动数据".to_string())?;
        match value.get("type").and_then(Value::as_str).unwrap_or_default() {
            "session.created" | "session.updated" => return Ok(()),
            "error" => {
                let detail = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("实时语音识别会话配置失败");
                if with_context {
                    // 热词上下文字段被服务端拒绝时降级重试，保证听写功能不受影响。
                    with_context = false;
                    continue;
                }
                return Err(friendly_realtime_error(detail));
            }
            _ => return Err("实时语音识别未能建立会话".to_string()),
        }
    }
}

async fn reconnect_qwen_realtime_socket(
    origin: &str,
    model: &str,
    key: &str,
    sample_rate: u32,
    context: Option<&str>,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, String> {
    let mut socket = connect_qwen_realtime_socket(origin, model, key).await?;
    qwen_realtime_handshake(&mut socket, sample_rate, context, 5).await?;
    Ok(socket)
}

/// Alt 按下时后台预连接：只对已启用流式模型的用户生效，任何失败都静默忽略。
async fn preconnect_realtime_dictation() {
    if !REALTIME_PRECONNECT_HINT.load(Ordering::SeqCst)
        || REALTIME_PRECONNECT_CONNECTING.swap(true, Ordering::SeqCst)
    {
        return;
    }
    let generation = REALTIME_PRECONNECT_GENERATION.load(Ordering::SeqCst);
    let result = async {
        if REALTIME_PRECONNECT.lock().ok().map(|pool| pool.is_some()).unwrap_or(true) {
            return None;
        }
        let Ok(app_config) = load_config() else { return None };
        let config = app_config.dictation_asr;
        if !is_qwen3_asr_realtime_model(&config.model) {
            return None;
        }
        let Some(origin) = dashscope_realtime_asr_origin(config.base_url.trim_end_matches('/')) else {
            return None;
        };
        let sample_rate = if config.model.to_ascii_lowercase().contains("8k") { 8_000 } else { 16_000 };
        let key = match read_secret(DICTATION_ASR_SECRET_ID) {
            Ok(Some(key)) if !key.trim().is_empty() => key,
            _ => return None,
        };
        let context = realtime_preconnect_vocabulary();
        let fingerprint = realtime_preconnect_fingerprint(origin, &config.model, sample_rate, context.as_deref());
        let mut socket = connect_qwen_realtime_socket(origin, config.model.trim(), key.trim()).await.ok()?;
        qwen_realtime_handshake(&mut socket, sample_rate, context.as_deref(), 5).await.ok()?;
        Some((socket, fingerprint))
    }
    .await;
    REALTIME_PRECONNECT_CONNECTING.store(false, Ordering::SeqCst);
    let Some((socket, fingerprint)) = result else { return };
    // 配置变更或正式会话已抢先启动时，丢弃这个过期预连接。
    if generation != REALTIME_PRECONNECT_GENERATION.load(Ordering::SeqCst)
        || !REALTIME_PRECONNECT_HINT.load(Ordering::SeqCst)
    {
        return;
    }
    let mut pool = match REALTIME_PRECONNECT.lock() {
        Ok(pool) => pool,
        Err(_) => return,
    };
    if pool.is_some() || generation != REALTIME_PRECONNECT_GENERATION.load(Ordering::SeqCst) {
        return;
    }
    *pool = Some(PreconnectedRealtime {
        socket,
        created_at: std::time::Instant::now(),
        fingerprint,
    });
    // 8 秒内未被认领则丢弃，避免长期挂着的空闲连接。
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(8));
        if generation != REALTIME_PRECONNECT_GENERATION.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(mut pool) = REALTIME_PRECONNECT.lock() {
            if let Some(preconnected) = pool.as_ref() {
                if preconnected.created_at.elapsed() >= std::time::Duration::from_secs(8) {
                    *pool = None;
                }
            }
        }
    });
}

struct RealtimeDictationSession {
    sender: tokio::sync::mpsc::Sender<RealtimeDictationCommand>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RealtimeDictationSessionStart {
    session_id: String,
    sample_rate: u32,
}

fn shared_http_client() -> Client {
    HTTP_CLIENT
        .get_or_init(|| {
            Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(90))
                .pool_idle_timeout(std::time::Duration::from_secs(120))
                .tcp_keepalive(std::time::Duration::from_secs(30))
                .build()
                .expect("Kero HTTP client configuration is valid")
        })
        .clone()
}

// Some OpenAI-compatible image gateways send malformed HTTP/2 or compressed
// response streams. Keep that interoperability workaround isolated to image
// generation so it cannot affect chat, dictation, or realtime connections.
fn image_generation_http_client() -> Client {
    IMAGE_HTTP_CLIENT
        .get_or_init(|| {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::ACCEPT_ENCODING,
                reqwest::header::HeaderValue::from_static("identity"),
            );
            Client::builder()
                .connect_timeout(std::time::Duration::from_secs(8))
                .timeout(std::time::Duration::from_secs(120))
                .pool_idle_timeout(std::time::Duration::from_secs(30))
                .tcp_keepalive(std::time::Duration::from_secs(30))
                .http1_only()
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .default_headers(headers)
                .build()
                .expect("Kero image HTTP client configuration is valid")
        })
        .clone()
}

#[cfg(windows)]
fn physical_alt_held() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    unsafe {
        [0x12, 0xa4, 0xa5]
            .iter()
            .any(|key| (GetAsyncKeyState(*key) as u16 & 0x8000) != 0)
    }
}

#[cfg(windows)]
fn physical_shift_held() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    unsafe {
        [0x10, 0xa0, 0xa1]
            .iter()
            .any(|key| (GetAsyncKeyState(*key) as u16 & 0x8000) != 0)
    }
}

#[cfg(windows)]
fn point_in_cursor_shape(x: f32, y: f32, points: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut previous = points.len() - 1;
    for current in 0..points.len() {
        let (xi, yi) = points[current];
        let (xj, yj) = points[previous];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

#[cfg(windows)]
fn reset_system_cursor_scheme() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_SETCURSORS};

    unsafe {
        SystemParametersInfoW(SPI_SETCURSORS, 0, std::ptr::null_mut(), 0);
    }
}

#[cfg(windows)]
fn restore_computer_cursor() {
    if COMPUTER_CURSOR_ACTIVE.swap(false, Ordering::SeqCst) {
        reset_system_cursor_scheme();
    }
}

#[cfg(windows)]
fn apply_computer_cursor() -> Result<(), String> {
    use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, SetSystemCursor, ICONINFO, OCR_NORMAL,
    };

    if COMPUTER_CURSOR_ACTIVE.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    const SIZE: usize = 32;
    let outer = [
        (2.0, 1.0),
        (2.0, 27.0),
        (9.5, 21.0),
        (14.5, 30.0),
        (20.5, 26.8),
        (15.4, 18.2),
        (27.5, 18.2),
    ];
    let inner = [
        (4.8, 5.0),
        (4.8, 22.0),
        (10.2, 17.8),
        (15.3, 27.0),
        (17.3, 25.5),
        (12.4, 15.6),
        (22.6, 16.2),
    ];
    let mut color = vec![0u32; SIZE * SIZE];
    let mut mask = vec![0xffu8; SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let opaque = point_in_cursor_shape(x as f32 + 0.5, y as f32 + 0.5, &outer);
            let pixel = if opaque {
                if point_in_cursor_shape(x as f32 + 0.5, y as f32 + 0.5, &inner) {
                    0xff111820
                } else {
                    0xfff8fbff
                }
            } else {
                0
            };
            color[y * SIZE + x] = pixel;
            if opaque {
                mask[y * 4 + x / 8] &= !(0x80 >> (x % 8));
            }
        }
    }

    unsafe {
        let color_bitmap = CreateBitmap(SIZE as i32, SIZE as i32, 1, 32, color.as_ptr().cast());
        let mask_bitmap = CreateBitmap(SIZE as i32, SIZE as i32, 1, 1, mask.as_ptr().cast());
        if color_bitmap.is_null() || mask_bitmap.is_null() {
            if !color_bitmap.is_null() {
                DeleteObject(color_bitmap);
            }
            if !mask_bitmap.is_null() {
                DeleteObject(mask_bitmap);
            }
            COMPUTER_CURSOR_ACTIVE.store(false, Ordering::SeqCst);
            return Err("无法创建电脑操控鼠标样式。".to_string());
        }
        let cursor = CreateIconIndirect(&ICONINFO {
            fIcon: 0,
            xHotspot: 3,
            yHotspot: 2,
            hbmMask: mask_bitmap,
            hbmColor: color_bitmap,
        });
        DeleteObject(color_bitmap);
        DeleteObject(mask_bitmap);
        if cursor.is_null() || SetSystemCursor(cursor, OCR_NORMAL) == 0 {
            COMPUTER_CURSOR_ACTIVE.store(false, Ordering::SeqCst);
            return Err("无法应用电脑操控鼠标样式。".to_string());
        }
    }
    Ok(())
}

#[cfg(windows)]
fn place_computer_mark(app: &AppHandle, kind: ComputerMarkKind) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    if COMPUTER_CONTROL_STOPPED.load(Ordering::SeqCst)
        || !COMPUTER_CURSOR_ACTIVE.load(Ordering::SeqCst)
    {
        return;
    }
    unsafe {
        let mut point = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut point) == 0 {
            return;
        }
        let Ok((left, top, width, height)) = primary_screen_physical_bounds() else {
            return;
        };
        let mark = ComputerMark {
            x: ((point.x - left) as f64 / width as f64).clamp(0.0, 1.0),
            y: ((point.y - top) as f64 / height as f64).clamp(0.0, 1.0),
            kind,
        };
        let Ok(mut stored_mark) = COMPUTER_MARK.get_or_init(|| Mutex::new(None)).lock() else {
            trace_runtime("computer mark state was poisoned; ignored user mark");
            return;
        };
        let sequence = COMPUTER_MARK_SEQUENCE.fetch_add(1, Ordering::SeqCst) + 1;
        *stored_mark = Some(mark);
        drop(stored_mark);
        let _ = app.emit_to(
            "edge",
            "kero:computer-mark",
            json!({
                "x": mark.x, "y": mark.y, "active": true
                ,"kind": mark.kind, "sequence": sequence
            }),
        );
        let _ = app.emit_to(
            "main",
            "kero:computer-marked",
            json!({ "kind": mark.kind, "x": mark.x, "y": mark.y, "sequence": sequence }),
        );
    }
}

#[cfg(windows)]
unsafe extern "system" fn keyboard_hook(code: i32, message: usize, data: isize) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, KBDLLHOOKSTRUCT, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    let suppress = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> bool {
        if code >= 0 {
            let hook = unsafe { &*(data as *const KBDLLHOOKSTRUCT) };
            let key = hook.vkCode;
            let is_injected = hook.flags & 0x10 != 0;
            let is_pressed = message as u32 == WM_KEYDOWN || message as u32 == WM_SYSKEYDOWN;
            let is_released = message as u32 == WM_KEYUP || message as u32 == WM_SYSKEYUP;
            if (key == 0x11 || key == 0xa2 || key == 0xa3) && !is_injected {
                CTRL_HELD.store(is_pressed && !is_released, Ordering::SeqCst);
                if is_released && CTRL_F_ACTIVE.load(Ordering::SeqCst) {
                    CTRL_F_ACTIVE.store(false, Ordering::SeqCst);
                    CTRL_F_PREFIX.store(false, Ordering::SeqCst);
                    return true;
                }
                if is_released {
                    CTRL_F_PREFIX.store(false, Ordering::SeqCst);
                }
            }
            if key == 0x46 && !is_injected && is_pressed && CTRL_HELD.load(Ordering::SeqCst) {
                // Ctrl+F+F7：Ctrl+F 只进入前缀状态，按 F7 才捕获选区并触发翻译。
                CTRL_F_PREFIX.store(true, Ordering::SeqCst);
                return true;
            }
            if key == 0x76 && !is_injected && is_pressed && CTRL_HELD.load(Ordering::SeqCst) && CTRL_F_PREFIX.load(Ordering::SeqCst) {
                let captured = capture_focus_target();
                CTRL_F_PREFIX.store(false, Ordering::SeqCst);
                CTRL_F_ACTIVE.store(captured, Ordering::SeqCst);
                if captured {
                    if let Some(app) = HOTKEY_APP.get() {
                        let _ = app.emit_to("main", "kero:shortcut-selection-translate", ());
                    }
                }
                return captured;
            }
            if key == 0x45 && !is_injected {
                // 记录 E 的物理按下状态，使 E→Alt 和 Alt→E 两种顺序都能进入英译听写。
                E_HELD.store(is_pressed && !is_released, Ordering::SeqCst);
                if is_pressed && ALT_HELD.load(Ordering::SeqCst) && !CTRL_HELD.load(Ordering::SeqCst) {
                    // Alt+E：在当前听写会话内升级为松开后英译；吞掉 E，避免它落入目标输入框。
                    ALT_DICTATION_ENGLISH.store(true, Ordering::SeqCst);
                    if let Some(app) = HOTKEY_APP.get() {
                        let _ = app.emit_to("main", "kero:shortcut-dictation-mode", json!({ "translateToEnglish": true }));
                    }
                    return true;
                }
                if CTRL_HELD.load(Ordering::SeqCst) && !ALT_HELD.load(Ordering::SeqCst) {
                    // Ctrl+E 是选区翻译的前缀，阻止其单独触发目标应用的 Ctrl+E 行为。
                    return true;
                }
                if is_released {
                    return ALT_DICTATION_SUPPRESSED.load(Ordering::SeqCst);
                }
            }
            if (key == 0x12 || key == 0xa4 || key == 0xa5) && !is_injected {
                if is_pressed && !ALT_HELD.swap(true, Ordering::SeqCst) {
                    let captured_external_target = capture_focus_target();
                    ALT_DICTATION_SUPPRESSED.store(captured_external_target, Ordering::SeqCst);
                    let translate_to_english = E_HELD.load(Ordering::SeqCst);
                    ALT_DICTATION_ENGLISH.store(translate_to_english, Ordering::SeqCst);
                    DICTATION_ACTIVE.store(captured_external_target, Ordering::SeqCst);
                    tauri::async_runtime::spawn(async {
                        let _ = warm_dictation_service_inner().await;
                    });
                    tauri::async_runtime::spawn(preconnect_realtime_dictation());
                    if let Some(app) = HOTKEY_APP.get() {
                        let _ = app.emit_to(
                            "main",
                            "kero:shortcut-dictation-start",
                            json!({ "translateToEnglish": translate_to_english }),
                        );
                    }
                } else if is_released && ALT_HELD.swap(false, Ordering::SeqCst) {
                    if let Some(app) = HOTKEY_APP.get() {
                        let _ = app.emit_to(
                            "main",
                            "kero:shortcut-dictation-stop",
                            json!({ "translateToEnglish": ALT_DICTATION_ENGLISH.swap(false, Ordering::SeqCst) }),
                        );
                    }
                    return ALT_DICTATION_SUPPRESSED.swap(false, Ordering::SeqCst);
                }
                return ALT_DICTATION_SUPPRESSED.load(Ordering::SeqCst);
            }
            if key == 0x1b {
                ESCAPE_HELD.store(is_pressed && !is_released, Ordering::SeqCst);
                if is_pressed {
                    COMPUTER_CONTROL_STOPPED.store(true, Ordering::SeqCst);
                    restore_computer_cursor();
                    clear_computer_mark();
                    if let Some(app) = HOTKEY_APP.get() {
                        if DICTATION_ACTIVE.swap(false, Ordering::SeqCst) {
                            // 听写进行中：Esc 取消听写并吞掉按键，不落到目标应用。
                            let _ = app.emit_to("main", "kero:shortcut-dictation-cancel", ());
                            return true;
                        }
                        let _ = app.emit_to("main", "kero:computer-control-stop", ());
                    }
                }
            } else if key == 0x4b && is_released {
                K_MARK_HELD.store(false, Ordering::SeqCst);
            } else if key == 0x4b && is_pressed && ESCAPE_HELD.load(Ordering::SeqCst) {
                if let Some(app) = HOTKEY_APP.get() {
                    if CLICK_THROUGH.swap(false, Ordering::SeqCst) {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.set_ignore_cursor_events(false);
                        }
                        let _ = app.emit_to("main", "kero:click-through-changed", false);
                    }
                }
            } else if key == 0x4b
                && is_pressed
                && !is_injected
                && !COMPUTER_CONTROL_STOPPED.load(Ordering::SeqCst)
                && COMPUTER_CURSOR_ACTIVE.load(Ordering::SeqCst)
                && !K_MARK_HELD.swap(true, Ordering::SeqCst)
            {
                if let Some(app) = HOTKEY_APP.get() {
                    let kind = if physical_shift_held() {
                        ComputerMarkKind::Mistake
                    } else {
                        ComputerMarkKind::Target
                    };
                    place_computer_mark(app, kind);
                }
            }
        }
        false
    }))
    .unwrap_or_else(|_| {
        trace_runtime("recovered panic in keyboard hook");
        false
    });
    if suppress {
        return 1;
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, message, data) }
}

#[cfg(windows)]
unsafe extern "system" fn mouse_hook(code: i32, message: usize, data: isize) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetClassNameW, WindowFromPoint, MSLLHOOKSTRUCT, WM_LBUTTONDOWN,
        WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
    };

    let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if code >= 0
            && POINTER_TRACE_ACTIVE.load(Ordering::SeqCst)
            && matches!(
                message as u32,
                WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP
            )
        {
            let event = unsafe { &*(data as *const MSLLHOOKSTRUCT) };
            let window = unsafe { WindowFromPoint(event.pt) };
            let mut class_name = [0u16; 128];
            let length =
                unsafe { GetClassNameW(window, class_name.as_mut_ptr(), class_name.len() as i32) };
            let class_name = String::from_utf16_lossy(&class_name[..length.max(0) as usize]);
            let name = match message as u32 {
                WM_LBUTTONDOWN => "left_down",
                WM_LBUTTONUP => "left_up",
                WM_RBUTTONDOWN => "right_down",
                WM_RBUTTONUP => "right_up",
                _ => "unknown",
            };
            trace_computer_control(&format!(
                "mouse hook event={name} point=({},{}) flags=0x{:x} window={:?} class={class_name}",
                event.pt.x, event.pt.y, event.flags, window
            ));
        }
    }));
    if handled.is_err() {
        trace_runtime("recovered panic in mouse hook");
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, message, data) }
}

#[cfg(windows)]
fn install_keyboard_hook(app: AppHandle) {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, MSG, WH_KEYBOARD_LL,
        WH_MOUSE_LL,
    };

    let _ = HOTKEY_APP.set(app);
    std::thread::spawn(|| unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), module, 0);
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0);
        if keyboard.is_null() && mouse.is_null() {
            trace_runtime("global keyboard and mouse hooks could not be installed");
            return;
        }
        let mut message = std::mem::zeroed::<MSG>();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    });
}

#[cfg(windows)]
fn acquire_instance_lock() -> bool {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name = "Local\\KeroAI-Capsule-SingleInstance"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    handle.is_null() || unsafe { GetLastError() } != 183
}

#[tauri::command]
fn is_alt_key_down() -> bool {
    #[cfg(windows)]
    {
        let held = physical_alt_held();
        if !held {
            ALT_HELD.store(false, Ordering::SeqCst);
        }
        return held;
    }
    #[cfg(not(windows))]
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredProvider {
    id: String,
    name: String,
    kind: String,
    base_url: String,
    model: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicProvider {
    id: String,
    name: String,
    kind: String,
    base_url: String,
    model: String,
    has_key: bool,
    is_default: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderInput {
    id: Option<String>,
    name: String,
    kind: String,
    base_url: String,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
}

const IMAGE_GENERATION_SECRET_ID: &str = "kero-image-generation-api-key";
const MAX_GENERATED_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const DICTATION_ASR_SECRET_ID: &str = "kero-dictation-asr-api-key";
const MAX_DICTATION_AUDIO_BYTES: usize = 12 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageGenerationSettings {
    #[serde(default = "default_image_generation_provider")]
    provider: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    model: String,
}

impl Default for ImageGenerationSettings {
    fn default() -> Self {
        Self {
            provider: default_image_generation_provider(),
            base_url: String::new(),
            model: String::new(),
        }
    }
}

fn default_image_generation_provider() -> String {
    "openai".to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicImageGenerationConfig {
    provider: String,
    base_url: String,
    model: String,
    has_key: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageGenerationConfigInput {
    provider: String,
    base_url: String,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DictationAsrSettings {
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    model: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicDictationAsrConfig {
    base_url: String,
    model: String,
    has_key: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DictationAsrConfigInput {
    base_url: String,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageGenerationRequest {
    prompt: String,
    aspect_ratio: String,
    count: u8,
    #[serde(default)]
    reference_images: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImagePromptOptimizationRequest {
    provider_id: Option<String>,
    prompt: String,
    #[serde(default)]
    reference_images: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedImage {
    path: String,
    prompt: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppConfig {
    #[serde(default)]
    providers: Vec<StoredProvider>,
    #[serde(default)]
    default_provider: Option<String>,
    #[serde(default)]
    image_generation: ImageGenerationSettings,
    #[serde(default)]
    dictation_asr: DictationAsrSettings,
    #[serde(default)]
    system_prompt: String,
    #[serde(default = "default_context_enabled")]
    context_enabled: bool,
    #[serde(default)]
    web_search_enabled: bool,
    #[serde(default)]
    web_search_proxy_enabled: bool,
    #[serde(default)]
    computer_control_enabled: bool,
    #[serde(default)]
    computer_control_risk_mode: bool,
    #[serde(default)]
    computer_corrections: Vec<ComputerCorrection>,
    #[serde(default = "default_translation_source_language")]
    translation_source_language: String,
    #[serde(default = "default_translation_target_language")]
    translation_target_language: String,
}

fn default_context_enabled() -> bool {
    true
}

fn default_translation_source_language() -> String {
    "auto".to_string()
}

fn default_translation_target_language() -> String {
    "zh-CN".to_string()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct EncryptedSecrets {
    #[serde(default)]
    keys: HashMap<String, String>,
}

struct Entry {
    provider_id: String,
}

impl Entry {
    fn set_password(&self, secret: &str) -> Result<(), String> {
        write_secret(&self.provider_id, secret)
    }

    fn get_password(&self) -> Result<String, String> {
        read_secret(&self.provider_id)?.ok_or_else(|| "API Key 尚未保存".to_string())
    }

    fn delete_credential(&self) -> Result<(), String> {
        delete_secret(&self.provider_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatAttachment {
    name: String,
    mime_type: String,
    data_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DictationTransformRequest {
    text: String,
    #[serde(default)]
    target_language: Option<String>,
    #[serde(default)]
    correct_typos: Option<bool>,
    #[serde(default)]
    vocabulary: Option<String>,
    #[serde(default)]
    memory: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatRequest {
    provider_id: Option<String>,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    attachments: Vec<ChatAttachment>,
    #[serde(default)]
    screen_image: Option<String>,
    #[serde(default)]
    web_search: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranslationSettings {
    source_language: String,
    target_language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScreenTranslationItem {
    text: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    #[serde(default)]
    font_size: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ScreenTranslationResponse {
    #[serde(default)]
    items: Vec<ScreenTranslationItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DetectedScreenText {
    id: String,
    text: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    font_size: f64,
}

#[derive(Debug, Deserialize)]
struct ScreenTextTranslationResponse {
    #[serde(default)]
    items: Vec<ScreenTextTranslationItem>,
}

#[derive(Debug, Deserialize)]
struct ScreenTextTranslationItem {
    id: String,
    text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebSearchAvailability {
    supported: bool,
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ComputerAction {
    action: String,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    end_x: Option<f64>,
    #[serde(default)]
    end_y: Option<f64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    desktop_target: Option<String>,
    #[serde(default)]
    window_target: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    ui_target: Option<String>,
    #[serde(default)]
    ui_action: Option<String>,
    #[serde(default)]
    plan_step_id: Option<String>,
    #[serde(default)]
    step_complete: bool,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    keys: Option<Vec<String>>,
    #[serde(default)]
    amount: Option<i32>,
    #[serde(default)]
    duration: Option<i32>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    expected_outcome: Option<String>,
    #[serde(default)]
    retry_evidence: Option<String>,
    #[serde(default)]
    final_evidence: Option<String>,
    #[serde(default)]
    requires_confirmation: bool,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    observation_sequence: Option<u64>,
    #[serde(default)]
    fast: bool,
}

#[derive(Debug, Clone)]
struct DesktopIcon {
    name: String,
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum ComputerMarkKind {
    Target,
    Mistake,
}

#[derive(Debug, Clone, Copy)]
struct ComputerMark {
    x: f64,
    y: f64,
    kind: ComputerMarkKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComputerCorrection {
    window_key: String,
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SemanticControl {
    id: String,
    name: String,
    control_type: String,
    x: f64,
    y: f64,
    enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SemanticSnapshot {
    window_key: String,
    window_name: String,
    window_class: String,
    fingerprint: String,
    controls: Vec<SemanticControl>,
}

#[cfg(windows)]
#[derive(Debug, Clone)]
struct VisibleWindowObservation {
    handle: windows_sys::Win32::Foundation::HWND,
    id: String,
    title: String,
    class_name: String,
    state: &'static str,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    visible_fraction: f64,
    foreground: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComputerPlanStep {
    #[serde(default)]
    id: String,
    title: String,
    expected_outcome: String,
    #[serde(default)]
    completion_hint: String,
    #[serde(default)]
    max_attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComputerTaskPlan {
    summary: String,
    steps: Vec<ComputerPlanStep>,
    #[serde(default)]
    final_outcome: String,
}

#[cfg(windows)]
#[repr(C)]
struct RemoteLvItemW {
    mask: u32,
    item: i32,
    sub_item: i32,
    state: u32,
    state_mask: u32,
    text: *mut u16,
    text_max: i32,
    image: i32,
    l_param: isize,
    indent: i32,
    group_id: i32,
    columns: u32,
    column_indices: *mut u32,
    column_formats: *mut i32,
    group: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamEvent {
    request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta: Option<String>,
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowPosition {
    x: i32,
    y: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextMenuPlacement {
    above: bool,
    target_y: Option<i32>,
}

fn config_path() -> Result<PathBuf, String> {
    let project_dirs = ProjectDirs::from("com", "Kero", "Kero")
        .ok_or_else(|| "无法找到本机应用数据目录".to_string())?;
    let data_dir = project_dirs.data_local_dir();
    fs::create_dir_all(data_dir).map_err(|error| format!("无法创建配置目录: {error}"))?;
    Ok(data_dir.join("providers.json"))
}

fn runtime_log_path() -> Result<PathBuf, String> {
    Ok(config_path()?.with_file_name("runtime.log"))
}

fn trace_runtime(message: &str) {
    let Ok(path) = runtime_log_path() else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let _ = writeln!(file, "{timestamp} {message}");
}

fn install_runtime_diagnostics() {
    if let Ok(path) = runtime_log_path() {
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 1_048_576) {
            let previous = path.with_file_name("runtime.previous.log");
            let _ = fs::remove_file(&previous);
            let _ = fs::rename(&path, previous);
        }
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("unknown panic");
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_else(|| "unknown location".to_string());
        trace_runtime(&format!("panic at {location}: {payload}"));
        previous(info);
    }));
}

struct RuntimeSessionGuard {
    marker: Option<PathBuf>,
}

impl RuntimeSessionGuard {
    fn begin() -> Self {
        let marker = runtime_log_path()
            .ok()
            .map(|path| path.with_file_name("runtime.active"));
        if let Some(path) = marker.as_ref() {
            if path.exists() {
                trace_runtime("previous process ended without removing its runtime marker");
            }
            let _ = fs::write(path, std::process::id().to_string());
        }
        trace_runtime(&format!(
            "process started pid={} autostart={}",
            std::process::id(),
            STARTED_BY_AUTOSTART.load(Ordering::SeqCst)
        ));
        Self { marker }
    }
}

impl Drop for RuntimeSessionGuard {
    fn drop(&mut self) {
        if let Some(path) = self.marker.as_ref() {
            let _ = fs::remove_file(path);
        }
        trace_runtime(if EXIT_REQUESTED.load(Ordering::SeqCst) {
            "process stopped after explicit exit"
        } else {
            "process event loop ended without explicit exit"
        });
    }
}

fn computer_control_log_path() -> Result<PathBuf, String> {
    Ok(config_path()?.with_file_name("computer-control.log"))
}

fn reset_computer_control_log() {
    if let Ok(path) = computer_control_log_path() {
        let _ = fs::write(path, "Kero computer control trace\n");
    }
}

fn trace_computer_control(message: &str) {
    let Ok(path) = computer_control_log_path() else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let _ = writeln!(file, "{timestamp} {message}");
}

fn load_config() -> Result<AppConfig, String> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }

    let text = fs::read_to_string(&path).map_err(|error| format!("无法读取配置: {error}"))?;
    match serde_json::from_str(&text) {
        Ok(config) => Ok(config),
        Err(original_error) => {
            let config = recover_config_with_invalid_system_prompt(&text)
                .ok_or_else(|| format!("配置格式无效: {original_error}"))?;
            let repaired = serde_json::to_string_pretty(&config)
                .map_err(|error| format!("无法修复配置: {error}"))?;
            fs::write(&path, repaired).map_err(|error| format!("无法修复配置: {error}"))?;
            trace_runtime("repaired malformed providers.json systemPrompt");
            Ok(config)
        }
    }
}

// Older builds could leave a malformed systemPrompt value after interrupted text input.
// Preserve every other setting and let the next normal save rewrite a valid JSON document.
fn recover_config_with_invalid_system_prompt(text: &str) -> Option<AppConfig> {
    let start = text.find("\"systemPrompt\"")?;
    let end = text[start..].find("\n  \"contextEnabled\"")? + start;
    let mut repaired = String::with_capacity(text.len());
    repaired.push_str(&text[..start]);
    repaired.push_str("\"systemPrompt\": \"\",");
    repaired.push_str(&text[end..]);
    serde_json::from_str(&repaired).ok()
}

fn save_config(config: &AppConfig) -> Result<(), String> {
    let path = config_path()?;
    let text =
        serde_json::to_string_pretty(config).map_err(|error| format!("无法序列化配置: {error}"))?;
    fs::write(path, text).map_err(|error| format!("无法保存配置: {error}"))
}

#[cfg(windows)]
const AUTOSTART_REGISTRY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
#[cfg(windows)]
const AUTOSTART_VALUE_NAME: &str = "Kero";

#[cfg(windows)]
fn expected_autostart_command() -> Result<String, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("无法读取 Kero 程序路径: {error}"))?;
    Ok(format!("\"{}\" --autostart", executable.display()))
}

#[tauri::command]
fn get_autostart_enabled() -> Result<bool, String> {
    #[cfg(windows)]
    {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};

        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let key = match current_user.open_subkey(AUTOSTART_REGISTRY_PATH) {
            Ok(key) => key,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("无法读取 Windows 启动项: {error}")),
        };
        let command: String = match key.get_value(AUTOSTART_VALUE_NAME) {
            Ok(command) => command,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("无法读取 Kero 启动项: {error}")),
        };
        return Ok(command.eq_ignore_ascii_case(&expected_autostart_command()?));
    }
    #[cfg(not(windows))]
    Ok(false)
}

#[tauri::command]
fn set_autostart_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};

        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        if enabled {
            let (key, _) = current_user
                .create_subkey(AUTOSTART_REGISTRY_PATH)
                .map_err(|error| format!("无法创建 Windows 启动项: {error}"))?;
            key.set_value(AUTOSTART_VALUE_NAME, &expected_autostart_command()?)
                .map_err(|error| format!("无法保存 Kero 启动项: {error}"))?;
        } else {
            let key = match current_user
                .open_subkey_with_flags(AUTOSTART_REGISTRY_PATH, winreg::enums::KEY_SET_VALUE)
            {
                Ok(key) => key,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(format!("无法打开 Windows 启动项: {error}")),
            };
            if let Err(error) = key.delete_value(AUTOSTART_VALUE_NAME) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("无法删除 Kero 启动项: {error}"));
                }
            }
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    Err("开机自启目前仅支持 Windows。".to_string())
}

#[tauri::command]
fn should_start_hidden() -> bool {
    STARTED_BY_AUTOSTART.load(Ordering::SeqCst)
}

fn encrypted_secrets_path() -> Result<PathBuf, String> {
    Ok(config_path()?.with_file_name("secrets.json"))
}

fn load_secrets() -> Result<EncryptedSecrets, String> {
    let path = encrypted_secrets_path()?;
    if !path.exists() {
        return Ok(EncryptedSecrets::default());
    }
    let text = fs::read_to_string(&path).map_err(|error| format!("无法读取加密密钥: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("加密密钥文件无效: {error}"))
}

fn save_secrets(secrets: &EncryptedSecrets) -> Result<(), String> {
    let path = encrypted_secrets_path()?;
    let text = serde_json::to_string_pretty(secrets)
        .map_err(|error| format!("无法序列化加密密钥: {error}"))?;
    fs::write(path, text).map_err(|error| format!("无法保存加密密钥: {error}"))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if value.len() % 2 != 0 {
        return Err("加密密钥数据损坏".to_string());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char)
                .to_digit(16)
                .ok_or_else(|| "加密密钥数据无效".to_string())?;
            let low = (pair[1] as char)
                .to_digit(16)
                .ok_or_else(|| "加密密钥数据无效".to_string())?;
            Ok(((high << 4) | low) as u8)
        })
        .collect()
}

#[cfg(windows)]
fn dpapi_protect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy.len() as u32,
        pbData: entropy.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let success = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(format!(
            "Windows 加密失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    let protected =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut c_void);
    }
    Ok(protected)
}

#[cfg(windows)]
fn dpapi_unprotect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy.len() as u32,
        pbData: entropy.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let success = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(format!(
            "Windows 解密失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut c_void);
    }
    Ok(plaintext)
}

fn read_secret(provider_id: &str) -> Result<Option<String>, String> {
    let secrets = load_secrets()?;
    let Some(encoded) = secrets.keys.get(provider_id) else {
        return Ok(None);
    };
    let encrypted = decode_hex(encoded)?;
    let plaintext = dpapi_unprotect(&encrypted, provider_id.as_bytes())?;
    String::from_utf8(plaintext)
        .map(Some)
        .map_err(|_| "解密后的 API Key 无效".to_string())
}

fn write_secret(provider_id: &str, secret: &str) -> Result<(), String> {
    let encrypted = dpapi_protect(secret.as_bytes(), provider_id.as_bytes())?;
    let mut secrets = load_secrets()?;
    secrets
        .keys
        .insert(provider_id.to_string(), encode_hex(&encrypted));
    save_secrets(&secrets)?;
    if read_secret(provider_id)?.as_deref() != Some(secret) {
        return Err("API Key 保存验证失败".to_string());
    }
    Ok(())
}

fn delete_secret(provider_id: &str) -> Result<(), String> {
    let mut secrets = load_secrets()?;
    secrets.keys.remove(provider_id);
    save_secrets(&secrets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_image_extension_detects_supported_formats() {
        assert_eq!(
            generated_image_extension(b"\x89PNG\r\n\x1a\nimage"),
            Ok("png")
        );
        assert_eq!(
            generated_image_extension(&[0xff, 0xd8, 0xff, 0xdb]),
            Ok("jpg")
        );
        assert_eq!(generated_image_extension(b"RIFFxxxxWEBPimage"), Ok("webp"));
        assert!(generated_image_extension(b"not-an-image").is_err());
    }

    #[test]
    fn recovers_config_when_only_system_prompt_is_malformed() {
        let broken = r#"{
  "providers": [],
  "imageGeneration": {"provider":"openai","baseUrl":"https://example.com/v1","model":"image-test"},
  "systemPrompt": "broken value
  "contextEnabled": true
}"#;
        let recovered = recover_config_with_invalid_system_prompt(broken)
            .expect("the remaining configuration should survive recovery");
        assert_eq!(recovered.image_generation.model, "image-test");
        assert!(recovered.context_enabled);
        assert!(recovered.system_prompt.is_empty());
    }

    #[test]
    fn dpapi_round_trip_preserves_api_keys() {
        let provider_id = format!("kero-dpapi-test-{}", Uuid::new_v4());
        let api_key = "test-key-for-local-dpapi-verification";
        write_secret(&provider_id, api_key).expect("temporary key should encrypt and save");
        assert_eq!(
            read_secret(&provider_id).expect("temporary key should decrypt"),
            Some(api_key.to_string())
        );
        delete_secret(&provider_id).expect("temporary key should be removed");
        assert_eq!(
            read_secret(&provider_id).expect("removed key should be absent"),
            None
        );
    }

    #[test]
    fn selects_the_dashscope_qwen_audio_flash_protocol() {
        assert_eq!(
            dashscope_asr_origin("https://dashscope.aliyuncs.com/compatible-mode/v1"),
            Some("https://dashscope.aliyuncs.com")
        );
        assert!(is_dashscope_qwen_audio_flash_model(
            "qwen-audio-3.0-asr-flash"
        ));
        assert!(is_dashscope_qwen_audio_flash_model(
            "qwen-audio-3.0-asr-flash-2026-08-01"
        ));
        assert!(!is_dashscope_qwen_audio_flash_model(
            "qwen-audio-3.0-asr-flash-filetrans"
        ));
        assert!(!is_dashscope_qwen_audio_flash_model(
            "qwen-audio-3.0-asr-flash-streaming"
        ));
    }

    #[test]
    fn rejects_dashscope_long_audio_models_for_short_dictation() {
        assert!(is_dashscope_long_audio_asr_model("fun-asr"));
        assert!(is_dashscope_long_audio_asr_model("fun-asr-2025-11-07"));
        assert!(is_dashscope_long_audio_asr_model("fun-asr-mtl"));
        assert!(!is_dashscope_long_audio_asr_model(
            "fun-asr-flash-2026-06-15"
        ));
        assert!(!is_dashscope_long_audio_asr_model("fun-asr-realtime"));
    }

    #[test]
    fn computer_action_parser_normalizes_coordinates() {
        let action = parse_computer_action(r#"```json
            {"action":"click","x":1.4,"y":-0.2,"description":"点击搜索框","requiresConfirmation":false}
        ```"#).expect("computer action should parse");
        assert_eq!(action.action, "click");
        assert_eq!(action.x, Some(1.0));
        assert_eq!(action.y, Some(0.0));
    }

    #[test]
    fn computer_action_parser_ignores_following_model_text() {
        let action = parse_computer_action(
            r#"{"action":"click","x":0.5,"y":0.5} 下一步再观察屏幕。 {"note":"ignore"}"#,
        )
        .expect("the first complete action object should parse");
        assert_eq!(action.action, "click");
    }

    #[test]
    fn pointer_coordinate_normalization_accepts_percentages_and_image_pixels() {
        assert_eq!(
            normalize_pointer_coordinates(25.0, 80.0, 1600.0, 1000.0),
            (0.25, 0.8)
        );
        assert_eq!(
            normalize_pointer_coordinates(800.0, 500.0, 1600.0, 1000.0),
            (0.5, 0.5)
        );
    }

    #[test]
    fn computer_action_parser_marks_sensitive_actions() {
        let action = parse_computer_action(r#"{"action":"key","key":"enter","description":"发送消息","requiresConfirmation":false}"#)
            .expect("sensitive action should parse");
        assert!(action.requires_confirmation);
    }

    #[test]
    fn computer_action_parser_keeps_distinct_pointer_clicks() {
        let double_click = parse_computer_action(r#"{"action":"double_click","x":0.4,"y":0.6}"#)
            .expect("double-click action should parse");
        let right_click = parse_computer_action(r#"{"action":"right_click","x":0.4,"y":0.6}"#)
            .expect("right-click action should parse");
        assert_eq!(double_click.action, "double_click");
        assert_eq!(right_click.action, "right_click");
    }

    #[test]
    fn computer_action_parser_keeps_verification_evidence() {
        let retry = parse_computer_action(
            r#"{"action":"click","x":0.4,"y":0.6,"retryEvidence":"the dialog is still visible"}"#,
        )
        .expect("retry action should parse");
        let done = parse_computer_action(
            r#"{"action":"done","message":"saved","finalEvidence":"the saved indicator is visible"}"#,
        )
        .expect("done action should parse");
        assert_eq!(
            retry.retry_evidence.as_deref(),
            Some("the dialog is still visible")
        );
        assert_eq!(
            done.final_evidence.as_deref(),
            Some("the saved indicator is visible")
        );
    }

    #[test]
    fn computer_action_parser_normalizes_pointer_action_aliases() {
        let action = parse_computer_action(r#"{"action":"doubleClick","x":0.4,"y":0.6}"#)
            .expect("camel-case double click should parse");
        let shortcut =
            parse_computer_action(r#"{"action":"open_desktop_shortcut","x":0.4,"y":0.6}"#)
                .expect("legacy shortcut action should parse");
        assert_eq!(action.action, "double_click");
        assert_eq!(shortcut.action, "double_click");
    }

    #[test]
    fn computer_action_parser_supports_existing_window_actions() {
        let activate = parse_computer_action(
            r#"{"action":"focusWindow","windowTarget":"hwnd:1a2b","observationSequence":7}"#,
        )
        .expect("focus-window action should parse");
        let maximize = parse_computer_action(
            r#"{"action":"maximizeWindow","windowTarget":"hwnd:3c4d","observationSequence":8}"#,
        )
        .expect("maximize-window action should parse");
        assert_eq!(activate.action, "activate_window");
        assert_eq!(activate.window_target.as_deref(), Some("hwnd:1a2b"));
        assert_eq!(activate.observation_sequence, Some(7));
        assert_eq!(maximize.action, "maximize_window");
        assert_eq!(maximize.window_target.as_deref(), Some("hwnd:3c4d"));
        assert_eq!(maximize.observation_sequence, Some(8));
    }

    #[test]
    fn screen_translation_defaults_to_auto_detect_and_simplified_chinese() {
        let settings = normalized_translation_settings(&AppConfig::default());
        assert_eq!(settings.source_language, "auto");
        assert_eq!(settings.target_language, "zh-CN");
    }

    #[test]
    fn screen_translation_change_detection_ignores_tiny_noise() {
        let previous = vec![8u8; 960];
        let mut small_noise = previous.clone();
        small_noise[10] = 12;
        small_noise[30] = 12;
        assert!(!screen_translation_changed(&previous, &small_noise));

        let mut changed = previous.clone();
        for value in changed.iter_mut().take(30) {
            *value = 16;
        }
        assert!(screen_translation_changed(&previous, &changed));
    }

    #[test]
    fn computer_action_parser_supports_drag_and_held_keys() {
        let drag = parse_computer_action(
            r#"{"action":"drag_to","x":0.1,"y":0.2,"endX":0.8,"endY":0.7,"duration":640}"#,
        )
        .expect("drag should parse");
        assert_eq!(drag.action, "drag");
        assert_eq!(
            (drag.end_x, drag.end_y, drag.duration),
            (Some(0.8), Some(0.7), Some(640))
        );
        let held =
            parse_computer_action(r#"{"action":"key_hold","keys":["w","shift"],"duration":500}"#)
                .expect("held keys should parse");
        assert_eq!(held.action, "hold_keys");
        assert_eq!(
            held.keys.as_deref(),
            Some(["w".to_string(), "shift".to_string()].as_slice())
        );
    }

    #[test]
    fn pointer_target_uses_physical_screen_dimensions() {
        assert_eq!(
            pointer_target_for_screen(0.5, 0.5, 0, 0, 2560, 1600),
            (1280, 800)
        );
        assert_eq!(
            pointer_target_for_screen(1.0, 1.0, 120, 80, 2560, 1600),
            (2679, 1679)
        );
    }

    #[test]
    fn computer_intent_distinguishes_execution_from_advice() {
        assert!(looks_like_explicit_computer_task(
            "帮我在桌面打开微信，然后给小明发一条消息"
        ));
        assert!(looks_like_explicit_computer_task(
            "把浏览器打开并搜索今天的天气"
        ));
        assert!(looks_like_explicit_computer_task("打开托盘里的 QQ"));
        assert!(!looks_like_explicit_computer_task("怎么打开微信？"));
        assert!(!looks_like_explicit_computer_task(
            "请介绍一下如何在浏览器里搜索网页"
        ));
    }

    #[test]
    fn computer_intent_parser_accepts_real_model_wrappers() {
        assert_eq!(parse_computer_control_intent("CONTROL"), Some(true));
        assert_eq!(parse_computer_control_intent("`CONTROL`。"), Some(true));
        assert_eq!(
            parse_computer_control_intent("The user wants an immediate UI operation.\nCONTROL"),
            Some(true)
        );
        assert_eq!(
            parse_computer_control_intent("```text\nCHAT\n```"),
            Some(false)
        );
        assert_eq!(
            parse_computer_control_intent("CONTROL, not CHAT"),
            Some(true)
        );
        assert_eq!(
            parse_computer_control_intent("CHAT, not CONTROL"),
            Some(false)
        );
        assert_eq!(parse_computer_control_intent("无法判断"), None);
    }

    #[test]
    fn qwen_transcript_deduplicates_replayed_completed_segments() {
        let mut transcript = RealtimeDictationTranscript::new();
        transcript.commit_segment("你好, ");
        transcript.commit_segment("你好, ");
        transcript.commit_segment("今天很好");
        assert_eq!(transcript.full_text(), "你好,今天很好");
    }

    #[test]
    fn qwen_transcript_merges_segment_boundary_overlap() {
        let mut transcript = RealtimeDictationTranscript::new();
        transcript.commit_segment("部署 Kero, ");
        transcript.commit_segment("Kero, 然后重启");
        assert_eq!(transcript.full_text(), "部署 Kero, 然后重启");
    }

    #[test]
    fn realtime_preconnect_fingerprint_changes_with_context() {
        let baseline = realtime_preconnect_fingerprint("wss://example.test", "qwen3-asr-flash-realtime", 16_000, Some("Kero"));
        assert_ne!(baseline, realtime_preconnect_fingerprint("wss://example.test", "qwen3-asr-flash-realtime", 16_000, Some("Codex")));
        assert_ne!(baseline, realtime_preconnect_fingerprint("wss://example.test", "other-model", 16_000, Some("Kero")));
    }

    #[test]
    fn realtime_error_messages_are_actionable() {
        assert!(friendly_realtime_error("401 Unauthorized").contains("API Key"));
        assert!(friendly_realtime_error("rate limit exceeded").contains("额度"));
        assert!(friendly_realtime_error("connection timeout").contains("超时"));
    }
}

fn secret_entry(provider_id: &str) -> Result<Entry, String> {
    Ok(Entry {
        provider_id: provider_id.to_string(),
    })
}

fn has_secret(provider_id: &str) -> bool {
    read_secret(provider_id)
        .map(|secret| secret.is_some_and(|value| !value.is_empty()))
        .unwrap_or(false)
}

fn public_provider(provider: &StoredProvider, default_provider: &Option<String>) -> PublicProvider {
    PublicProvider {
        id: provider.id.clone(),
        name: provider.name.clone(),
        kind: provider.kind.clone(),
        base_url: provider.base_url.clone(),
        model: provider.model.clone(),
        has_key: has_secret(&provider.id),
        is_default: default_provider.as_deref() == Some(provider.id.as_str()),
    }
}

fn public_image_generation_config(config: &ImageGenerationSettings) -> PublicImageGenerationConfig {
    PublicImageGenerationConfig {
        provider: config.provider.clone(),
        base_url: config.base_url.clone(),
        model: config.model.clone(),
        has_key: has_secret(IMAGE_GENERATION_SECRET_ID),
    }
}

#[tauri::command]
fn get_image_generation_config() -> Result<PublicImageGenerationConfig, String> {
    Ok(public_image_generation_config(
        &load_config()?.image_generation,
    ))
}

#[tauri::command]
fn save_image_generation_config(
    config: ImageGenerationConfigInput,
) -> Result<PublicImageGenerationConfig, String> {
    let provider = config.provider.trim().to_ascii_lowercase();
    if provider != "openai" && provider != "compatible" {
        return Err("图片生成仅支持 OpenAI Images 或 OpenAI 兼容接口".to_string());
    }
    let model = config.model.trim();
    let base_url = config.base_url.trim().trim_end_matches('/');
    let api_key = config.api_key.unwrap_or_default().trim().to_string();
    let has_existing_key = has_secret(IMAGE_GENERATION_SECRET_ID);
    let mut app_config = load_config()?;

    if model.is_empty() {
        return Err("请填写图片模型名称".to_string());
    }
    if provider == "compatible" && base_url.is_empty() {
        return Err("兼容接口需要填写服务地址".to_string());
    }
    if api_key.is_empty() && !has_existing_key {
        return Err("请填写图片生成 API Key".to_string());
    }
    if !api_key.is_empty() {
        write_secret(IMAGE_GENERATION_SECRET_ID, &api_key)
            .map_err(|error| format!("无法保存图片生成密钥: {error}"))?;
    }

    app_config.image_generation = ImageGenerationSettings {
        provider,
        base_url: base_url.to_string(),
        model: model.to_string(),
    };
    save_config(&app_config)?;
    Ok(public_image_generation_config(&app_config.image_generation))
}

fn public_dictation_asr_config(config: &DictationAsrSettings) -> PublicDictationAsrConfig {
    PublicDictationAsrConfig {
        base_url: config.base_url.clone(),
        model: config.model.clone(),
        has_key: has_secret(DICTATION_ASR_SECRET_ID),
    }
}

#[tauri::command]
fn get_dictation_asr_config() -> Result<PublicDictationAsrConfig, String> {
    Ok(public_dictation_asr_config(&load_config()?.dictation_asr))
}

#[tauri::command]
fn save_dictation_asr_config(
    config: DictationAsrConfigInput,
) -> Result<PublicDictationAsrConfig, String> {
    let base_url = config.base_url.trim().trim_end_matches('/');
    let model = config.model.trim();
    let api_key = config.api_key.unwrap_or_default().trim().to_string();
    let has_existing_key = has_secret(DICTATION_ASR_SECRET_ID);
    if base_url.is_empty() || !(base_url.starts_with("https://") || base_url.starts_with("http://"))
    {
        return Err("请填写 AI 语音识别服务地址".to_string());
    }
    if model.is_empty() {
        return Err("请填写 AI 语音识别模型名称".to_string());
    }
    if api_key.is_empty() && !has_existing_key {
        return Err("请填写 AI 语音识别 API Key".to_string());
    }
    if !api_key.is_empty() {
        write_secret(DICTATION_ASR_SECRET_ID, &api_key)
            .map_err(|error| format!("无法保存 AI 语音识别密钥: {error}"))?;
    }
    let mut app_config = load_config()?;
    app_config.dictation_asr = DictationAsrSettings {
        base_url: base_url.to_string(),
        model: model.to_string(),
    };
    save_config(&app_config)?;
    invalidate_realtime_preconnect();
    Ok(public_dictation_asr_config(&app_config.dictation_asr))
}

fn dictation_audio_extension(mime_type: &str) -> Result<&'static str, String> {
    match mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "audio/webm" => Ok("webm"),
        "audio/ogg" => Ok("ogg"),
        "audio/wav" | "audio/x-wav" => Ok("wav"),
        "audio/mpeg" | "audio/mp3" => Ok("mp3"),
        "audio/mp4" | "audio/x-m4a" => Ok("m4a"),
        _ => Err("当前系统原生录制的音频格式不受 AI 语音识别接口支持".to_string()),
    }
}

fn dashscope_asr_origin(base_url: &str) -> Option<&'static str> {
    let base_url = base_url.trim().to_ascii_lowercase();
    if base_url.starts_with("https://dashscope-intl.aliyuncs.com") {
        Some("https://dashscope-intl.aliyuncs.com")
    } else if base_url.starts_with("https://dashscope.aliyuncs.com") {
        Some("https://dashscope.aliyuncs.com")
    } else {
        None
    }
}

fn is_dashscope_qwen_audio_flash_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model == "qwen-audio-3.0-asr-flash"
        || (model.starts_with("qwen-audio-3.0-asr-flash-")
            && !model.contains("filetrans")
            && !model.contains("streaming"))
}

fn is_dashscope_long_audio_asr_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    (model == "fun-asr" || model.starts_with("fun-asr-") || model.starts_with("fun-asr-mtl"))
        && !model.contains("flash")
        && !model.contains("realtime")
}

fn unsupported_dashscope_dictation_model_message(model: &str) -> String {
    format!(
        "模型“{model}”是百炼的长音频异步转写模型，需要公网音频 URL 和任务轮询，不适合 Kero 的按住 Alt 短句听写。请改用 qwen-audio-3.0-asr-flash。"
    )
}

#[tauri::command]
async fn transcribe_dictation_audio(
    audio_base64: String,
    mime_type: String,
) -> Result<String, String> {
    let config = load_config()?.dictation_asr;
    if config.model.trim().is_empty() || config.base_url.trim().is_empty() {
        return Err("请先在设置中配置 AI 语音识别".to_string());
    }
    let key = read_secret(DICTATION_ASR_SECRET_ID)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "AI 语音识别 API Key 尚未保存".to_string())?;
    let audio = BASE64
        .decode(audio_base64.trim())
        .map_err(|_| "无法读取录制的音频数据".to_string())?;
    if audio.is_empty() || audio.len() > MAX_DICTATION_AUDIO_BYTES {
        return Err("录音为空或超过 12 MB 限制，请缩短单次听写".to_string());
    }
    let extension = dictation_audio_extension(&mime_type)?;
    let mime = mime_type.split(';').next().unwrap_or("audio/webm").trim();
    let base_url = config.base_url.trim_end_matches('/');
    let dashscope_origin = dashscope_asr_origin(base_url);
    let model_name = config.model.trim();
    trace_runtime(&format!(
        "dictation transcription request model={model_name} mime={mime} bytes={} dashscope={}",
        audio.len(),
        dashscope_origin.is_some()
    ));
    if dashscope_origin.is_some() && is_dashscope_long_audio_asr_model(model_name) {
        return Err(unsupported_dashscope_dictation_model_message(model_name));
    }
    let response = if dashscope_origin.is_some() && is_dashscope_qwen_audio_flash_model(model_name)
    {
        let audio_url = format!("data:{mime};base64,{}", BASE64.encode(&audio));
        let request = serde_json::json!({
            "model": model_name,
            "input": {
                "messages": [{
                    "role": "user",
                    "content": [{
                        "type": "input_audio",
                        "input_audio": { "data": audio_url }
                    }]
                }]
            },
            "parameters": { "format": extension }
        });
        shared_http_client()
            .post(format!(
                "{}/api/v1/services/aigc/multimodal-generation/generation",
                dashscope_origin.expect("DashScope origin was checked")
            ))
            .header("X-DashScope-SSE", "disable")
            .bearer_auth(key)
            .json(&request)
            .send()
            .await
    } else if dashscope_origin.is_some() && model_name.starts_with("qwen3-asr-") {
        let audio_url = format!("data:{mime};base64,{}", BASE64.encode(&audio));
        let request = serde_json::json!({
            "model": model_name,
            "input": {
                "messages": [{
                    "role": "user",
                    "content": [{ "audio": audio_url }]
                }]
            },
            "parameters": {
                "result_format": "message",
                "asr_options": { "language": "zh", "enable_lid": true }
            }
        });
        shared_http_client()
            .post(format!(
                "{}/api/v1/services/aigc/multimodal-generation/generation",
                dashscope_origin.expect("DashScope origin was checked")
            ))
            .bearer_auth(key)
            .json(&request)
            .send()
            .await
    } else {
        let audio_part = reqwest::multipart::Part::bytes(audio)
            .file_name(format!("kero-dictation.{extension}"))
            .mime_str(mime)
            .map_err(|error| format!("无法准备语音文件: {error}"))?;
        let form = reqwest::multipart::Form::new()
            .text("model", config.model.clone())
            .part("file", audio_part);
        shared_http_client()
            .post(format!("{base_url}/audio/transcriptions"))
            .bearer_auth(key)
            .multipart(form)
            .send()
            .await
    }
    .map_err(|error| format!("无法连接 AI 语音识别服务: {error}"))?;
    let status = response.status();
    let body_text = response
        .text()
        .await
        .map_err(|error| format!("无法读取 AI 语音识别结果: {error}"))?;
    let body = serde_json::from_str::<Value>(&body_text).map_err(|_| {
        let summary = body_text.split_whitespace().collect::<Vec<_>>().join(" ");
        let summary = summary.chars().take(280).collect::<String>();
        if summary.is_empty() {
            format!("AI 语音识别服务返回了空响应 (HTTP {status})")
        } else {
            format!("AI 语音识别服务返回了非 JSON 响应 (HTTP {status}): {summary}")
        }
    })?;
    let response_code = body.get("code").and_then(Value::as_str).unwrap_or("-");
    let response_message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("-")
        .chars()
        .take(180)
        .collect::<String>();
    trace_runtime(&format!(
        "dictation transcription response status={status} code={response_code} message={response_message}"
    ));
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    body.get("text")
        .and_then(Value::as_str)
        .or_else(|| body.pointer("/result/text").and_then(Value::as_str))
        .or_else(|| body.pointer("/output/text").and_then(Value::as_str))
        .or_else(|| {
            body.pointer("/output/output/sentence/text")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            body.pointer("/output/choices/0/message/content/0/text")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            body.pointer("/output/choices/0/message/content")
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "AI 语音识别没有返回文本".to_string())
}

fn dashscope_realtime_asr_origin(base_url: &str) -> Option<&'static str> {
    if base_url.starts_with("https://dashscope-intl.aliyuncs.com") {
        Some("wss://dashscope-intl.aliyuncs.com")
    } else if base_url.starts_with("https://dashscope.aliyuncs.com") {
        Some("wss://dashscope.aliyuncs.com")
    } else {
        None
    }
}

fn dictation_realtime_text(value: &Value) -> Option<&str> {
    value
        .pointer("/payload/output/sentence/text")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .pointer("/output/sentence/text")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .pointer("/payload/output/text")
                .and_then(Value::as_str)
        })
}

fn realtime_dictation_sessions() -> &'static Mutex<HashMap<String, RealtimeDictationSession>> {
    REALTIME_DICTATION_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn emit_realtime_dictation_event(
    app: &AppHandle,
    session_id: &str,
    text: Option<&str>,
    full_text: Option<&str>,
    done: bool,
    error: Option<&str>,
) {
    let _ = app.emit_to(
        "main",
        "kero:realtime-dictation",
        json!({
            "sessionId": session_id,
            "text": text,
            "fullText": full_text,
            "done": done,
            "error": error,
        }),
    );
}

/// 一次流式听写会话内累积的完整转写：已完成句 + 当前进行中的句子。
/// 事件里必须带全量文本，否则前端只能拿到"当前句"，说多句时前面的句子会被覆盖。
struct RealtimeDictationTranscript {
    committed: String,
    current: String,
    last_begin_time: Option<i64>,
}

impl RealtimeDictationTranscript {
    fn new() -> Self {
        Self {
            committed: String::new(),
            current: String::new(),
            last_begin_time: None,
        }
    }

    fn full_text(&self) -> String {
        format!("{}{}", self.committed, self.current)
    }

    /// Qwen 在重连或网络抖动后可能重复发送已完成分句；追加时去掉精确重复和边界重叠。
    fn commit_segment(&mut self, segment: &str) {
        let segment = segment.trim();
        if segment.is_empty() || self.committed.ends_with(segment) {
            self.current.clear();
            return;
        }
        let committed = self.committed.chars().collect::<Vec<_>>();
        let incoming = segment.chars().collect::<Vec<_>>();
        let max_overlap = committed.len().min(incoming.len());
        let overlap = (1..=max_overlap)
            .rev()
            .find(|length| committed[committed.len() - *length..] == incoming[..*length])
            .unwrap_or(0);
        self.committed.extend(incoming[overlap..].iter());
        self.current.clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RealtimeDictationProtocol {
    /// qwen3-asr-*-realtime：OpenAI-realtime 风格协议（session.update / input_audio_buffer.append / server_vad）
    QwenRealtime,
    /// 旧 api-ws/v1/inference 双工协议（run-task / result-generated / finish-task）
    DashScopeDuplex,
}

fn dictation_realtime_protocol(model: &str) -> RealtimeDictationProtocol {
    let model = model.trim().to_ascii_lowercase();
    if model.contains("qwen3-asr") && model.contains("realtime") {
        RealtimeDictationProtocol::QwenRealtime
    } else {
        RealtimeDictationProtocol::DashScopeDuplex
    }
}

fn is_stream_dictation_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.contains("realtime") || model.contains("streaming")
}

fn is_qwen3_asr_realtime_model(model: &str) -> bool {
    dictation_realtime_protocol(model) == RealtimeDictationProtocol::QwenRealtime
}

async fn run_qwen_realtime_dictation(
    mut socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    mut receiver: tokio::sync::mpsc::Receiver<RealtimeDictationCommand>,
    app: AppHandle,
    session_id: String,
    origin: String,
    model: String,
    key: String,
    sample_rate: u32,
    context: Option<String>,
) {
    let mut transcript = RealtimeDictationTranscript::new();
    let mut finishing = false;
    let mut reconnect_attempted = false;
    let terminal = |message: String, transcript: &RealtimeDictationTranscript| {
        emit_realtime_dictation_event(&app, &session_id, None, Some(&transcript.full_text()), true, Some(&message));
    };
    loop {
        tokio::select! {
            command = receiver.recv(), if !finishing => match command {
                Some(RealtimeDictationCommand::Audio(frame)) => {
                    let payload = json!({
                        "type": "input_audio_buffer.append",
                        "audio": BASE64.encode(&frame),
                    }).to_string();
                    if let Err(error) = socket.send(Message::Text(payload.clone().into())).await {
                        // 只恢复一次，且只回放这一帧（约 100ms）明确未发送成功的 PCM；
                        // 已成功 send 的历史音频绝不重放，避免网络抖动造成重复转写。
                        if !reconnect_attempted {
                            reconnect_attempted = true;
                            match reconnect_qwen_realtime_socket(&origin, &model, &key, sample_rate, context.as_deref()).await {
                                Ok(mut recovered) => match recovered.send(Message::Text(payload.into())).await {
                                    Ok(()) => { socket = recovered; continue; }
                                    Err(retry_error) => terminal(format!("实时语音识别重连后无法发送录音: {retry_error}"), &transcript),
                                },
                                Err(retry_error) => terminal(format!("实时语音识别重连失败: {retry_error}"), &transcript),
                            }
                        } else {
                            terminal(format!("无法发送实时录音: {error}"), &transcript);
                        }
                        break;
                    }
                }
                Some(RealtimeDictationCommand::Finish) | None => {
                    finishing = true;
                    if let Err(error) = socket.send(Message::Text(json!({ "type": "session.finish" }).to_string().into())).await {
                        terminal(format!("无法结束实时语音识别: {error}"), &transcript);
                        break;
                    }
                }
            },
            message = socket.next() => match message {
                Some(Ok(Message::Text(message))) => {
                    let Ok(value) = serde_json::from_str::<Value>(&message) else { continue; };
                    let event = value.get("type").and_then(Value::as_str).unwrap_or_default();
                    match event {
                        "conversation.item.input_audio_transcription.text" => {
                            let text = value.get("text").and_then(Value::as_str).unwrap_or_default();
                            let stash = value.get("stash").and_then(Value::as_str).unwrap_or_default();
                            let preview = format!("{text}{stash}");
                            let preview = preview.trim();
                            if !preview.is_empty() {
                                transcript.current = preview.to_string();
                                let full = transcript.full_text();
                                emit_realtime_dictation_event(&app, &session_id, Some(preview), Some(&full), false, None);
                            }
                        }
                        "conversation.item.input_audio_transcription.completed" => {
                            if let Some(segment) = value.get("transcript").and_then(Value::as_str) {
                                let segment = segment.trim();
                                if !segment.is_empty() {
                                    transcript.commit_segment(segment);
                                    let full = transcript.full_text();
                                    emit_realtime_dictation_event(&app, &session_id, Some(segment), Some(&full), false, None);
                                }
                            }
                        }
                        "conversation.item.input_audio_transcription.failed" | "error" => {
                            let message = value.pointer("/error/message")
                                .and_then(Value::as_str)
                                .or_else(|| value.get("message").and_then(Value::as_str))
                                .unwrap_or("实时语音识别转写失败");
                            terminal(friendly_realtime_error(message), &transcript);
                            break;
                        }
                        "session.finished" => {
                            let full = transcript.full_text();
                            emit_realtime_dictation_event(&app, &session_id, None, Some(&full), true, None);
                            break;
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Ping(payload))) => { let _ = socket.send(Message::Pong(payload)).await; }
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    let reason = format!("读取实时语音识别结果失败: {error}");
                    if !finishing && !reconnect_attempted {
                        reconnect_attempted = true;
                        match reconnect_qwen_realtime_socket(&origin, &model, &key, sample_rate, context.as_deref()).await {
                            Ok(recovered) => { socket = recovered; continue; }
                            Err(reconnect_error) => terminal(format!("实时语音识别重连失败: {reconnect_error}"), &transcript),
                        }
                    } else {
                        terminal(reason, &transcript);
                    }
                    break;
                }
                None => {
                    let reason = "实时语音识别服务已断开".to_string();
                    if !finishing && !reconnect_attempted {
                        reconnect_attempted = true;
                        match reconnect_qwen_realtime_socket(&origin, &model, &key, sample_rate, context.as_deref()).await {
                            Ok(recovered) => { socket = recovered; continue; }
                            Err(reconnect_error) => terminal(format!("实时语音识别重连失败: {reconnect_error}"), &transcript),
                        }
                    } else {
                        terminal(reason, &transcript);
                    }
                    break;
                }
            }
        }
    }
    realtime_dictation_sessions().lock().unwrap().remove(&session_id);
}

fn register_realtime_dictation_session(
) -> (
    String,
    tokio::sync::mpsc::Receiver<RealtimeDictationCommand>,
) {
    let session_id = Uuid::new_v4().to_string();
    // 100ms PCM 一帧，容量 40 即最多约 4 秒缓存：网络异常时内存有确定上限。
    let (sender, receiver) = tokio::sync::mpsc::channel(40);
    realtime_dictation_sessions()
        .lock()
        .unwrap()
        .insert(session_id.clone(), RealtimeDictationSession { sender });
    (session_id, receiver)
}

#[tauri::command]
async fn start_realtime_dictation(
    app: AppHandle,
    vocabulary: Option<String>,
) -> Result<RealtimeDictationSessionStart, String> {
    let config = load_config()?.dictation_asr;
    if !is_stream_dictation_model(&config.model) {
        return Err(
            "当前模型不是流式语音识别模型（模型名需包含 realtime 或 streaming）".to_string(),
        );
    }
    let origin = dashscope_realtime_asr_origin(config.base_url.trim_end_matches('/'))
        .ok_or_else(|| "流式语音识别目前仅支持 DashScope 服务地址".to_string())?;
    let sample_rate = if config.model.to_ascii_lowercase().contains("8k") {
        8_000
    } else {
        16_000
    };
    let key = read_secret(DICTATION_ASR_SECRET_ID)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "AI 语音识别 API Key 尚未保存".to_string())?;

    if is_qwen3_asr_realtime_model(&config.model) {
        // ---- Qwen3-ASR-Flash-Realtime：OpenAI-realtime 风格协议 ----
        if let Some(vocabulary) = vocabulary {
            let compact = vocabulary
                .split(['\n', ',', ';'])
                .map(str::trim)
                .filter(|term| !term.is_empty())
                .collect::<Vec<_>>()
                .join(";");
            if let Ok(mut stored) = REALTIME_VOCABULARY.lock() {
                *stored = Some(compact.chars().take(400).collect());
            }
        }
        let context = realtime_preconnect_vocabulary();
        let fingerprint = realtime_preconnect_fingerprint(origin, &config.model, sample_rate, context.as_deref());
        // 认领 Alt 按下时预建的已握手连接（6 秒内且配置完全一致）；没有则现场建连并握手。
        let preconnected = match REALTIME_PRECONNECT.lock() {
            Ok(mut pool) => match pool.take() {
                Some(preconnected)
                    if preconnected.created_at.elapsed() <= std::time::Duration::from_secs(6)
                        && preconnected.fingerprint == fingerprint =>
                {
                    Some(preconnected.socket)
                }
                _ => None,
            },
            Err(_) => None,
        };
        // 正式会话开始后，后台预连接若晚到也不得再进入池。
        REALTIME_PRECONNECT_GENERATION.fetch_add(1, Ordering::SeqCst);
        let socket = match preconnected {
            Some(socket) => socket,
            None => {
                let mut socket = connect_qwen_realtime_socket(origin, config.model.trim(), key.trim()).await?;
                qwen_realtime_handshake(&mut socket, sample_rate, context.as_deref(), 8).await?;
                socket
            }
        };

        let (session_id, receiver) = register_realtime_dictation_session();
        let background_app = app.clone();
        let background_session_id = session_id.clone();
        let reconnect_origin = origin.to_string();
        let reconnect_model = config.model.clone();
        let reconnect_key = key.clone();
        tauri::async_runtime::spawn(run_qwen_realtime_dictation(
            socket,
            receiver,
            background_app,
            background_session_id,
            reconnect_origin,
            reconnect_model,
            reconnect_key,
            sample_rate,
            context,
        ));
        return Ok(RealtimeDictationSessionStart {
            session_id,
            sample_rate,
        });
    }

    // ---- 旧 DashScope 双工协议（run-task / finish-task）----
    let mut request = format!("{origin}/api-ws/v1/inference")
        .into_client_request()
        .map_err(|error| format!("无法准备实时语音识别请求: {error}"))?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {key}")
            .parse()
            .map_err(|error| format!("无法准备实时语音识别认证: {error}"))?,
    );
    let (mut socket, _) = connect_async(request)
        .await
        .map_err(|error| format!("无法连接实时语音识别服务: {error}"))?;
    let task_id = Uuid::new_v4().simple().to_string();
    socket
        .send(Message::Text(
            json!({
                "header": { "task_id": task_id, "action": "run-task", "streaming": "duplex" },
                "payload": {
                    "model": config.model,
                    "task_group": "audio",
                    "task": "asr",
                    "function": "recognition",
                    "input": {},
                    "parameters": { "format": "pcm", "sample_rate": sample_rate }
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|error| format!("无法启动实时语音识别: {error}"))?;
    let started = tokio::time::timeout(std::time::Duration::from_secs(8), socket.next())
        .await
        .map_err(|_| "实时语音识别启动超时".to_string())?
        .ok_or_else(|| "实时语音识别服务提前断开".to_string())?
        .map_err(|error| format!("实时语音识别启动失败: {error}"))?;
    let started = started
        .into_text()
        .map_err(|_| "实时语音识别返回了无效启动响应".to_string())?;
    let started: Value =
        serde_json::from_str(&started).map_err(|_| "实时语音识别返回了无效启动数据".to_string())?;
    match started.pointer("/header/event").and_then(Value::as_str) {
        Some("task-started") => {}
        Some("task-failed") => {
            return Err(started
                .pointer("/header/error_message")
                .and_then(Value::as_str)
                .unwrap_or("实时语音识别启动失败")
                .to_string())
        }
        _ => return Err("实时语音识别未能启动任务".to_string()),
    }

    let (session_id, mut receiver) = register_realtime_dictation_session();
    let background_app = app.clone();
    let background_session_id = session_id.clone();
    tauri::async_runtime::spawn(async move {
        let (mut writer, mut reader) = socket.split();
        let mut transcript = RealtimeDictationTranscript::new();
        let mut finishing = false;
        loop {
            tokio::select! {
                command = receiver.recv(), if !finishing => match command {
                    Some(RealtimeDictationCommand::Audio(frame)) => {
                        if let Err(error) = writer.send(Message::Binary(frame.into())).await {
                            transcript.current.clear();
                            emit_realtime_dictation_event(&background_app, &background_session_id, None, Some(&transcript.full_text()), true, Some(&format!("无法发送实时录音: {error}")));
                            break;
                        }
                    }
                    Some(RealtimeDictationCommand::Finish) | None => {
                        finishing = true;
                        if let Err(error) = writer.send(Message::Text(json!({
                            "header": { "task_id": task_id, "action": "finish-task", "streaming": "duplex" },
                            "payload": { "input": {} }
                        }).to_string().into())).await {
                            emit_realtime_dictation_event(&background_app, &background_session_id, None, Some(&transcript.full_text()), true, Some(&format!("无法结束实时语音识别: {error}")));
                            break;
                        }
                    }
                },
                message = reader.next() => match message {
                    Some(Ok(Message::Text(message))) => {
                        match serde_json::from_str::<Value>(&message) {
                            Ok(value) => {
                                let event = value.pointer("/header/event").and_then(Value::as_str).unwrap_or_default();
                                if event == "task-failed" {
                                    emit_realtime_dictation_event(&background_app, &background_session_id, None, Some(&transcript.full_text()), true, value.pointer("/header/error_message").and_then(Value::as_str));
                                    break;
                                }
                                if let Some(text) = dictation_realtime_text(&value).map(str::trim).filter(|text| !text.is_empty()) {
                                    // 每个事件只带"当前句"，句子切换（begin_time 变化）时把上一句转入 committed，
                                    // 否则前端整段替换会把已上屏的前几句覆盖掉。
                                    let begin_time = value
                                        .pointer("/payload/output/sentence/begin_time")
                                        .or_else(|| value.pointer("/output/sentence/begin_time"))
                                        .and_then(Value::as_i64);
                                    if let Some(begin_time) = begin_time {
                                        if transcript.last_begin_time.is_some_and(|previous| previous != begin_time)
                                            && !transcript.current.is_empty()
                                        {
                                            transcript.committed.push_str(&transcript.current);
                                        }
                                        transcript.last_begin_time = Some(begin_time);
                                    }
                                    transcript.current = text.to_string();
                                    let full = transcript.full_text();
                                    emit_realtime_dictation_event(&background_app, &background_session_id, Some(text), Some(&full), false, None);
                                }
                                if event == "task-finished" {
                                    let full = transcript.full_text();
                                    emit_realtime_dictation_event(&background_app, &background_session_id, None, Some(&full), true, None);
                                    break;
                                }
                            }
                            Err(_) => {}
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => { let _ = writer.send(Message::Pong(payload)).await; }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        emit_realtime_dictation_event(&background_app, &background_session_id, None, Some(&transcript.full_text()), true, Some(&format!("读取实时语音识别结果失败: {error}")));
                        break;
                    }
                    None => {
                        emit_realtime_dictation_event(&background_app, &background_session_id, None, Some(&transcript.full_text()), true, Some("实时语音识别服务已断开"));
                        break;
                    }
                }
            }
        }
        realtime_dictation_sessions()
            .lock()
            .unwrap()
            .remove(&background_session_id);
    });
    Ok(RealtimeDictationSessionStart {
        session_id,
        sample_rate,
    })
}

#[tauri::command]
fn push_realtime_dictation_audio(session_id: String, pcm_base64: String) -> Result<(), String> {
    let frame = BASE64
        .decode(pcm_base64.trim())
        .map_err(|_| "无法读取实时录音数据".to_string())?;
    if frame.is_empty() {
        return Ok(());
    }
    let sender = realtime_dictation_sessions()
        .lock()
        .unwrap()
        .get(&session_id)
        .map(|session| session.sender.clone())
        .ok_or_else(|| "实时语音识别会话已结束".to_string())?;
    match sender.try_send(RealtimeDictationCommand::Audio(frame)) {
        Ok(()) => Ok(()),
        // 网络短暂拥塞时丢弃过期音频帧：保留实时性与固定内存上限，不让 IPC 队列无限增长。
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err("实时语音识别会话已结束".to_string()),
    }
}

#[tauri::command]
fn finish_realtime_dictation(session_id: String) -> Result<(), String> {
    let sender = realtime_dictation_sessions()
        .lock()
        .unwrap()
        .get(&session_id)
        .map(|session| session.sender.clone())
        .ok_or_else(|| "实时语音识别会话已结束".to_string())?;
    sender
        .try_send(RealtimeDictationCommand::Finish)
        .map_err(|_| "实时语音识别会话已结束".to_string())
}

#[tauri::command]
async fn transcribe_realtime_dictation_audio(
    pcm_base64: String,
    sample_rate: u32,
) -> Result<String, String> {
    let config = load_config()?.dictation_asr;
    let base_url = config.base_url.trim_end_matches('/');
    if !is_stream_dictation_model(&config.model) {
        return Err("当前模型不是流式语音识别模型（模型名需包含 realtime 或 streaming）".to_string());
    }
    let origin = dashscope_realtime_asr_origin(base_url)
        .ok_or_else(|| "流式语音识别当前仅支持 DashScope 服务地址".to_string())?;
    let expected_rate = if config.model.contains("8k") {
        8_000
    } else {
        16_000
    };
    if sample_rate != expected_rate {
        return Err(format!("当前模型要求 {expected_rate} Hz PCM 音频"));
    }
    let key = read_secret(DICTATION_ASR_SECRET_ID)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "AI 语音识别 API Key 尚未保存".to_string())?;
    let audio = BASE64
        .decode(pcm_base64.trim())
        .map_err(|_| "无法读取实时录音数据".to_string())?;
    if audio.is_empty() || audio.len() > MAX_DICTATION_AUDIO_BYTES {
        return Err("录音为空或超过 12 MB 限制，请缩短单次听写".to_string());
    }

    if is_qwen3_asr_realtime_model(&config.model) {
        // ---- Qwen3-ASR-Flash-Realtime 协议回放 ----
        let mut request = format!("{origin}/api-ws/v1/realtime?model={}", config.model)
            .into_client_request()
            .map_err(|error| format!("无法准备实时语音识别请求: {error}"))?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {key}")
                .parse()
                .map_err(|error| format!("无法准备实时语音识别认证: {error}"))?,
        );
        request.headers_mut().insert(
            "OpenAI-Beta",
            "realtime=v1"
                .parse()
                .map_err(|error| format!("无法准备实时语音识别协议头: {error}"))?,
        );
        let (mut socket, _) = connect_async(request)
            .await
            .map_err(|error| format!("无法连接实时语音识别服务: {error}"))?;
        socket
            .send(Message::Text(
                json!({
                    "type": "session.update",
                    "session": {
                        "input_audio_format": "pcm",
                        "sample_rate": sample_rate,
                        "input_audio_transcription": { "language": "zh" },
                        "turn_detection": {
                            "type": "server_vad",
                            "silence_duration_ms": 500
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .map_err(|error| format!("无法配置实时语音识别会话: {error}"))?;

        let mut committed = String::new();
        let mut current = String::new();
        // 逐块回放；与实时路径不同，这里音频是整段现成的，发送与接收并行推进。
        let mut audio_offset = 0usize;
        let mut finish_sent = false;
        let (mut writer, mut reader) = socket.split();
        loop {
            tokio::select! {
                send_done = async {
                    if audio_offset < audio.len() {
                        let end = (audio_offset + 6_400).min(audio.len());
                        let payload = json!({
                            "type": "input_audio_buffer.append",
                            "audio": BASE64.encode(&audio[audio_offset..end]),
                        })
                        .to_string();
                        audio_offset = end;
                        writer.send(Message::Text(payload.into())).await
                    } else {
                        finish_sent = true;
                        writer.send(Message::Text(json!({ "type": "session.finish" }).to_string().into())).await
                    }
                }, if !finish_sent => {
                    send_done.map_err(|error| format!("无法发送实时录音: {error}"))?;
                }
                message = reader.next() => {
                    let message = message
                        .ok_or_else(|| "实时语音识别服务提前断开".to_string())?
                        .map_err(|error| format!("读取实时语音识别结果失败: {error}"))?;
                    if let Message::Text(message) = message {
                        let Ok(value) = serde_json::from_str::<Value>(&message) else { continue };
                        let event = value.get("type").and_then(Value::as_str).unwrap_or_default();
                        match event {
                            "conversation.item.input_audio_transcription.text" => {
                                let text = value.get("text").and_then(Value::as_str).unwrap_or_default();
                                let stash = value.get("stash").and_then(Value::as_str).unwrap_or_default();
                                let preview = format!("{text}{stash}");
                                current = preview.trim().to_string();
                            }
                            "conversation.item.input_audio_transcription.completed" => {
                                if let Some(segment) = value.get("transcript").and_then(Value::as_str) {
                                    let segment = segment.trim();
                                    if !segment.is_empty() {
                                        committed.push_str(segment);
                                        current.clear();
                                    }
                                }
                            }
                            "conversation.item.input_audio_transcription.failed" | "error" => {
                                return Err(value
                                    .pointer("/error/message")
                                    .and_then(Value::as_str)
                                    .or_else(|| value.get("message").and_then(Value::as_str))
                                    .unwrap_or("实时语音识别失败")
                                    .to_string());
                            }
                            "session.finished" => {
                                let text = format!("{committed}{current}");
                                let text = text.trim().to_string();
                                if text.is_empty() {
                                    return Err("实时语音识别没有听到有效内容".to_string());
                                }
                                return Ok(text);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // ---- 旧 DashScope 双工协议回放 ----
    let mut request = format!("{origin}/api-ws/v1/inference")
        .into_client_request()
        .map_err(|error| format!("无法准备实时语音识别请求: {error}"))?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {key}")
            .parse()
            .map_err(|error| format!("无法准备实时语音识别认证: {error}"))?,
    );
    let (mut socket, _) = connect_async(request)
        .await
        .map_err(|error| format!("无法连接实时语音识别服务: {error}"))?;
    let task_id = Uuid::new_v4().simple().to_string();
    let start = json!({
        "header": { "task_id": task_id, "action": "run-task", "streaming": "duplex" },
        "payload": {
            "model": config.model,
            "task_group": "audio",
            "task": "asr",
            "function": "recognition",
            "input": {},
            "parameters": { "format": "pcm", "sample_rate": sample_rate }
        }
    });
    socket
        .send(Message::Text(start.to_string().into()))
        .await
        .map_err(|error| format!("无法启动实时语音识别: {error}"))?;

    let started = tokio::time::timeout(std::time::Duration::from_secs(8), socket.next())
        .await
        .map_err(|_| "实时语音识别启动超时".to_string())?
        .ok_or_else(|| "实时语音识别服务提前断开".to_string())?
        .map_err(|error| format!("实时语音识别启动失败: {error}"))?;
    let started = started
        .into_text()
        .map_err(|_| "实时语音识别返回了无效启动响应".to_string())?;
    let started_json: Value =
        serde_json::from_str(&started).map_err(|_| "实时语音识别返回了无效启动数据".to_string())?;
    if started_json
        .pointer("/header/event")
        .and_then(Value::as_str)
        == Some("task-failed")
    {
        return Err(started_json
            .pointer("/header/error_message")
            .and_then(Value::as_str)
            .unwrap_or("实时语音识别启动失败")
            .to_string());
    }
    if started_json
        .pointer("/header/event")
        .and_then(Value::as_str)
        != Some("task-started")
    {
        return Err("实时语音识别未能启动任务".to_string());
    }

    for frame in audio.chunks(6_400) {
        socket
            .send(Message::Binary(frame.to_vec().into()))
            .await
            .map_err(|error| format!("无法发送实时录音: {error}"))?;
    }
    let finish = json!({
        "header": { "task_id": task_id, "action": "finish-task", "streaming": "duplex" },
        "payload": { "input": {} }
    });
    socket
        .send(Message::Text(finish.to_string().into()))
        .await
        .map_err(|error| format!("无法结束实时语音识别: {error}"))?;

    let mut text = String::new();
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(12), socket.next())
            .await
            .map_err(|_| "等待实时语音识别结果超时".to_string())?
            .ok_or_else(|| "实时语音识别服务提前断开".to_string())?
            .map_err(|error| format!("读取实时语音识别结果失败: {error}"))?;
        if let Message::Text(message) = message {
            let value: Value = serde_json::from_str(&message)
                .map_err(|_| "实时语音识别返回了无效结果".to_string())?;
            let event = value
                .pointer("/header/event")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if event == "task-failed" {
                return Err(value
                    .pointer("/header/error_message")
                    .and_then(Value::as_str)
                    .unwrap_or("实时语音识别失败")
                    .to_string());
            }
            if let Some(segment) = dictation_realtime_text(&value) {
                if !segment.trim().is_empty() {
                    text = segment.trim().to_string();
                }
            }
            if event == "task-finished" {
                break;
            }
        }
    }
    if text.is_empty() {
        Err("实时语音识别没有听到有效内容".to_string())
    } else {
        Ok(text)
    }
}

#[tauri::command]
fn list_providers() -> Result<Vec<PublicProvider>, String> {
    let config = load_config()?;
    Ok(config
        .providers
        .iter()
        .map(|provider| public_provider(provider, &config.default_provider))
        .collect())
}

#[tauri::command]
fn save_provider(provider: ProviderInput) -> Result<PublicProvider, String> {
    let mut config = load_config()?;
    let id = provider.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let normalized_name = provider.name.trim();
    let normalized_model = provider.model.trim();
    let api_key = provider
        .api_key
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    let has_existing_key = has_secret(&id);

    if normalized_name.is_empty() || normalized_model.is_empty() {
        return Err("请填写提供商名称和模型名称".to_string());
    }
    if !has_existing_key && api_key.is_empty() {
        return Err("请填写 API Key；Kero 会在保存后立即验证凭据存储".to_string());
    }

    let stored = StoredProvider {
        id: id.clone(),
        name: normalized_name.to_string(),
        kind: provider.kind,
        base_url: provider.base_url.trim().trim_end_matches('/').to_string(),
        model: normalized_model.to_string(),
    };

    if let Some(index) = config.providers.iter().position(|item| item.id == id) {
        config.providers[index] = stored.clone();
    } else {
        config.providers.push(stored.clone());
        if config.default_provider.is_none() {
            config.default_provider = Some(id.clone());
        }
    }

    if !api_key.is_empty() {
        let entry = secret_entry(&id)?;
        entry
            .set_password(&api_key)
            .map_err(|error| format!("无法保存密钥: {error}"))?;
        let persisted = entry
            .get_password()
            .map_err(|error| format!("密钥保存后无法验证: {error}"))?;
        if persisted != api_key {
            return Err("密钥保存校验失败，请重新输入 API Key".to_string());
        }
    }

    save_config(&config)?;
    Ok(public_provider(&stored, &config.default_provider))
}

#[tauri::command]
fn delete_provider(provider_id: String) -> Result<(), String> {
    let mut config = load_config()?;
    config
        .providers
        .retain(|provider| provider.id != provider_id);
    if config.default_provider.as_deref() == Some(provider_id.as_str()) {
        config.default_provider = config.providers.first().map(|provider| provider.id.clone());
    }
    if let Ok(entry) = secret_entry(&provider_id) {
        let _ = entry.delete_credential();
    }
    save_config(&config)
}

#[tauri::command]
fn set_default_provider(provider_id: String) -> Result<(), String> {
    let mut config = load_config()?;
    if !config
        .providers
        .iter()
        .any(|provider| provider.id == provider_id)
    {
        return Err("找不到这个提供商".to_string());
    }
    config.default_provider = Some(provider_id);
    save_config(&config)
}

#[tauri::command]
fn get_system_prompt() -> Result<String, String> {
    Ok(load_config()?.system_prompt)
}

#[tauri::command]
fn set_system_prompt(system_prompt: String) -> Result<(), String> {
    let mut config = load_config()?;
    config.system_prompt = system_prompt.trim().to_string();
    save_config(&config)
}

#[tauri::command]
fn get_context_enabled() -> Result<bool, String> {
    Ok(load_config()?.context_enabled)
}

#[tauri::command]
fn set_context_enabled(context_enabled: bool) -> Result<(), String> {
    let mut config = load_config()?;
    config.context_enabled = context_enabled;
    save_config(&config)
}

fn translation_language_name(code: &str) -> Option<&'static str> {
    match code {
        "auto" => Some("自动检测"),
        "en" => Some("英语"),
        "zh-CN" => Some("简体中文"),
        "ja" => Some("日语"),
        "ko" => Some("韩语"),
        "fr" => Some("法语"),
        "de" => Some("德语"),
        "es" => Some("西班牙语"),
        "ru" => Some("俄语"),
        _ => None,
    }
}

fn normalized_translation_settings(config: &AppConfig) -> TranslationSettings {
    let source_language = translation_language_name(&config.translation_source_language)
        .is_some()
        .then(|| config.translation_source_language.clone())
        .unwrap_or_else(default_translation_source_language);
    let target_language = translation_language_name(&config.translation_target_language)
        .filter(|_| config.translation_target_language != "auto")
        .is_some()
        .then(|| config.translation_target_language.clone())
        .unwrap_or_else(default_translation_target_language);
    TranslationSettings {
        source_language,
        target_language,
    }
}

#[tauri::command]
fn get_screen_translation_settings() -> Result<TranslationSettings, String> {
    Ok(normalized_translation_settings(&load_config()?))
}

#[tauri::command]
fn set_screen_translation_settings(
    source_language: String,
    target_language: String,
) -> Result<(), String> {
    if translation_language_name(&source_language).is_none() {
        return Err("不支持这个源语言。".to_string());
    }
    if target_language == "auto" || translation_language_name(&target_language).is_none() {
        return Err("不支持这个目标语言。".to_string());
    }
    if source_language == target_language {
        return Err("源语言和目标语言不能相同。".to_string());
    }
    let mut config = load_config()?;
    config.translation_source_language = source_language;
    config.translation_target_language = target_language;
    save_config(&config)
}

#[tauri::command]
fn get_computer_control_enabled() -> Result<bool, String> {
    Ok(load_config()?.computer_control_enabled)
}

#[tauri::command]
fn set_computer_control_enabled(computer_control_enabled: bool) -> Result<(), String> {
    let mut config = load_config()?;
    config.computer_control_enabled = computer_control_enabled;
    if !computer_control_enabled {
        COMPUTER_CONTROL_STOPPED.store(true, Ordering::SeqCst);
        #[cfg(windows)]
        restore_computer_cursor();
        config.computer_control_risk_mode = false;
    }
    save_config(&config)
}

#[tauri::command]
fn get_computer_control_risk_mode() -> Result<bool, String> {
    Ok(load_config()?.computer_control_risk_mode)
}

#[tauri::command]
fn set_computer_control_risk_mode(computer_control_risk_mode: bool) -> Result<(), String> {
    let mut config = load_config()?;
    if computer_control_risk_mode && !config.computer_control_enabled {
        return Err("请先开启 AI 操控电脑。".to_string());
    }
    config.computer_control_risk_mode = computer_control_risk_mode;
    save_config(&config)
}

#[tauri::command]
fn start_computer_control() -> Result<(), String> {
    if !load_config()?.computer_control_enabled {
        return Err("请先在设置中开启 AI 操控电脑。".to_string());
    }
    reset_computer_control_log();
    trace_computer_control("task started");
    clear_computer_mark();
    #[cfg(windows)]
    apply_computer_cursor()?;
    COMPUTER_CONTROL_STOPPED.store(false, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn stop_computer_control() {
    COMPUTER_CONTROL_STOPPED.store(true, Ordering::SeqCst);
    clear_computer_mark();
    #[cfg(windows)]
    restore_computer_cursor();
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpBridgeRequest {
    op: String,
    #[serde(default)]
    action: Option<ComputerAction>,
    #[serde(default)]
    actions: Option<Vec<ComputerAction>>,
    #[serde(default)]
    max_width: Option<u32>,
    #[serde(default)]
    max_height: Option<u32>,
    #[serde(default)]
    jpeg_quality: Option<u8>,
    #[serde(default)]
    color_mode: Option<String>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    next_action: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    aspect_ratio: Option<String>,
    #[serde(default)]
    count: Option<u8>,
    #[serde(default)]
    reference_images: Option<Vec<String>>,
}

fn mcp_bridge_result(result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => json!({ "ok": true, "result": value }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

fn publish_mcp_decision(
    app: &AppHandle,
    phase: Option<&str>,
    summary: Option<&str>,
    detail: Option<&str>,
    next_action: Option<&str>,
    active: bool,
) -> Result<Value, String> {
    let summary = summary.unwrap_or("正在分析当前画面").trim();
    if summary.is_empty() {
        return Err("MCP decision summary cannot be empty".to_string());
    }
    let phase = match phase.unwrap_or("observe") {
        "observe" | "decide" | "act" | "verify" | "complete" => phase.unwrap_or("observe"),
        _ => return Err("Unsupported MCP decision phase".to_string()),
    };
    let truncate = |value: Option<&str>, limit: usize| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(limit).collect::<String>())
    };
    let payload = json!({
        "active": active,
        "phase": phase,
        "summary": summary.chars().take(120).collect::<String>(),
        "detail": truncate(detail, 520),
        "nextAction": truncate(next_action, 180),
    });
    if let Some(main) = app.get_webview_window("main") {
        main.show().map_err(|error| error.to_string())?;
        main.set_skip_taskbar(true)
            .map_err(|error| error.to_string())?;
    }
    app.emit_to("main", "kero:mcp-decision", payload.clone())
        .map_err(|error| error.to_string())?;
    Ok(payload)
}

fn start_mcp_computer_control(app: &AppHandle, color_mode: Option<&str>) -> Result<Value, String> {
    let edge = app
        .get_webview_window("edge")
        .ok_or_else(|| "Kero edge window is not ready".to_string())?;
    if !EDGE_FRONTEND_READY.load(Ordering::SeqCst) {
        let main = app
            .get_webview_window("main")
            .ok_or_else(|| "Kero main window is not ready".to_string())?;
        if let Some(monitor) = main.current_monitor().map_err(|error| error.to_string())? {
            let monitor_position = monitor.position();
            let monitor_size = monitor.size();
            edge.set_size(PhysicalSize::new(64, 64))
                .map_err(|error| error.to_string())?;
            edge.set_position(PhysicalPosition::new(
                monitor_position.x + monitor_size.width as i32 - 1,
                monitor_position.y + monitor_size.height as i32 - 1,
            ))
            .map_err(|error| error.to_string())?;
        }
        edge.set_ignore_cursor_events(true)
            .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        show_window_without_activation(&edge)?;
    }

    let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    while !EDGE_FRONTEND_READY.load(Ordering::SeqCst) {
        if std::time::Instant::now() >= ready_deadline {
            let _ = edge.hide();
            return Err("Kero edge shader did not become ready in time".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    std::thread::sleep(std::time::Duration::from_millis(80));
    edge.hide().map_err(|error| error.to_string())?;
    show_edge_on_main_monitor(app)?;
    reset_computer_control_log();
    trace_computer_control("MCP control session started");
    clear_computer_mark();
    #[cfg(windows)]
    apply_computer_cursor()?;
    COMPUTER_CONTROL_STOPPED.store(false, Ordering::SeqCst);
    app.emit_to(
        "edge",
        "kero:edge-state",
        json!({
            "active": true,
            "energy": 0.72,
            "colorMode": if color_mode == Some("blue") { "blue" } else { "rainbow" }
        }),
    )
    .map_err(|error| error.to_string())?;
    publish_mcp_decision(
        app,
        Some("observe"),
        Some("MCP 已连接，正在观察屏幕"),
        Some("Kero 会在每次操作前显示 AI 的判断摘要。"),
        Some("读取当前画面"),
        true,
    )?;
    Ok(json!({ "active": true }))
}

#[tauri::command]
fn mark_edge_ready() {
    EDGE_FRONTEND_READY.store(true, Ordering::SeqCst);
}

fn stop_mcp_computer_control(app: &AppHandle) -> Result<Value, String> {
    stop_computer_control();
    publish_mcp_decision(
        app,
        Some("complete"),
        Some("MCP 电脑操控已结束"),
        None,
        Some("已恢复系统鼠标"),
        false,
    )?;
    app.emit_to(
        "edge",
        "kero:edge-state",
        json!({ "active": false, "energy": 0.0, "colorMode": "rainbow" }),
    )
    .map_err(|error| error.to_string())?;
    app.emit_to(
        "edge",
        "kero:computer-pointer",
        json!({ "x": 0.5, "y": 0.5, "active": false, "click": false }),
    )
    .ok();
    let app_handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(460));
        if COMPUTER_CONTROL_STOPPED.load(Ordering::SeqCst)
            && !SCREEN_TRANSLATION_ACTIVE.load(Ordering::SeqCst)
        {
            if let Some(edge) = app_handle.get_webview_window("edge") {
                let _ = edge.hide();
            }
        }
    });
    Ok(json!({ "active": false }))
}

fn handle_mcp_bridge_request(app: &AppHandle, request: McpBridgeRequest) -> Value {
    let result = match request.op.as_str() {
        "ping" => Ok(json!({ "name": "Kero", "bridgeVersion": 1 })),
        "start" => start_mcp_computer_control(app, request.color_mode.as_deref()),
        "stop" => stop_mcp_computer_control(app),
        "decision" => publish_mcp_decision(
            app,
            request.phase.as_deref(),
            request.summary.as_deref(),
            request.detail.as_deref(),
            request.next_action.as_deref(),
            true,
        ),
        "capture" => capture_screen_excluding_kero(
            app,
            request.max_width.unwrap_or(1600).clamp(320, 2560),
            request.max_height.unwrap_or(1080).clamp(240, 1600),
            request.jpeg_quality.unwrap_or(80).clamp(45, 95),
            take_computer_mark(),
        )
        .map(|image| json!({ "image": image })),
        "inspect_ui" => foreground_semantic_snapshot().map(|snapshot| {
            json!({
                "window": {
                    "id": snapshot.window_key,
                    "name": snapshot.window_name,
                    "class": snapshot.window_class,
                },
                "fingerprint": snapshot.fingerprint,
                "controls": snapshot.controls,
            })
        }),
        "generate_image" => request
            .prompt
            .as_deref()
            .ok_or_else(|| "MCP image generation request is missing a prompt".to_string())
            .and_then(|prompt| {
                tauri::async_runtime::block_on(generate_image(
                    app.clone(),
                    ImageGenerationRequest {
                        prompt: prompt.to_string(),
                        aspect_ratio: request.aspect_ratio.unwrap_or_else(|| "1:1".to_string()),
                        count: request.count.unwrap_or(1),
                        reference_images: request.reference_images.unwrap_or_default(),
                    },
                ))
                .map(|images| json!({ "images": images }))
            }),
        "execute" => request
            .action
            .ok_or_else(|| "MCP execute request is missing an action".to_string())
            .and_then(|action| {
                tauri::async_runtime::block_on(computer_execute_action(
                    app.clone(),
                    action,
                    Some(true),
                ))
                .map(|message| json!({ "message": message }))
            }),
        "execute_batch" => request
            .actions
            .ok_or_else(|| "MCP batch request is missing actions".to_string())
            .and_then(|actions| {
                if actions.is_empty() || actions.len() > 8 {
                    return Err("MCP batch must contain 1 to 8 actions".to_string());
                }
                tauri::async_runtime::block_on(async {
                    let mut messages = Vec::with_capacity(actions.len());
                    for action in actions {
                        messages
                            .push(computer_execute_action(app.clone(), action, Some(true)).await?);
                    }
                    Ok::<_, String>(json!({ "messages": messages }))
                })
            }),
        _ => Err(format!("Unsupported MCP bridge operation: {}", request.op)),
    };
    mcp_bridge_result(result)
}

fn serve_mcp_bridge_connection(app: &AppHandle, stream: TcpStream) -> Result<(), String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(185)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(185)))
        .map_err(|error| error.to_string())?;
    let mut writer = stream.try_clone().map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if bytes_read == 0 {
            return Ok(());
        }
        if line.len() > 8 * 1024 * 1024 {
            return Err("MCP bridge request is too large".to_string());
        }
        let request = serde_json::from_str::<McpBridgeRequest>(line.trim())
            .map_err(|error| format!("Invalid MCP bridge request: {error}"))?;
        let response = handle_mcp_bridge_request(app, request);
        serde_json::to_writer(&mut writer, &response).map_err(|error| error.to_string())?;
        writer.write_all(b"\n").map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())?;
    }
}

fn start_mcp_bridge(app: AppHandle) {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind(("127.0.0.1", 47_821)) {
            Ok(listener) => listener,
            Err(error) => {
                trace_computer_control(&format!("MCP bridge unavailable: {error}"));
                return;
            }
        };
        trace_computer_control("MCP bridge listening on 127.0.0.1:47821");
        for connection in listener.incoming() {
            match connection {
                Ok(stream) => {
                    if let Err(error) = serve_mcp_bridge_connection(&app, stream) {
                        trace_computer_control(&format!("MCP bridge request failed: {error}"));
                    }
                }
                Err(error) => {
                    trace_computer_control(&format!("MCP bridge connection failed: {error}"))
                }
            }
        }
    });
}

fn looks_like_explicit_computer_task(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 1_200 {
        return false;
    }
    let text = text.to_ascii_lowercase();
    let actions = [
        "打开",
        "启动",
        "运行",
        "点击",
        "双击",
        "输入",
        "填写",
        "粘贴",
        "发送",
        "回复",
        "关闭",
        "切换",
        "滚动",
        "查找",
        "搜索",
        "下载",
        "上传",
        "安装",
        "卸载",
        "打开软件",
        "open",
        "launch",
        "click",
        "double click",
        "type",
        "paste",
        "send",
        "reply",
        "close",
        "switch",
        "scroll",
        "download",
        "upload",
    ];
    let has_action = actions.iter().any(|action| text.contains(action));
    if !has_action {
        return [
            "操作电脑",
            "操控电脑",
            "控制电脑",
            "操作软件",
            "帮我发",
            "替我发",
        ]
        .iter()
        .any(|phrase| text.contains(phrase));
    }

    let delegation = [
        "帮我",
        "替我",
        "请你",
        "你去",
        "麻烦你",
        "给我",
        "直接",
        "现在就",
        "立刻",
        "我要",
        "我想",
    ]
    .iter()
    .any(|phrase| text.contains(phrase));
    let direct_action = actions.iter().any(|action| text.starts_with(action));
    let operation_target =
        text.contains("把") || text.contains("给") || text.contains("在") || text.contains("桌面");
    let asks_for_advice = [
        "怎么",
        "如何",
        "教程",
        "方法",
        "原理",
        "为什么",
        "是什么",
        "介绍",
        "解释",
        "告诉我",
    ]
    .iter()
    .any(|phrase| text.contains(phrase));

    has_action
        && (delegation || direct_action || operation_target)
        && !(asks_for_advice && !delegation)
}

#[tauri::command]
async fn computer_control_intent(text: String) -> Result<bool, String> {
    let config = load_config()?;
    if !config.computer_control_enabled {
        return Ok(false);
    }
    if text.trim().is_empty() || text.chars().count() > 1_200 {
        return Ok(false);
    }
    let explicit_fallback = looks_like_explicit_computer_task(&text);

    let request = ChatRequest {
        provider_id: None,
        messages: Vec::new(),
        attachments: Vec::new(),
        screen_image: None,
        web_search: false,
    };
    let (provider, key, _, _) = resolve_chat_provider(&request)?;
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: "你是 Kero 宿主的电脑操控意图路由器。Kero 确实拥有可调用的 computer_control(task) 方法，可以观察 Windows 屏幕并操作鼠标、键盘、窗口、桌面、任务栏和系统托盘。你的输出会直接决定是否调用此方法。只能回复 CONTROL 或 CHAT，不要解释。\n\n当用户希望 Kero 实际完成电脑上的动作时回复 CONTROL，包括打开或关闭软件、从桌面/任务栏/系统托盘寻找程序、点击、双击、输入、填写、发送、回复、切换页面、滚动、搜索、下载、上传，以及在当前软件、网页或画布中创建或修改内容。即使没有说“操控电脑”，只要语义是在委托 Kero 动手完成，就应回复 CONTROL。\n\n当用户只是在询问知识、教程、方法、能力，或者只需要生成文字而不需要碰电脑界面时回复 CHAT。\n\n示例：‘打开托盘里的 QQ’→CONTROL；‘帮我打开微信给小王发消息’→CONTROL；‘QQ 怎么最小化到托盘’→CHAT；‘给我写一段发给小王的消息’→CHAT。".to_string(),
        },
        ChatMessage { role: "user".to_string(), content: text },
    ];
    let client = shared_http_client();
    let answer = tokio::time::timeout(std::time::Duration::from_millis(3_500), async {
        match provider.kind.as_str() {
            "anthropic" => complete_anthropic(&client, &provider, &key, &messages).await,
            "google" => complete_google(&client, &provider, &key, &messages).await,
            _ => {
                let url = endpoint(
                    &provider.base_url,
                    "https://api.openai.com",
                    "/v1/chat/completions",
                );
                let response = client
                    .post(url)
                    .bearer_auth(&key)
                    .json(&json!({
                        "model": provider.model,
                        "messages": messages,
                        "stream": false,
                        "temperature": 0,
                        "max_tokens": 12
                    }))
                    .send()
                    .await
                    .map_err(|error| format!("无法连接意图识别服务: {error}"))?;
                let status = response.status();
                let body = response
                    .json::<Value>()
                    .await
                    .map_err(|error| format!("无法读取意图识别结果: {error}"))?;
                if !status.is_success() {
                    return Err(api_error(status, &body));
                }
                body.pointer("/choices/0/message/content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| "意图识别模型没有返回结果".to_string())
            }
        }
    })
    .await;
    let model_decision = match answer {
        Ok(Ok(answer)) => {
            let parsed = parse_computer_control_intent(&answer);
            trace_runtime(&format!(
                "computer intent model_decision={parsed:?} explicit_fallback={explicit_fallback}"
            ));
            parsed
        }
        Ok(Err(error)) => {
            trace_runtime(&format!(
                "computer intent model failed; explicit_fallback={explicit_fallback}; error={error}"
            ));
            None
        }
        Err(_) => {
            trace_runtime(&format!(
                "computer intent model timed out; explicit_fallback={explicit_fallback}"
            ));
            None
        }
    };
    Ok(match model_decision {
        Some(true) => true,
        Some(false) => explicit_fallback,
        None => explicit_fallback,
    })
}

fn parse_computer_control_intent(answer: &str) -> Option<bool> {
    answer
        .split(|character: char| !character.is_ascii_alphabetic())
        .filter_map(|token| {
            if token.eq_ignore_ascii_case("CONTROL") {
                Some(true)
            } else if token.eq_ignore_ascii_case("CHAT") {
                Some(false)
            } else {
                None
            }
        })
        .next()
}

#[tauri::command]
async fn computer_plan_task(app: AppHandle, task: String) -> Result<ComputerTaskPlan, String> {
    if task.trim().is_empty() {
        return Err("电脑操控任务不能为空。".to_string());
    }
    let config = load_config()?;
    if !config.computer_control_enabled {
        return Err("请先在设置中开启 AI 操控电脑。".to_string());
    }
    let request = ChatRequest {
        provider_id: None,
        messages: Vec::new(),
        attachments: Vec::new(),
        screen_image: None,
        web_search: false,
    };
    let (provider, key, _, _) = resolve_chat_provider(&request)?;
    if !matches!(provider.kind.as_str(), "openai" | "compatible") {
        return Err("当前电脑操控仅支持 OpenAI 协议及其兼容中转站。".to_string());
    }
    let (screen_image, semantic_snapshot, _) = observe_computer_state(&app, None).await?;
    let semantic_text = semantic_snapshot
        .as_ref()
        .map(semantic_snapshot_text)
        .unwrap_or_else(|| {
            "[WINDOW SEMANTICS] unavailable; use visual grounding only.".to_string()
        });
    let correction_text = remembered_correction_text(semantic_snapshot.as_ref());
    let system = "You are Kero's Windows task planner. Inspect the current screenshot and the optional Windows accessibility control list. Create a short executable plan for the requested desktop task, but do not execute anything and do not describe generic tutorials. Return only one JSON object: {\"summary\":\"...\",\"finalOutcome\":\"visible final result\",\"steps\":[{\"id\":\"step-1\",\"title\":\"...\",\"expectedOutcome\":\"visible result after this step\",\"completionHint\":\"what proves this step is complete\",\"maxAttempts\":10}]}. Use 2 to 8 steps. Each step must be concrete, observable, ordered, and have a testable completion condition. Prefer named semantic controls for normal apps; use visual steps for games, canvas apps, and controls that are not exposed. Sensitive final actions such as sending, deleting, purchasing, publishing, granting access, or entering secrets must be their own final step. The executor will re-check the screen after every step and may adapt the plan.";
    let system = format!("{system} Mandatory desktop-folder rule: when the user asks to create and name a folder on the Windows Desktop, the first and only creation step must say to use Kero's create_folder PowerShell action with the requested folder name. Do not plan a right-click menu, keyboard shortcut, or small rename input field for this task.");
    let user_text = format!(
        "Task: {task}\n\n{semantic_text}\n{correction_text}\nPlan the task from the current screen."
    );
    let url = endpoint(
        &provider.base_url,
        "https://api.openai.com",
        "/v1/chat/completions",
    );
    let payload = json!({
        "model": provider.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": [
                { "type": "text", "text": user_text },
                { "type": "image_url", "image_url": { "url": screen_image, "detail": "high" } }
            ]}
        ],
        "stream": false,
        "max_tokens": 500
    });
    let response = shared_http_client()
        .post(url)
        .bearer_auth(key)
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("无法连接任务规划模型: {error}"))?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| format!("无法读取任务规划响应: {error}"))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    let content = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| "任务规划模型没有返回计划。".to_string())?;
    let json_text = first_complete_json_object(content)
        .ok_or_else(|| "任务规划模型没有返回有效 JSON。".to_string())?;
    let mut plan: ComputerTaskPlan =
        serde_json::from_str(json_text).map_err(|error| format!("无法解析任务计划: {error}"))?;
    plan.summary = plan.summary.trim().to_string();
    plan.steps.retain(|step| !step.title.trim().is_empty());
    plan.steps.truncate(8);
    for (index, step) in plan.steps.iter_mut().enumerate() {
        if step.id.trim().is_empty() {
            step.id = format!("step-{}", index + 1);
        }
        step.id = step.id.trim().replace(char::is_whitespace, "-");
        if step.completion_hint.trim().is_empty() {
            step.completion_hint = step.expected_outcome.clone();
        }
        step.max_attempts = 10;
    }
    plan.final_outcome = plan.final_outcome.trim().to_string();
    if plan.summary.is_empty() {
        plan.summary = "正在按照当前界面分步完成任务。".to_string();
    }
    if plan.steps.is_empty() {
        return Err("任务规划没有给出可执行步骤。".to_string());
    }
    Ok(plan)
}

fn endpoint(base_url: &str, fallback: &str, suffix: &str) -> String {
    let base = if base_url.trim().is_empty() {
        fallback
    } else {
        base_url
    };
    let base = base.trim_end_matches('/');
    if base.ends_with(suffix) {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}{}", suffix.trim_start_matches("/v1"))
    } else {
        format!("{base}{suffix}")
    }
}

fn generated_images_path(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("无法找到图片保存目录: {error}"))?
        .join("generated-images");
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建图片保存目录: {error}"))?;
    Ok(directory)
}

fn available_download_path(directory: &std::path::Path, filename: &str) -> PathBuf {
    let candidate = directory.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let source = std::path::Path::new(filename);
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Kero 图片");
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("png");
    for index in 2..10_000 {
        let candidate = directory.join(format!("{stem} ({index}).{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    directory.join(format!("{stem}-{}.{}", Uuid::new_v4(), extension))
}

#[tauri::command]
fn download_generated_image(app: AppHandle, path: String) -> Result<String, String> {
    let images_directory = generated_images_path(&app)?
        .canonicalize()
        .map_err(|error| format!("无法读取图片保存目录: {error}"))?;
    let source = PathBuf::from(path)
        .canonicalize()
        .map_err(|_| "找不到这张已生成的图片".to_string())?;
    if !source.starts_with(&images_directory) {
        return Err("只能保存由 Kero 生成的图片".to_string());
    }
    let downloads = UserDirs::new()
        .and_then(|directories| directories.download_dir().map(PathBuf::from))
        .ok_or_else(|| "找不到系统下载文件夹".to_string())?;
    fs::create_dir_all(&downloads).map_err(|error| format!("无法创建下载目录: {error}"))?;
    let filename = source
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "图片文件名无效".to_string())?;
    let target = available_download_path(&downloads, filename);
    fs::copy(&source, &target).map_err(|error| format!("无法保存图片到下载文件夹: {error}"))?;
    Ok(target.to_string_lossy().into_owned())
}

fn generated_image_extension(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok("jpg")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Ok("webp")
    } else {
        Err("图片服务返回的内容不是受支持的 PNG、JPEG 或 WebP 图片".to_string())
    }
}

fn image_data_url_parts(data_url: &str) -> Result<(String, Vec<u8>), String> {
    let (metadata, encoded) = data_url
        .split_once(',')
        .ok_or_else(|| "图片数据格式无效".to_string())?;
    let mime_type = metadata
        .strip_prefix("data:")
        .and_then(|value| value.strip_suffix(";base64"))
        .ok_or_else(|| "图片数据格式无效".to_string())?;
    if !matches!(mime_type, "image/png" | "image/jpeg" | "image/webp") {
        return Err("仅支持 PNG、JPEG 和 WebP 图片".to_string());
    }
    let bytes = BASE64
        .decode(encoded)
        .map_err(|_| "图片数据无法解码".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_GENERATED_IMAGE_BYTES {
        return Err("图片大小超出允许范围".to_string());
    }
    Ok((mime_type.to_string(), bytes))
}

fn openai_messages_with_attachments(
    messages: &[ChatMessage],
    attachments: &[ChatAttachment],
) -> Result<Value, String> {
    let last_user = messages.iter().rposition(|message| message.role == "user");
    let attachment_names = attachments
        .iter()
        .map(|attachment| attachment.name.as_str())
        .collect::<Vec<_>>()
        .join("、");
    let mut result = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        if Some(index) == last_user && !attachments.is_empty() {
            let mut content = vec![json!({
                "type": "text",
                "text": format!("{}\n\n已附图片：{}。请结合图片回答。", message.content, attachment_names),
            })];
            for attachment in attachments {
                let (mime_type, _) = image_data_url_parts(&attachment.data_url)?;
                if attachment.mime_type != mime_type {
                    return Err(format!("附件 {} 的图片类型与内容不一致", attachment.name));
                }
                content.push(json!({
                    "type": "image_url",
                    "image_url": { "url": attachment.data_url, "detail": "auto" },
                }));
            }
            result.push(json!({ "role": message.role, "content": content }));
        } else {
            result.push(json!({ "role": message.role, "content": message.content }));
        }
    }
    Ok(Value::Array(result))
}

async fn complete_openai_with_attachments(
    client: &Client,
    provider: &StoredProvider,
    key: &str,
    messages: &[ChatMessage],
    attachments: &[ChatAttachment],
) -> Result<String, String> {
    let url = endpoint(
        &provider.base_url,
        "https://api.openai.com",
        "/v1/chat/completions",
    );
    let payload = json!({
        "model": provider.model,
        "messages": openai_messages_with_attachments(messages, attachments)?,
        "stream": false,
    });
    let response = client
        .post(url)
        .bearer_auth(key)
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("无法连接模型服务: {error}"))?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| format!("无法读取模型响应: {error}"))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    openai_message_text(&body).ok_or_else(|| "模型响应中没有可显示的内容".to_string())
}

fn image_response_preview(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(360)
        .collect()
}

fn parse_image_generation_json(bytes: &[u8]) -> Result<Value, String> {
    let payload = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    if let Ok(json) = serde_json::from_slice::<Value>(payload) {
        return Ok(json);
    }

    // A few OpenAI-compatible gateways return one JSON event instead of a plain
    // JSON document. Accept the final non-empty Server-Sent Event payload.
    for line in String::from_utf8_lossy(payload).lines().rev() {
        let line = line.trim();
        if let Some(event) = line.strip_prefix("data:") {
            let event = event.trim();
            if !event.is_empty() && event != "[DONE]" {
                if let Ok(json) = serde_json::from_str::<Value>(event) {
                    return Ok(json);
                }
            }
        }
    }
    Err("response is not JSON".to_string())
}

async fn read_image_generation_response(
    response: reqwest::Response,
) -> Result<(reqwest::StatusCode, Value), String> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| {
            trace_runtime(&format!(
                "image generation response body read failed: status={status}, content_type={content_type}, error={error:?}"
            ));
            format!("无法读取图片生成响应体 (HTTP {status}, Content-Type {content_type}): {error}")
        })?
        .to_vec();

    if status.is_success() && content_type.to_ascii_lowercase().starts_with("image/") {
        if bytes.is_empty() || bytes.len() > MAX_GENERATED_IMAGE_BYTES {
            return Err("图片生成服务直接返回的图片大小无效".to_string());
        }
        return Ok((
            status,
            json!({ "data": [{ "b64_json": BASE64.encode(bytes) }] }),
        ));
    }
    match parse_image_generation_json(&bytes) {
        Ok(body) => Ok((status, body)),
        Err(_) => {
            let preview = image_response_preview(&bytes);
            Err(format!(
                "图片生成服务返回了无法解析的响应 (HTTP {status}, Content-Type {content_type}){}",
                if preview.is_empty() {
                    "，响应体为空".to_string()
                } else {
                    format!("：{preview}")
                }
            ))
        }
    }
}

fn image_data_items(body: &Value) -> Option<&Vec<Value>> {
    body.get("data")
        .and_then(Value::as_array)
        .or_else(|| body.get("images").and_then(Value::as_array))
        .or_else(|| body.pointer("/output/results").and_then(Value::as_array))
        .or_else(|| body.pointer("/result/images").and_then(Value::as_array))
}

async fn image_bytes_from_response(body: &Value) -> Result<Vec<u8>, String> {
    if let Some(encoded) = body.pointer("/data/0/b64_json").and_then(Value::as_str) {
        let bytes = BASE64
            .decode(encoded)
            .map_err(|_| "图片服务返回了无效的图片数据".to_string())?;
        if bytes.is_empty() || bytes.len() > MAX_GENERATED_IMAGE_BYTES {
            return Err("生成的图片大小超出允许范围".to_string());
        }
        return Ok(bytes);
    }

    let image_url = body
        .pointer("/data/0/url")
        .and_then(Value::as_str)
        .ok_or_else(|| "图片服务响应中没有图片数据".to_string())?;
    let response = image_generation_http_client()
        .get(image_url)
        .send()
        .await
        .map_err(|error| format!("无法下载生成的图片: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("无法下载生成的图片: HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("无法读取生成的图片: {error}"))?
        .to_vec();
    if bytes.is_empty() || bytes.len() > MAX_GENERATED_IMAGE_BYTES {
        return Err("生成的图片大小超出允许范围".to_string());
    }
    Ok(bytes)
}

#[tauri::command]
async fn generate_image(
    app: AppHandle,
    request: ImageGenerationRequest,
) -> Result<Vec<GeneratedImage>, String> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err("请输入图片描述".to_string());
    }
    if prompt.chars().count() > 6000 {
        return Err("图片描述不能超过 6000 个字符".to_string());
    }
    let size = match request.aspect_ratio.as_str() {
        "1:1" => "1024x1024",
        "16:9" => "1792x1024",
        "9:16" => "1024x1792",
        _ => return Err("不支持的图片比例".to_string()),
    };
    if !(1..=4).contains(&request.count) {
        return Err("一次最多生成 4 张图片".to_string());
    }
    if request.reference_images.len() > 4 {
        return Err("最多使用 4 张参考图片".to_string());
    }

    let config = load_config()?.image_generation;
    if config.model.trim().is_empty() {
        return Err("请先在设置中填写图片模型名称".to_string());
    }
    let key = read_secret(IMAGE_GENERATION_SECRET_ID)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "图片生成 API Key 尚未保存".to_string())?;
    let payload = json!({
        "model": config.model,
        "prompt": prompt,
        "size": size,
        "n": request.count,
        "response_format": "b64_json"
    });
    let response = if request.reference_images.is_empty() {
        let url = endpoint(
            &config.base_url,
            "https://api.openai.com",
            "/v1/images/generations",
        );
        image_generation_http_client()
            .post(url)
            .bearer_auth(&key)
            .json(&payload)
            .send()
            .await
    } else {
        let url = endpoint(
            &config.base_url,
            "https://api.openai.com",
            "/v1/images/edits",
        );
        let mut form = reqwest::multipart::Form::new()
            .text("model", config.model.clone())
            .text("prompt", prompt.to_string())
            .text("size", size.to_string())
            .text("n", request.count.to_string())
            .text("response_format", "b64_json");
        for (index, data_url) in request.reference_images.iter().enumerate() {
            let (mime_type, bytes) = image_data_url_parts(data_url)?;
            let extension = match mime_type.as_str() {
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                _ => "png",
            };
            let part = reqwest::multipart::Part::bytes(bytes)
                .file_name(format!("reference-{}.{}", index + 1, extension))
                .mime_str(&mime_type)
                .map_err(|error| format!("无法准备参考图片: {error}"))?;
            form = form.part("image[]", part);
        }
        image_generation_http_client()
            .post(url)
            .bearer_auth(&key)
            .multipart(form)
            .send()
            .await
    }
    .map_err(|error| format!("无法连接图片生成服务: {error}"))?;
    let (status, body) = read_image_generation_response(response).await?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }

    let data = image_data_items(&body)
        .filter(|items| !items.is_empty())
        .ok_or_else(|| "图片服务响应中没有可用图片数据".to_string())?;
    let directory = generated_images_path(&app)?;
    let mut images = Vec::with_capacity(data.len());
    for item in data {
        let item = if let Some(url) = item.as_str() {
            json!({ "url": url })
        } else {
            item.clone()
        };
        let bytes = image_bytes_from_response(&json!({ "data": [item] })).await?;
        let extension = generated_image_extension(&bytes)?;
        let path = directory.join(format!("{}.{}", Uuid::new_v4(), extension));
        fs::write(&path, bytes).map_err(|error| format!("无法保存生成的图片: {error}"))?;
        images.push(GeneratedImage {
            path: path.to_string_lossy().into_owned(),
            prompt: prompt.to_string(),
        });
    }
    Ok(images)
}

fn api_error(status: reqwest::StatusCode, payload: &Value) -> String {
    let detail = payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .unwrap_or("服务返回了未知错误");
    format!("请求失败 ({status}): {detail}")
}

async fn complete_openai(
    client: &Client,
    provider: &StoredProvider,
    key: &str,
    messages: &[ChatMessage],
) -> Result<String, String> {
    let url = endpoint(
        &provider.base_url,
        "https://api.openai.com",
        "/v1/chat/completions",
    );
    let payload = json!({
        "model": provider.model,
        "messages": messages,
        "stream": false
    });
    let response = client
        .post(url)
        .bearer_auth(key)
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("无法连接模型服务: {error}"))?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| format!("无法读取模型响应: {error}"))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    body.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "模型响应中没有可显示的内容".to_string())
}

fn responses_output_text(body: &Value) -> Option<String> {
    let content = body
        .get("output")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    (!content.is_empty()).then(|| content.join(""))
}

fn responses_input(messages: &[ChatMessage], screen_image: Option<&str>) -> Value {
    let last_user = messages.iter().rposition(|message| message.role == "user");
    Value::Array(
        messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let mut content = vec![json!({ "type": "input_text", "text": message.content })];
                if Some(index) == last_user && screen_image.is_some() {
                    content.push(json!({ "type": "input_image", "image_url": screen_image.unwrap(), "detail": "low" }));
                }
                json!({ "role": message.role, "content": content })
            })
            .collect(),
    )
}

fn with_web_search_instruction(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut result = Vec::with_capacity(messages.len() + 1);
    result.push(ChatMessage {
        role: "system".to_string(),
        content: "联网搜索已经开启。回答前必须使用联网搜索工具；对于用户提供的网址或网页相关问题，应检索并阅读该网页的相关内容后再回答。引用网页内容时请给出可点击的来源链接。".to_string(),
    });
    result.extend_from_slice(messages);
    result
}

async fn send_responses_request(
    client: &Client,
    url: &str,
    key: &str,
    payload: &Value,
) -> Result<reqwest::Response, String> {
    for attempt in 0..2 {
        match client.post(url).bearer_auth(key).json(payload).send().await {
            Ok(response) => {
                let retryable = matches!(
                    response.status(),
                    reqwest::StatusCode::BAD_GATEWAY
                        | reqwest::StatusCode::SERVICE_UNAVAILABLE
                        | reqwest::StatusCode::GATEWAY_TIMEOUT
                );
                if retryable && attempt == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                    continue;
                }
                return Ok(response);
            }
            Err(error) if attempt == 0 => {
                let _ = error;
                tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            }
            Err(error) => return Err(format!("Unable to connect to model service: {error}")),
        }
    }
    unreachable!()
}

async fn complete_openai_web_search(
    client: &Client,
    provider: &StoredProvider,
    key: &str,
    messages: &[ChatMessage],
) -> Result<String, String> {
    ensure_web_search_supported(provider)?;
    let url = endpoint(
        &provider.base_url,
        "https://api.openai.com",
        "/v1/responses",
    );
    let messages = with_web_search_instruction(messages);
    let payload = json!({
        "model": provider.model,
        "input": responses_input(&messages, None),
        "tools": [{ "type": "web_search" }],
        "tool_choice": "required",
        "max_output_tokens": 2048
    });
    let response = send_responses_request(client, &url, key, &payload).await?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| format!("Unable to read model response: {error}"))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    responses_output_text(&body)
        .ok_or_else(|| "The model response contains no displayable content.".to_string())
}

async fn complete_anthropic(
    client: &Client,
    provider: &StoredProvider,
    key: &str,
    messages: &[ChatMessage],
) -> Result<String, String> {
    let url = endpoint(
        &provider.base_url,
        "https://api.anthropic.com",
        "/v1/messages",
    );
    let system = messages
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let conversation = messages
        .iter()
        .filter(|message| message.role != "system")
        .map(|message| json!({ "role": if message.role == "assistant" { "assistant" } else { "user" }, "content": message.content }))
        .collect::<Vec<_>>();
    let payload = json!({
        "model": provider.model,
        "max_tokens": 2048,
        "system": system,
        "messages": conversation
    });
    let response = client
        .post(url)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("无法连接模型服务: {error}"))?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| format!("无法读取模型响应: {error}"))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    body.pointer("/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "模型响应中没有可显示的内容".to_string())
}

async fn complete_google(
    client: &Client,
    provider: &StoredProvider,
    key: &str,
    messages: &[ChatMessage],
) -> Result<String, String> {
    let base = if provider.base_url.trim().is_empty() {
        "https://generativelanguage.googleapis.com"
    } else {
        provider.base_url.trim_end_matches('/')
    };
    let url = format!(
        "{base}/v1beta/models/{}:generateContent?key={key}",
        provider.model
    );
    let system = messages
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let contents = messages
        .iter()
        .filter(|message| message.role != "system")
        .map(|message| json!({ "role": if message.role == "assistant" { "model" } else { "user" }, "parts": [{ "text": message.content }] }))
        .collect::<Vec<_>>();
    let payload = json!({
        "systemInstruction": { "parts": [{ "text": system }] },
        "contents": contents,
        "generationConfig": { "temperature": 0.7 }
    });
    let response = client
        .post(url)
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("无法连接模型服务: {error}"))?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| format!("无法读取模型响应: {error}"))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    body.pointer("/candidates/0/content/parts/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "模型响应中没有可显示的内容".to_string())
}

fn resolve_chat_provider(
    request: &ChatRequest,
) -> Result<(StoredProvider, String, String, bool), String> {
    let config = load_config()?;
    let provider_id = request
        .provider_id
        .clone()
        .or(config.default_provider.clone())
        .ok_or_else(|| "请先在设置中添加模型服务".to_string())?;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .cloned()
        .ok_or_else(|| "找不到选定的模型服务".to_string())?;
    let key = secret_entry(&provider.id)?
        .get_password()
        .map_err(|_| "这个提供商还没有保存 API Key".to_string())?;
    Ok((provider, key, config.system_prompt, config.context_enabled))
}

fn is_official_openai_endpoint(provider: &StoredProvider) -> bool {
    if provider.kind != "openai" {
        return false;
    }
    let base_url = provider.base_url.trim().trim_end_matches('/');
    base_url.is_empty()
        || base_url == "https://api.openai.com"
        || base_url == "https://api.openai.com/v1"
}

fn is_openai_web_search_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    ["gpt-5", "gpt-4.5", "gpt-4.1", "gpt-4o", "o3", "o4"]
        .iter()
        .any(|prefix| model.starts_with(prefix))
}

fn web_search_availability_for(
    provider: &StoredProvider,
    allow_proxy: bool,
) -> WebSearchAvailability {
    if provider.kind != "openai" {
        return WebSearchAvailability {
            supported: false,
            reason: "当前服务商不支持 OpenAI 原生联网搜索。".to_string(),
        };
    }
    if !is_official_openai_endpoint(provider) && !allow_proxy {
        return WebSearchAvailability {
            supported: false,
            reason: "当前是中转站。请先在高级选项中允许并验证中转站的原生联网搜索能力。"
                .to_string(),
        };
    }
    if !is_openai_web_search_model(&provider.model) {
        return WebSearchAvailability {
            supported: false,
            reason: format!("模型 {} 不支持模型原生联网搜索。请改用 GPT-4.1、GPT-4o、GPT-4.5、GPT-5 或 o3/o4 系列。", provider.model),
        };
    }
    WebSearchAvailability {
        supported: true,
        reason: String::new(),
    }
}

fn ensure_web_search_supported(provider: &StoredProvider) -> Result<(), String> {
    let availability =
        web_search_availability_for(provider, load_config()?.web_search_proxy_enabled);
    availability
        .supported
        .then_some(())
        .ok_or(availability.reason)
}

#[tauri::command]
fn get_web_search_enabled() -> Result<bool, String> {
    Ok(load_config()?.web_search_enabled)
}

#[tauri::command]
fn web_search_availability(provider_id: Option<String>) -> Result<WebSearchAvailability, String> {
    let config = load_config()?;
    let provider_id = provider_id
        .or(config.default_provider.clone())
        .ok_or_else(|| "请先在设置中配置并设为默认模型服务。".to_string())?;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "找不到当前选定的模型服务。".to_string())?;
    Ok(web_search_availability_for(
        provider,
        config.web_search_proxy_enabled,
    ))
}

#[tauri::command]
fn set_web_search_enabled(web_search_enabled: bool) -> Result<(), String> {
    let mut config = load_config()?;
    if web_search_enabled {
        let provider_id = config
            .default_provider
            .clone()
            .ok_or_else(|| "请先在设置中配置并设为默认模型服务。".to_string())?;
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .ok_or_else(|| "找不到当前默认模型服务。".to_string())?;
        let availability = web_search_availability_for(provider, config.web_search_proxy_enabled);
        availability
            .supported
            .then_some(())
            .ok_or(availability.reason)?;
    }
    config.web_search_enabled = web_search_enabled;
    save_config(&config)
}

#[tauri::command]
fn get_web_search_proxy_enabled() -> Result<bool, String> {
    Ok(load_config()?.web_search_proxy_enabled)
}

async fn verify_proxy_web_search(provider: &StoredProvider, key: &str) -> Result<(), String> {
    if provider.kind != "openai" {
        return Err("只有 OpenAI 协议的中转站可使用此高级选项。".to_string());
    }
    let url = endpoint(
        &provider.base_url,
        "https://api.openai.com",
        "/v1/responses",
    );
    let response = shared_http_client()
        .post(url)
        .bearer_auth(key)
        .json(&json!({
            "model": provider.model,
            "input": "Use web search to find the official OpenAI API homepage. Reply only with OK.",
            "tools": [{ "type": "web_search" }],
            "tool_choice": "required",
            "max_output_tokens": 16
        }))
        .send()
        .await
        .map_err(|error| format!("无法连接中转站进行联网能力验证: {error}"))?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| format!("无法读取中转站验证响应: {error}"))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    let used_web_search = body
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|output| {
            output
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
        });
    used_web_search
        .then_some(())
        .ok_or_else(|| "中转站返回成功，但没有实际调用模型原生联网工具。".to_string())
}

#[tauri::command]
async fn set_web_search_proxy_enabled(web_search_proxy_enabled: bool) -> Result<(), String> {
    let config = load_config()?;
    if web_search_proxy_enabled {
        let provider_id = config
            .default_provider
            .clone()
            .ok_or_else(|| "请先在设置中配置并设为默认模型服务。".to_string())?;
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .cloned()
            .ok_or_else(|| "找不到当前默认模型服务。".to_string())?;
        if is_official_openai_endpoint(&provider) {
            return Err("当前使用的是 OpenAI 官方接口，无需开启中转站高级选项。".to_string());
        }
        let key = secret_entry(&provider.id)?.get_password()?;
        verify_proxy_web_search(&provider, &key).await?;
    }

    let mut config = config;
    config.web_search_proxy_enabled = web_search_proxy_enabled;
    if !web_search_proxy_enabled {
        config.web_search_enabled = false;
    }
    save_config(&config)
}

fn with_system_prompt(
    messages: &[ChatMessage],
    system_prompt: &str,
    context_enabled: bool,
) -> Vec<ChatMessage> {
    let mut result = Vec::with_capacity(messages.len() + 1);
    const HOST_CAPABILITY_PROMPT: &str = "你运行在 Kero Windows 桌面助手中。Kero 宿主具备真实的电脑操控能力，并提供 computer_control(task) 调用，可观察屏幕以及操作鼠标、键盘、窗口、桌面、任务栏和系统托盘。操作请求通常会在进入聊天前自动路由；如果仍有要求你实际操作当前电脑的请求到达这里，不要声称无法操作，也不要给用户操作教程。此时只输出 [[KERO_COMPUTER_CONTROL]]，不要附加任何其他文字；宿主会拦截该标记，并使用用户的原始请求调用 computer_control(task)。仅询问知识、教程、方法或能力时正常回答，不要输出该标记。";
    let computer_control_enabled = load_config()
        .map(|config| config.computer_control_enabled)
        .unwrap_or(false);
    let system = match (system_prompt.trim(), computer_control_enabled) {
        ("", true) => HOST_CAPABILITY_PROMPT.to_string(),
        ("", false) => String::new(),
        (custom, true) => format!("{custom}\n\n{HOST_CAPABILITY_PROMPT}"),
        (custom, false) => custom.to_string(),
    };
    if !system.is_empty() {
        result.push(ChatMessage {
            role: "system".to_string(),
            content: system,
        });
    }
    let conversation = messages
        .iter()
        .filter(|message| message.role != "system")
        .rev()
        .take(if context_enabled { 12 } else { 1 })
        .cloned()
        .collect::<Vec<_>>();
    result.extend(conversation.into_iter().rev());
    result
}

fn emit_stream_event(
    app: &AppHandle,
    request_id: &str,
    delta: Option<String>,
    done: bool,
    error: Option<String>,
) {
    let _ = app.emit_to(
        "main",
        "kero:stream",
        StreamEvent {
            request_id: request_id.to_string(),
            delta,
            done,
            error,
        },
    );
}

fn take_sse_event(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let delimiter = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| (index, 4))
        })?;
    let event = buffer[..delimiter.0].to_vec();
    buffer.drain(..delimiter.0 + delimiter.1);
    Some(event)
}

fn sse_data(event: &[u8]) -> Option<String> {
    let event = String::from_utf8_lossy(event);
    let data = event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>();
    (!data.is_empty()).then(|| data.join("\n"))
}

fn openai_stream_delta(payload: &Value) -> Option<String> {
    payload
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn responses_stream_delta(payload: &Value) -> Option<String> {
    (payload.get("type").and_then(Value::as_str) == Some("response.output_text.delta"))
        .then(|| {
            payload
                .get("delta")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten()
}

fn anthropic_stream_delta(payload: &Value) -> Option<String> {
    (payload.get("type").and_then(Value::as_str) == Some("content_block_delta"))
        .then(|| {
            payload
                .pointer("/delta/text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten()
}

fn google_stream_delta(payload: &Value) -> Option<String> {
    payload
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
}

async fn read_sse_stream(
    mut response: reqwest::Response,
    app: &AppHandle,
    request_id: &str,
    extract_delta: fn(&Value) -> Option<String>,
) -> Result<(), String> {
    let status = response.status();
    if !status.is_success() {
        let body = response.json::<Value>().await.unwrap_or_else(|_| json!({}));
        return Err(api_error(status, &body));
    }

    let mut buffer = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("读取流式响应失败: {error}"))?
    {
        buffer.extend_from_slice(&chunk);
        while let Some(event) = take_sse_event(&mut buffer) {
            let Some(data) = sse_data(&event) else {
                continue;
            };
            if data == "[DONE]" {
                return Ok(());
            }
            let payload: Value = serde_json::from_str(&data)
                .map_err(|error| format!("模型返回了无效的流式数据: {error}"))?;
            if let Some(detail) = payload.pointer("/error/message").and_then(Value::as_str) {
                return Err(detail.to_string());
            }
            if let Some(delta) = extract_delta(&payload).filter(|text| !text.is_empty()) {
                emit_stream_event(app, request_id, Some(delta), false, None);
            }
        }
    }
    Ok(())
}

async fn stream_openai(
    client: &Client,
    provider: &StoredProvider,
    key: &str,
    messages: &[ChatMessage],
    app: &AppHandle,
    request_id: &str,
    screen_image: Option<&str>,
) -> Result<(), String> {
    let url = endpoint(
        &provider.base_url,
        "https://api.openai.com",
        "/v1/chat/completions",
    );
    let last_user = messages.iter().rposition(|message| message.role == "user");
    let request_messages = messages.iter().enumerate().map(|(index, message)| {
        if Some(index) == last_user && screen_image.is_some() {
            json!({ "role": message.role, "content": [
                { "type": "text", "text": message.content },
                { "type": "image_url", "image_url": { "url": screen_image.unwrap(), "detail": "low" } }
            ]})
        } else {
            json!({ "role": message.role, "content": message.content })
        }
    }).collect::<Vec<_>>();
    let compact = request_id.starts_with("dictation-transform:");
    let response = client
        .post(url)
        .bearer_auth(key)
        .json(&json!({
            "model": provider.model,
            "messages": request_messages,
            "stream": true,
            "temperature": if compact { 0.1 } else { 0.7 },
            "max_tokens": if compact { 512 } else if screen_image.is_some() { 512 } else { 2048 }
        }))
        .send()
        .await
        .map_err(|error| format!("无法连接模型服务: {error}"))?;
    read_sse_stream(response, app, request_id, openai_stream_delta).await
}

async fn stream_openai_web_search(
    client: &Client,
    provider: &StoredProvider,
    key: &str,
    messages: &[ChatMessage],
    app: &AppHandle,
    request_id: &str,
    screen_image: Option<&str>,
) -> Result<(), String> {
    ensure_web_search_supported(provider)?;
    let url = endpoint(
        &provider.base_url,
        "https://api.openai.com",
        "/v1/responses",
    );
    let messages = with_web_search_instruction(messages);
    let payload = json!({
        "model": provider.model,
        "input": responses_input(&messages, screen_image),
        "tools": [{ "type": "web_search" }],
        "tool_choice": "required",
        "stream": true,
        "max_output_tokens": if screen_image.is_some() { 512 } else { 2048 }
    });
    let response = send_responses_request(client, &url, key, &payload).await?;
    read_sse_stream(response, app, request_id, responses_stream_delta).await
}

async fn stream_anthropic(
    client: &Client,
    provider: &StoredProvider,
    key: &str,
    messages: &[ChatMessage],
    app: &AppHandle,
    request_id: &str,
    screen_image: Option<&str>,
) -> Result<(), String> {
    let url = endpoint(
        &provider.base_url,
        "https://api.anthropic.com",
        "/v1/messages",
    );
    let system = messages
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let last_user = messages.iter().rposition(|message| message.role == "user");
    let conversation = messages.iter().enumerate()
        .filter(|(_, message)| message.role != "system")
        .map(|(index, message)| {
            let role = if message.role == "assistant" { "assistant" } else { "user" };
            if Some(index) == last_user && screen_image.is_some() {
                let (_, encoded) = screen_image.unwrap().split_once(',').ok_or_else(|| "Invalid screen image".to_string())?;
                Ok(json!({ "role": role, "content": [
                    { "type": "text", "text": message.content },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": encoded } }
                ]}))
            } else { Ok(json!({ "role": role, "content": message.content })) }
        })
        .collect::<Result<Vec<_>, String>>()?;
    let compact = request_id.starts_with("dictation-transform:");
    let response = client
        .post(url)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": provider.model,
            "max_tokens": if compact { 512 } else if screen_image.is_some() { 512 } else { 2048 },
            "temperature": if compact { 0.1 } else { 0.7 },
            "system": system,
            "messages": conversation,
            "stream": true
        }))
        .send()
        .await
        .map_err(|error| format!("无法连接模型服务: {error}"))?;
    read_sse_stream(response, app, request_id, anthropic_stream_delta).await
}

async fn stream_google(
    client: &Client,
    provider: &StoredProvider,
    key: &str,
    messages: &[ChatMessage],
    app: &AppHandle,
    request_id: &str,
    screen_image: Option<&str>,
) -> Result<(), String> {
    let base = if provider.base_url.trim().is_empty() {
        "https://generativelanguage.googleapis.com"
    } else {
        provider.base_url.trim_end_matches('/')
    };
    let url = format!(
        "{base}/v1beta/models/{}:streamGenerateContent?alt=sse&key={key}",
        provider.model
    );
    let system = messages
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let last_user = messages.iter().rposition(|message| message.role == "user");
    let contents = messages.iter().enumerate()
        .filter(|(_, message)| message.role != "system")
        .map(|(index, message)| {
            let mut parts = vec![json!({ "text": message.content })];
            if Some(index) == last_user && screen_image.is_some() {
                if let Some((_, encoded)) = screen_image.unwrap().split_once(',') {
                    parts.push(json!({ "inlineData": { "mimeType": "image/jpeg", "data": encoded } }));
                }
            }
            json!({ "role": if message.role == "assistant" { "model" } else { "user" }, "parts": parts })
        })
        .collect::<Vec<_>>();
    let compact = request_id.starts_with("dictation-transform:");
    let response = client
        .post(url)
        .json(&json!({
            "systemInstruction": { "parts": [{ "text": system }] },
            "contents": contents,
            "generationConfig": { "temperature": if compact { 0.1 } else { 0.7 }, "maxOutputTokens": if compact { 512 } else if screen_image.is_some() { 512 } else { 2048 } }
        }))
        .send()
        .await
        .map_err(|error| format!("无法连接模型服务: {error}"))?;
    read_sse_stream(response, app, request_id, google_stream_delta).await
}

#[tauri::command]
async fn chat_completion(request: ChatRequest) -> Result<String, String> {
    let (provider, key, system_prompt, context_enabled) = resolve_chat_provider(&request)?;
    let client = shared_http_client();
    let messages = with_system_prompt(&request.messages, &system_prompt, context_enabled);

    if !request.attachments.is_empty() {
        if request.web_search {
            return Err("附加图片时暂不能同时启用联网搜索".to_string());
        }
        if !matches!(provider.kind.as_str(), "openai" | "compatible") {
            return Err("当前图片附件仅支持 OpenAI 协议及其兼容接口".to_string());
        }
        return complete_openai_with_attachments(
            &client,
            &provider,
            &key,
            &messages,
            &request.attachments,
        )
        .await;
    }

    if request.web_search {
        return complete_openai_web_search(&client, &provider, &key, &messages).await;
    }

    match provider.kind.as_str() {
        "anthropic" => complete_anthropic(&client, &provider, &key, &messages).await,
        "google" => complete_google(&client, &provider, &key, &messages).await,
        _ => complete_openai(&client, &provider, &key, &messages).await,
    }
}

#[tauri::command]
async fn optimize_image_prompt(request: ImagePromptOptimizationRequest) -> Result<String, String> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err("请先输入图片描述".to_string());
    }
    if prompt.chars().count() > 6000 {
        return Err("图片描述不能超过 6000 个字符".to_string());
    }
    let chat_request = ChatRequest {
        provider_id: request.provider_id,
        messages: Vec::new(),
        attachments: Vec::new(),
        screen_image: None,
        web_search: false,
    };
    let (provider, key, _, _) = resolve_chat_provider(&chat_request)?;
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: "你只负责把用户的图片描述改写为一条可直接提交给图片生成模型的单一提示词。必须保留原意和用户明确给出的主体、风格、文字、比例、限制；仅补充有助于画面生成的构图、镜头、光线、材质和细节。不要与用户对话，不要回答问题，不要解释，不要使用 Markdown、标题、项目符号、引号、前缀或后缀。直接输出优化后的提示词正文。".to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
        },
    ];
    if request.reference_images.len() > 4 {
        return Err("最多使用 4 张图片素材优化提示词".to_string());
    }
    let mut attachments = Vec::with_capacity(request.reference_images.len());
    for (index, data_url) in request.reference_images.iter().enumerate() {
        let (mime_type, _) = image_data_url_parts(data_url)?;
        attachments.push(ChatAttachment {
            name: format!("图片素材 {}", index + 1),
            mime_type,
            data_url: data_url.to_string(),
        });
    }
    let client = shared_http_client();
    let result = if attachments.is_empty() {
        match provider.kind.as_str() {
            "anthropic" => complete_anthropic(&client, &provider, &key, &messages).await,
            "google" => complete_google(&client, &provider, &key, &messages).await,
            _ => complete_openai(&client, &provider, &key, &messages).await,
        }
    } else {
        if !matches!(provider.kind.as_str(), "openai" | "compatible") {
            return Err(
                "带图片素材优化提示词需要使用支持视觉输入的 OpenAI 协议模型或兼容接口".to_string(),
            );
        }
        complete_openai_with_attachments(&client, &provider, &key, &messages, &attachments).await
    }?;
    Ok(result.trim().trim_matches('`').trim().to_string())
}

fn openai_message_text(body: &Value) -> Option<String> {
    let content = body.pointer("/choices/0/message/content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let parts = content
        .as_array()?
        .iter()
        .filter_map(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .or_else(|| part.pointer("/text/value").and_then(Value::as_str))
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(""))
}

fn screen_translation_prompt(settings: &TranslationSettings) -> String {
    let source = translation_language_name(&settings.source_language).unwrap_or("自动检测");
    let target = translation_language_name(&settings.target_language).unwrap_or("简体中文");
    format!(
        "分析这张完整屏幕截图，把所有清晰可见的{source}界面文字翻译成{target}。只返回一个 JSON 对象，不要 Markdown、解释或代码围栏。格式必须是 {{\"items\":[{{\"text\":\"译文\",\"x\":0.0,\"y\":0.0,\"width\":0.2,\"height\":0.04,\"fontSize\":14}}]}}。x/y 是原文矩形左上角相对屏幕的坐标，width/height 是相对屏幕的尺寸，全部使用 0 到 1。fontSize 使用 11 到 38 的 CSS 像素。保留按钮、菜单和专业术语的准确含义；不要翻译已经是目标语言、纯数字、网址、代码、文件路径、无意义符号或看不清的内容。相邻且属于同一句的文本可以合并，但不得覆盖无关控件。最多返回 90 项。"
    )
}

async fn send_openai_translation_json(
    provider: &StoredProvider,
    key: &str,
    mut payload: Value,
) -> Result<Value, String> {
    let url = endpoint(
        &provider.base_url,
        "https://api.openai.com",
        "/v1/chat/completions",
    );
    for attempt in 0..2 {
        let response = shared_http_client()
            .post(&url)
            .bearer_auth(key)
            .json(&payload)
            .send()
            .await
            .map_err(|error| format!("无法连接屏幕翻译模型: {error}"))?;
        let status = response.status();
        let body = response
            .json::<Value>()
            .await
            .map_err(|error| format!("无法读取屏幕翻译响应: {error}"))?;
        if status.is_success() {
            return Ok(body);
        }
        let detail = api_error(status, &body).to_ascii_lowercase();
        let reasoning_rejected = detail.contains("reasoning_effort")
            || detail.contains("reasoning effort")
            || (detail.contains("unknown") && detail.contains("reasoning"))
            || (detail.contains("unsupported") && detail.contains("reasoning"));
        if attempt == 0 && reasoning_rejected {
            if let Some(object) = payload.as_object_mut() {
                object.remove("reasoning_effort");
            }
            continue;
        }
        return Err(api_error(status, &body));
    }
    unreachable!()
}

async fn send_google_translation_json(
    provider: &StoredProvider,
    key: &str,
    mut payload: Value,
) -> Result<Value, String> {
    let base = if provider.base_url.trim().is_empty() {
        "https://generativelanguage.googleapis.com"
    } else {
        provider.base_url.trim_end_matches('/')
    };
    let url = format!(
        "{base}/v1beta/models/{}:generateContent?key={key}",
        provider.model
    );
    for attempt in 0..2 {
        let response = shared_http_client()
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|error| format!("无法连接屏幕翻译模型: {error}"))?;
        let status = response.status();
        let body = response
            .json::<Value>()
            .await
            .map_err(|error| format!("无法读取屏幕翻译响应: {error}"))?;
        if status.is_success() {
            return Ok(body);
        }
        let detail = api_error(status, &body).to_ascii_lowercase();
        let thinking_rejected = detail.contains("thinkingconfig")
            || detail.contains("thinking_budget")
            || detail.contains("thinkingbudget");
        if attempt == 0 && thinking_rejected {
            if let Some(generation) = payload
                .get_mut("generationConfig")
                .and_then(Value::as_object_mut)
            {
                generation.remove("thinkingConfig");
            }
            continue;
        }
        return Err(api_error(status, &body));
    }
    unreachable!()
}

async fn request_screen_text_translation(
    provider: &StoredProvider,
    key: &str,
    settings: &TranslationSettings,
    detected: &[DetectedScreenText],
) -> Result<Vec<ScreenTranslationItem>, String> {
    let source = translation_language_name(&settings.source_language).unwrap_or("自动检测");
    let target = translation_language_name(&settings.target_language).unwrap_or("简体中文");
    let compact_input = detected
        .iter()
        .take(110)
        .map(|item| json!({ "id": item.id, "text": item.text }))
        .collect::<Vec<_>>();
    let prompt = format!(
        "立即把下面界面文字中的{source}翻译成{target}，不要分析或思考。已经是目标语言、纯数字、网址、代码、路径和无需翻译的项目不要返回。只输出 JSON：{{\"items\":[{{\"id\":\"t1\",\"text\":\"译文\"}}]}}。保持按钮和菜单用语简短准确，ID 必须原样保留。输入：{}",
        serde_json::to_string(&compact_input)
            .map_err(|error| format!("无法准备快速翻译文本: {error}"))?
    );
    let system = "你是低延迟界面翻译器。直接翻译，不进行推理，不输出解释或 Markdown。";
    let content = match provider.kind.as_str() {
        "anthropic" => {
            complete_anthropic(
                &shared_http_client(),
                provider,
                key,
                &[
                    ChatMessage {
                        role: "system".to_string(),
                        content: system.to_string(),
                    },
                    ChatMessage {
                        role: "user".to_string(),
                        content: prompt,
                    },
                ],
            )
            .await?
        }
        "google" => {
            let body = send_google_translation_json(
                provider,
                key,
                json!({
                    "systemInstruction": { "parts": [{ "text": system }] },
                    "contents": [{ "role": "user", "parts": [{ "text": prompt }] }],
                    "generationConfig": {
                        "temperature": 0,
                        "maxOutputTokens": 2048,
                        "thinkingConfig": { "thinkingBudget": 0 }
                    }
                }),
            )
            .await?;
            body.pointer("/candidates/0/content/parts/0/text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| "屏幕翻译模型没有返回可显示内容。".to_string())?
        }
        _ => {
            let body = send_openai_translation_json(
                provider,
                key,
                json!({
                    "model": provider.model,
                    "messages": [
                        { "role": "system", "content": system },
                        { "role": "user", "content": prompt }
                    ],
                    "stream": false,
                    "temperature": 0,
                    "reasoning_effort": "none",
                    "max_tokens": 2048
                }),
            )
            .await?;
            openai_message_text(&body)
                .ok_or_else(|| "屏幕翻译模型没有返回可显示内容。".to_string())?
        }
    };
    let json_text = first_complete_json_object(&content)
        .ok_or_else(|| "快速屏幕翻译没有返回有效 JSON。".to_string())?;
    let translated: ScreenTextTranslationResponse = serde_json::from_str(json_text)
        .map_err(|error| format!("无法解析快速屏幕翻译结果: {error}"))?;
    let by_id = detected
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    Ok(translated
        .items
        .into_iter()
        .take(110)
        .filter_map(|item| {
            let original = by_id.get(item.id.trim())?;
            let text = item.text.trim().chars().take(300).collect::<String>();
            (!text.is_empty()).then(|| ScreenTranslationItem {
                text,
                x: original.x,
                y: original.y,
                width: original.width,
                height: original.height,
                font_size: Some(original.font_size),
            })
        })
        .collect())
}

async fn request_screen_translation(
    provider: &StoredProvider,
    key: &str,
    settings: &TranslationSettings,
    screen_image: &str,
) -> Result<Vec<ScreenTranslationItem>, String> {
    let prompt = screen_translation_prompt(settings);
    let client = shared_http_client();
    let content = match provider.kind.as_str() {
        "anthropic" => {
            let (_, encoded) = screen_image
                .split_once(',')
                .ok_or_else(|| "屏幕图像数据无效。".to_string())?;
            let response = client
                    .post(endpoint(
                        &provider.base_url,
                        "https://api.anthropic.com",
                        "/v1/messages",
                    ))
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&json!({
                        "model": provider.model,
                        "max_tokens": 2400,
                        "system": "你是低延迟屏幕翻译器。直接翻译，不进行分析或扩展思考，只输出请求的 JSON。",
                        "messages": [{ "role": "user", "content": [
                            { "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": encoded } },
                            { "type": "text", "text": prompt }
                        ] }]
                    }))
                    .send()
                    .await
                    .map_err(|error| format!("无法连接屏幕翻译模型: {error}"))?;
            let status = response.status();
            let body = response
                .json::<Value>()
                .await
                .map_err(|error| format!("无法读取屏幕翻译响应: {error}"))?;
            if !status.is_success() {
                return Err(api_error(status, &body));
            }
            body.pointer("/content/0/text")
                .and_then(Value::as_str)
                .map(str::to_string)
        }
        "google" => {
            let (_, encoded) = screen_image
                .split_once(',')
                .ok_or_else(|| "屏幕图像数据无效。".to_string())?;
            let body = send_google_translation_json(provider, key, json!({
                        "systemInstruction": { "parts": [{ "text": "你是低延迟屏幕翻译器。直接翻译，不进行分析或扩展思考，只输出请求的 JSON。" }] },
                        "contents": [{ "role": "user", "parts": [
                            { "inlineData": { "mimeType": "image/jpeg", "data": encoded } },
                            { "text": prompt }
                        ] }],
                        "generationConfig": { "temperature": 0, "maxOutputTokens": 2400, "thinkingConfig": { "thinkingBudget": 0 } }
                    })).await?;
            body.pointer("/candidates/0/content/parts/0/text")
                .and_then(Value::as_str)
                .map(str::to_string)
        }
        _ => {
            let body = send_openai_translation_json(provider, key, json!({
                        "model": provider.model,
                        "messages": [
                            { "role": "system", "content": "你是低延迟屏幕翻译器。直接翻译，不进行分析或扩展思考，只输出请求的 JSON。" },
                            { "role": "user", "content": [
                                { "type": "text", "text": prompt },
                                { "type": "image_url", "image_url": { "url": screen_image, "detail": "low" } }
                            ] }
                        ],
                        "stream": false,
                        "temperature": 0,
                        "reasoning_effort": "none",
                        "max_tokens": 2400
                    })).await?;
            openai_message_text(&body)
        }
    };
    let content =
        content.ok_or_else(|| "当前模型没有返回屏幕翻译结果，可能不支持图片识别。".to_string())?;
    let json_text = first_complete_json_object(&content)
        .ok_or_else(|| "屏幕翻译模型没有返回有效 JSON。".to_string())?;
    let mut translated: ScreenTranslationResponse = serde_json::from_str(json_text)
        .map_err(|error| format!("无法解析屏幕翻译结果: {error}"))?;
    translated.items.retain(|item| {
        !item.text.trim().is_empty()
            && item.x.is_finite()
            && item.y.is_finite()
            && item.width.is_finite()
            && item.height.is_finite()
    });
    translated.items.truncate(90);
    for item in &mut translated.items {
        item.text = item.text.trim().chars().take(300).collect();
        item.x = item.x.clamp(0.0, 0.98);
        item.y = item.y.clamp(0.0, 0.98);
        item.width = item.width.clamp(0.025, 1.0 - item.x);
        item.height = item.height.clamp(0.018, 1.0 - item.y);
        item.font_size = Some(item.font_size.unwrap_or(14.0).clamp(11.0, 38.0));
    }
    Ok(translated.items)
}

fn screen_translation_fingerprint(image_data: &str) -> Option<Vec<u8>> {
    let encoded = image_data.split_once(',')?.1;
    let bytes = BASE64.decode(encoded).ok()?;
    let thumbnail = image::load_from_memory(&bytes)
        .ok()?
        .resize_exact(40, 24, image::imageops::FilterType::Triangle)
        .to_luma8();
    Some(thumbnail.pixels().map(|pixel| pixel[0] / 12).collect())
}

fn screen_translation_changed(previous: &[u8], current: &[u8]) -> bool {
    if previous.len() != current.len() {
        return true;
    }
    let changed = previous
        .iter()
        .zip(current)
        .filter(|(left, right)| left.abs_diff(**right) >= 2)
        .count();
    changed > previous.len() / 40
}

fn emit_screen_translation_status(
    app: &AppHandle,
    active: bool,
    loading: bool,
    error: Option<&str>,
) {
    let payload = json!({ "active": active, "loading": loading, "error": error });
    let _ = app.emit_to("main", "kero:screen-translation-status", payload.clone());
    let _ = app.emit_to("chat", "kero:screen-translation-status", payload.clone());
    let _ = app.emit_to("edge", "kero:screen-translation-status", payload);
}

async fn run_screen_translation(
    app: AppHandle,
    sequence: u64,
    provider: StoredProvider,
    key: String,
    settings: TranslationSettings,
) {
    let mut previous_fingerprint: Option<Vec<u8>> = None;
    let mut previous_text_signature: Option<String> = None;
    while SCREEN_TRANSLATION_ACTIVE.load(Ordering::SeqCst)
        && SCREEN_TRANSLATION_SEQUENCE.load(Ordering::SeqCst) == sequence
    {
        let detected = tokio::task::spawn_blocking(foreground_screen_text)
            .await
            .unwrap_or_default();
        if SCREEN_TRANSLATION_SEQUENCE.load(Ordering::SeqCst) != sequence {
            break;
        }
        let detected_characters = detected
            .iter()
            .map(|item| item.text.chars().count())
            .sum::<usize>();
        let use_fast_path = detected.len() >= 2 && detected_characters >= 6;
        if use_fast_path {
            let signature = detected
                .iter()
                .map(|item| {
                    format!(
                        "{}:{:.3}:{:.3}:{:.3}:{:.3}",
                        item.text, item.x, item.y, item.width, item.height
                    )
                })
                .collect::<Vec<_>>()
                .join("|");
            if previous_text_signature.as_deref() != Some(signature.as_str()) {
                emit_screen_translation_status(&app, true, true, None);
                match request_screen_text_translation(&provider, &key, &settings, &detected).await {
                    Ok(items) => {
                        if SCREEN_TRANSLATION_SEQUENCE.load(Ordering::SeqCst) != sequence {
                            break;
                        }
                        previous_text_signature = Some(signature);
                        previous_fingerprint = None;
                        let _ = app.emit_to(
                            "edge",
                            "kero:screen-translation-result",
                            json!({ "active": true, "items": items }),
                        );
                        emit_screen_translation_status(&app, true, false, None);
                    }
                    Err(_) => {
                        // Some apps expose incomplete accessibility text. Fall through
                        // to the visual path instead of failing the translation session.
                        previous_text_signature = None;
                    }
                }
            }
            if previous_text_signature.is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(1_250)).await;
                continue;
            }
        }

        let capture_app = app.clone();
        let screen_image = match tokio::task::spawn_blocking(move || {
            capture_screen_excluding_kero(&capture_app, 1600, 1000, 80, None)
        })
        .await
        {
            Ok(Ok(image)) => image,
            Ok(Err(error)) => {
                emit_screen_translation_status(&app, false, false, Some(&error));
                break;
            }
            Err(error) => {
                let detail = format!("屏幕翻译截图任务异常结束: {error}");
                emit_screen_translation_status(&app, false, false, Some(&detail));
                break;
            }
        };
        let fingerprint = screen_translation_fingerprint(&screen_image);
        let needs_visual_translation = match (&previous_fingerprint, &fingerprint) {
            (Some(previous), Some(current)) => screen_translation_changed(previous, current),
            _ => true,
        };
        if needs_visual_translation {
            emit_screen_translation_status(&app, true, true, None);
            match request_screen_translation(&provider, &key, &settings, &screen_image).await {
                Ok(items) => {
                    if SCREEN_TRANSLATION_SEQUENCE.load(Ordering::SeqCst) != sequence {
                        break;
                    }
                    previous_fingerprint = fingerprint;
                    previous_text_signature = None;
                    let _ = app.emit_to(
                        "edge",
                        "kero:screen-translation-result",
                        json!({ "active": true, "items": items }),
                    );
                    emit_screen_translation_status(&app, true, false, None);
                }
                Err(error) => {
                    SCREEN_TRANSLATION_ACTIVE.store(false, Ordering::SeqCst);
                    emit_screen_translation_status(&app, false, false, Some(&error));
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(1_250)).await;
    }
}

#[tauri::command]
fn start_screen_translation(app: AppHandle) -> Result<(), String> {
    let request = ChatRequest {
        provider_id: None,
        messages: Vec::new(),
        attachments: Vec::new(),
        screen_image: None,
        web_search: false,
    };
    let (provider, key, _, _) = resolve_chat_provider(&request)?;
    let settings = normalized_translation_settings(&load_config()?);
    let sequence = SCREEN_TRANSLATION_SEQUENCE.fetch_add(1, Ordering::SeqCst) + 1;
    SCREEN_TRANSLATION_ACTIVE.store(true, Ordering::SeqCst);
    show_edge_on_main_monitor(&app)?;
    emit_screen_translation_status(&app, true, true, None);
    tauri::async_runtime::spawn(run_screen_translation(
        app, sequence, provider, key, settings,
    ));
    Ok(())
}

#[tauri::command]
fn stop_screen_translation(app: AppHandle) -> Result<(), String> {
    SCREEN_TRANSLATION_ACTIVE.store(false, Ordering::SeqCst);
    SCREEN_TRANSLATION_SEQUENCE.fetch_add(1, Ordering::SeqCst);
    emit_screen_translation_status(&app, false, false, None);
    app.emit_to(
        "edge",
        "kero:screen-translation-result",
        json!({ "active": false, "items": [] }),
    )
    .map_err(|error| error.to_string())?;
    if let Some(edge) = app.get_webview_window("edge") {
        edge.hide().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn optimize_dictation(
    text: String,
    correct_typos: Option<bool>,
    vocabulary: Option<String>,
    memory: Option<String>,
) -> Result<String, String> {
    let optimized = dictation::optimize_dictation(text, correct_typos, vocabulary, memory).await?;
    Ok(dictation::normalize_dictation_punctuation(&optimized))
}

#[tauri::command]
async fn warm_dictation_service() -> Result<(), String> {
    warm_dictation_service_inner().await
}

async fn warm_dictation_service_inner() -> Result<(), String> {
    // 润色走聊天模型，ASR 走 DashScope；两条链路并行预热。
    let polish_warm = async {
        let request = ChatRequest {
            provider_id: None,
            messages: Vec::new(),
            attachments: Vec::new(),
            screen_image: None,
            web_search: false,
        };
        let Ok((provider, key, _, _)) = resolve_chat_provider(&request) else {
            return;
        };
        let client = shared_http_client();
        let request = match provider.kind.as_str() {
            "anthropic" => client
                .head(endpoint(
                    &provider.base_url,
                    "https://api.anthropic.com",
                    "/v1/messages",
                ))
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            "google" => {
                let base = if provider.base_url.trim().is_empty() {
                    "https://generativelanguage.googleapis.com"
                } else {
                    provider.base_url.trim_end_matches('/')
                };
                client.head(format!("{base}/v1beta/models?key={key}"))
            }
            _ => client
                .head(endpoint(
                    &provider.base_url,
                    "https://api.openai.com",
                    "/v1/models",
                ))
                .bearer_auth(key),
        };
        let _ = tokio::time::timeout(std::time::Duration::from_millis(1_500), request.send()).await;
    };
    let asr_warm = async {
        let Ok(config) = load_config() else {
            return;
        };
        let Some(origin) = dashscope_asr_origin(config.dictation_asr.base_url.trim_end_matches('/'))
        else {
            return;
        };
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(1_200),
            shared_http_client().head(origin).send(),
        )
        .await;
    };
    let ((), ()) = tokio::join!(polish_warm, asr_warm);
    Ok(())
}

#[tauri::command]
fn realtime_model_supported(request: ChatRequest) -> Result<bool, String> {
    // Custom OpenAI-compatible gateways often expose a visual model under an
    // arbitrary alias. Model-name matching cannot reliably determine vision
    // support, so let the configured endpoint decide when it receives an image.
    let _ = resolve_chat_provider(&request)?;
    Ok(true)
}

fn take_computer_mark() -> Option<ComputerMark> {
    take_computer_mark_observation().0
}

fn take_computer_mark_observation() -> (Option<ComputerMark>, u64) {
    let Ok(mut stored_mark) = COMPUTER_MARK.get_or_init(|| Mutex::new(None)).lock() else {
        return (None, COMPUTER_MARK_SEQUENCE.load(Ordering::SeqCst));
    };
    let mark = stored_mark.take();
    let sequence = COMPUTER_MARK_SEQUENCE.load(Ordering::SeqCst);
    (mark, sequence)
}

fn clear_computer_mark() {
    let _ = take_computer_mark();
    K_MARK_HELD.store(false, Ordering::SeqCst);
    if let Some(app) = HOTKEY_APP.get() {
        let _ = app.emit_to("edge", "kero:computer-mark", json!({ "active": false }));
    }
}

fn draw_computer_mark(image: &mut image::RgbaImage, mark: ComputerMark) {
    let center_x = (mark.x * (image.width().saturating_sub(1)) as f64).round() as i32;
    let center_y = (mark.y * (image.height().saturating_sub(1)) as f64).round() as i32;
    for offset_y in -28i32..=28 {
        for offset_x in -28i32..=28 {
            let x = center_x + offset_x;
            let y = center_y + offset_y;
            if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
                continue;
            }
            let distance = ((offset_x * offset_x + offset_y * offset_y) as f64).sqrt();
            let outer_ring = (distance - 20.0).abs() <= 2.5;
            let inner_ring = (distance - 16.0).abs() <= 1.5;
            let crosshair = (offset_x.abs() <= 1 && offset_y.abs() <= 28)
                || (offset_y.abs() <= 1 && offset_x.abs() <= 28);
            if outer_ring || inner_ring || crosshair {
                let colour = if outer_ring {
                    Rgba([255, 255, 255, 255])
                } else {
                    match mark.kind {
                        ComputerMarkKind::Target => Rgba([9, 102, 232, 255]),
                        ComputerMarkKind::Mistake => Rgba([235, 75, 75, 255]),
                    }
                };
                *image.get_pixel_mut(x as u32, y as u32) = colour;
            }
        }
    }
}

fn capture_screen(
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    mark: Option<ComputerMark>,
) -> Result<String, String> {
    let screen = Screen::from_point(0, 0).map_err(|error| format!("无法读取主屏幕: {error}"))?;
    let mut image = screen
        .capture()
        .map_err(|error| format!("无法截取屏幕: {error}"))?;
    if let Some(mark) = mark {
        draw_computer_mark(&mut image, mark);
    }
    let image = DynamicImage::ImageRgba8(image).resize(
        max_width,
        max_height,
        image::imageops::FilterType::Triangle,
    );
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            ImageOutputFormat::Jpeg(jpeg_quality),
        )
        .map_err(|error| format!("无法压缩屏幕画面: {error}"))?;
    Ok(format!("data:image/jpeg;base64,{}", BASE64.encode(bytes)))
}

#[cfg(windows)]
fn exclude_edge_from_screen_capture(window: &tauri::WebviewWindow) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowDisplayAffinity;

    const WDA_EXCLUDEFROMCAPTURE: u32 = 0x0000_0011;
    let Ok(hwnd) = window.hwnd() else {
        return false;
    };
    unsafe { SetWindowDisplayAffinity(hwnd.0 as _, WDA_EXCLUDEFROMCAPTURE) != 0 }
}

#[cfg(not(windows))]
fn exclude_edge_from_screen_capture(_window: &tauri::WebviewWindow) -> bool {
    false
}

#[cfg(windows)]
fn hide_window_for_capture(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows_sys::Win32::Graphics::Dwm::DwmFlush;
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    unsafe {
        ShowWindow(hwnd.0 as _, SW_HIDE);
        let _ = DwmFlush();
    }
    Ok(())
}

#[cfg(not(windows))]
fn hide_window_for_capture(window: &tauri::WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|error| error.to_string())
}

fn capture_screen_excluding_kero(
    app: &AppHandle,
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    mark: Option<ComputerMark>,
) -> Result<String, String> {
    let mut hidden_windows = Vec::new();
    for label in ["main", "edge", "chat", "settings"] {
        if label == "edge" && EDGE_CAPTURE_EXCLUDED.load(Ordering::SeqCst) {
            continue;
        }
        let Some(window) = app.get_webview_window(label) else {
            continue;
        };
        if window.is_visible().map_err(|error| error.to_string())? {
            hide_window_for_capture(&window)?;
            hidden_windows.push((label, window));
        }
    }

    // Native hiding plus a DWM flush prevents transparent WebViews from leaking into the capture.
    std::thread::sleep(std::time::Duration::from_millis(120));
    let captured = capture_screen(max_width, max_height, jpeg_quality, mark);

    for (label, window) in hidden_windows {
        let restored = if label == "edge" {
            window
                .set_ignore_cursor_events(true)
                .map_err(|error| error.to_string())
                .and_then(|_| show_window_without_activation(&window))
        } else {
            show_window_without_activation(&window)
        };
        if let Err(error) = restored {
            trace_computer_control(&format!("failed to restore Kero window {label}: {error}"));
        }
    }
    captured
}

#[tauri::command]
fn capture_primary_screen(app: AppHandle) -> Result<String, String> {
    capture_screen_excluding_kero(&app, 1280, 720, 72, None)
}

fn capture_computer_control_screen(
    app: &AppHandle,
    mark: Option<ComputerMark>,
) -> Result<String, String> {
    capture_screen_excluding_kero(app, 2048, 1280, 88, mark)
}

async fn observe_computer_state(
    app: &AppHandle,
    mark: Option<ComputerMark>,
) -> Result<(String, Option<SemanticSnapshot>, String), String> {
    let capture_app = app.clone();
    let capture =
        tokio::task::spawn_blocking(move || capture_computer_control_screen(&capture_app, mark));
    let semantics = tokio::task::spawn_blocking(|| foreground_semantic_snapshot().ok());
    let windows = tokio::task::spawn_blocking(visible_window_inventory);
    let tray = tokio::task::spawn_blocking(system_tray_inventory);
    let desktop = tokio::task::spawn_blocking(|| {
        desktop_icons()
            .unwrap_or_default()
            .into_iter()
            .take(256)
            .map(|icon| icon.name)
            .collect::<Vec<_>>()
            .join(" | ")
    });
    let screen_image = capture
        .await
        .map_err(|error| format!("屏幕观察任务异常结束: {error}"))??;
    let semantic_snapshot = semantics
        .await
        .map_err(|error| format!("控件观察任务异常结束: {error}"))?;
    let desktop_index = desktop
        .await
        .map_err(|error| format!("桌面索引任务异常结束: {error}"))?;
    let window_inventory = windows
        .await
        .map_err(|error| format!("窗口观察任务异常结束: {error}"))?;
    let tray_inventory = tray
        .await
        .map_err(|error| format!("系统托盘观察任务异常结束: {error}"))?;
    Ok((
        screen_image,
        semantic_snapshot,
        format!(
            "[DESKTOP SHORTCUTS]\n{}\n\n{}\n\n{}",
            if desktop_index.is_empty() {
                "(No desktop shortcuts detected.)"
            } else {
                &desktop_index
            },
            window_inventory,
            tray_inventory
        ),
    ))
}

fn screen_with_focus_inset(image_data: &str, x: f64, y: f64) -> Result<String, String> {
    let encoded = image_data
        .split_once(',')
        .map(|(_, value)| value)
        .ok_or_else(|| "屏幕图像数据无效。".to_string())?;
    let bytes = BASE64
        .decode(encoded)
        .map_err(|error| format!("无法读取局部屏幕图像: {error}"))?;
    let full = image::load_from_memory(&bytes)
        .map_err(|error| format!("无法解码局部屏幕图像: {error}"))?;
    let width = full.width();
    let height = full.height();
    let crop_width = (width as f64 * 0.36).round().clamp(280.0, 720.0) as u32;
    let crop_height = (height as f64 * 0.42).round().clamp(240.0, 720.0) as u32;
    let center_x = (x.clamp(0.0, 1.0) * width.saturating_sub(1) as f64).round() as i64;
    let center_y = (y.clamp(0.0, 1.0) * height.saturating_sub(1) as f64).round() as i64;
    let left =
        (center_x - crop_width as i64 / 2).clamp(0, width.saturating_sub(crop_width) as i64) as u32;
    let top = (center_y - crop_height as i64 / 2)
        .clamp(0, height.saturating_sub(crop_height) as i64) as u32;
    let inset = full
        .crop_imm(left, top, crop_width.min(width), crop_height.min(height))
        .resize(560, 420, image::imageops::FilterType::CatmullRom)
        .to_rgba8();
    let mut composite = full.to_rgba8();
    let inset_x = composite.width().saturating_sub(inset.width() + 26);
    let inset_y = 26u32;
    for border_y in
        inset_y.saturating_sub(4)..(inset_y + inset.height() + 4).min(composite.height())
    {
        for border_x in
            inset_x.saturating_sub(4)..(inset_x + inset.width() + 4).min(composite.width())
        {
            if border_x < inset_x
                || border_x >= inset_x + inset.width()
                || border_y < inset_y
                || border_y >= inset_y + inset.height()
            {
                *composite.get_pixel_mut(border_x, border_y) = Rgba([245, 248, 255, 255]);
            }
        }
    }
    image::imageops::overlay(&mut composite, &inset, inset_x as i64, inset_y as i64);
    let mut output = Vec::new();
    DynamicImage::ImageRgba8(composite)
        .write_to(
            &mut std::io::Cursor::new(&mut output),
            ImageOutputFormat::Jpeg(86),
        )
        .map_err(|error| format!("无法压缩局部屏幕图像: {error}"))?;
    Ok(format!("data:image/jpeg;base64,{}", BASE64.encode(output)))
}

fn last_history_point(history: &str) -> Option<(f64, f64)> {
    let marker = "point=(";
    let start = history.rfind(marker)? + marker.len();
    let rest = &history[start..];
    let end = rest.find(')')?;
    let (x, y) = rest[..end].split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

#[cfg(windows)]
unsafe extern "system" fn collect_visible_window(
    hwnd: windows_sys::Win32::Foundation::HWND,
    context: windows_sys::Win32::Foundation::LPARAM,
) -> i32 {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed,
    };

    if hwnd.is_null() || IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let mut process_id = 0u32;
    GetWindowThreadProcessId(hwnd, &mut process_id);
    if process_id == GetCurrentProcessId() {
        return 1;
    }
    let title_length = GetWindowTextLengthW(hwnd);
    if title_length <= 0 || title_length > 512 {
        return 1;
    }
    let mut title_buffer = vec![0u16; title_length as usize + 1];
    let copied = GetWindowTextW(hwnd, title_buffer.as_mut_ptr(), title_buffer.len() as i32);
    if copied <= 0 {
        return 1;
    }
    let title = String::from_utf16_lossy(&title_buffer[..copied as usize])
        .trim()
        .replace(['\r', '\n', '|'], " ");
    if title.is_empty() {
        return 1;
    }
    let mut class_buffer = [0u16; 160];
    let class_length = GetClassNameW(hwnd, class_buffer.as_mut_ptr(), class_buffer.len() as i32);
    let class_name = String::from_utf16_lossy(&class_buffer[..class_length.max(0) as usize]);
    let Ok((screen_left, screen_top, screen_width, screen_height)) =
        primary_screen_physical_bounds()
    else {
        return 1;
    };
    let minimized = IsIconic(hwnd) != 0;
    let maximized = IsZoomed(hwnd) != 0;
    let mut rect = RECT::default();
    if !minimized && GetWindowRect(hwnd, &mut rect) == 0 {
        return 1;
    }
    let clipped_left = rect.left.max(screen_left);
    let clipped_top = rect.top.max(screen_top);
    let clipped_right = rect.right.min(screen_left + screen_width);
    let clipped_bottom = rect.bottom.min(screen_top + screen_height);
    let visible_width = (clipped_right - clipped_left).max(0);
    let visible_height = (clipped_bottom - clipped_top).max(0);
    if !minimized && (visible_width < 2 || visible_height < 2) {
        return 1;
    }
    let full_area =
        ((rect.right - rect.left).max(1) as f64) * ((rect.bottom - rect.top).max(1) as f64);
    let visible_area = visible_width as f64 * visible_height as f64;
    let windows = &mut *(context as *mut Vec<VisibleWindowObservation>);
    windows.push(VisibleWindowObservation {
        handle: hwnd,
        id: format!("hwnd:{:x}", hwnd as usize),
        title,
        class_name,
        state: if minimized {
            "minimized"
        } else if maximized {
            "maximized"
        } else {
            "normal"
        },
        left: ((clipped_left - screen_left) as f64 / screen_width.max(1) as f64).clamp(0.0, 1.0),
        top: ((clipped_top - screen_top) as f64 / screen_height.max(1) as f64).clamp(0.0, 1.0),
        right: ((clipped_right - screen_left) as f64 / screen_width.max(1) as f64).clamp(0.0, 1.0),
        bottom: ((clipped_bottom - screen_top) as f64 / screen_height.max(1) as f64)
            .clamp(0.0, 1.0),
        visible_fraction: if minimized {
            0.0
        } else {
            visible_area / full_area
        },
        foreground: hwnd == GetForegroundWindow(),
    });
    1
}

#[cfg(windows)]
fn visible_windows() -> Vec<VisibleWindowObservation> {
    use windows_sys::Win32::UI::WindowsAndMessaging::EnumWindows;

    let mut windows: Vec<VisibleWindowObservation> = Vec::new();
    unsafe {
        EnumWindows(
            Some(collect_visible_window),
            (&mut windows as *mut Vec<_>) as isize,
        );
    }
    windows.sort_by(|left, right| {
        right
            .foreground
            .cmp(&left.foreground)
            .then_with(|| left.visible_fraction.total_cmp(&right.visible_fraction))
    });
    windows
}

#[cfg(windows)]
fn visible_window_inventory() -> String {
    let windows = visible_windows()
        .into_iter()
        .take(36)
        .map(|window| {
            format!(
                "- id={} | title={} | class={} | state={} | visibleBounds=({:.3},{:.3})-({:.3},{:.3}) | visibleFraction={:.3} | foreground={}",
                window.id,
                window.title,
                window.class_name,
                window.state,
                window.left,
                window.top,
                window.right,
                window.bottom,
                window.visible_fraction,
                window.foreground,
            )
        })
        .collect::<Vec<_>>();
    format!(
        "[VISIBLE WINDOWS]\n{}",
        if windows.is_empty() {
            "(No external top-level windows detected.)".to_string()
        } else {
            windows.join("\n")
        }
    )
}

#[cfg(windows)]
fn named_controls_for_window(
    hwnd: windows_sys::Win32::Foundation::HWND,
    source: &str,
) -> Vec<String> {
    use uiautomation::{types::Handle, UIAutomation, UIElement};

    if hwnd.is_null() {
        return Vec::new();
    }
    let Ok(automation) = UIAutomation::new() else {
        return Vec::new();
    };
    let Ok(root) = automation.element_from_handle(Handle::from(hwnd as isize)) else {
        return Vec::new();
    };
    let Ok(walker) = automation.get_control_view_walker() else {
        return Vec::new();
    };
    let Ok((_, _, screen_width, screen_height)) = primary_screen_physical_bounds() else {
        return Vec::new();
    };
    let mut controls = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<(UIElement, usize)> = vec![(root, 0)];
    while let Some((element, depth)) = stack.pop() {
        if depth > 10 || controls.len() >= 96 {
            continue;
        }
        let mut children = Vec::new();
        if let Ok(first) = walker.get_first_child(&element) {
            let mut child = first;
            loop {
                children.push(child.clone());
                match walker.get_next_sibling(&child) {
                    Ok(next) => child = next,
                    Err(_) => break,
                }
            }
        }
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
        if depth == 0
            || element.is_offscreen().unwrap_or(true)
            || !element.is_enabled().unwrap_or(false)
        {
            continue;
        }
        let name = element
            .get_name()
            .unwrap_or_default()
            .trim()
            .replace(['\r', '\n', '|'], " ");
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        let rect = match element.get_bounding_rectangle() {
            Ok(rect) if rect.get_width() > 3 && rect.get_height() > 3 => rect,
            _ => continue,
        };
        let control_type = element
            .get_control_type()
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|_| "Unknown".to_string());
        if !matches!(
            control_type.as_str(),
            "Button" | "MenuItem" | "ListItem" | "Hyperlink" | "TabItem"
        ) {
            continue;
        }
        let x = ((rect.get_left() + rect.get_width() / 2) as f64 / screen_width.max(1) as f64)
            .clamp(0.0, 1.0);
        let y = ((rect.get_top() + rect.get_height() / 2) as f64 / screen_height.max(1) as f64)
            .clamp(0.0, 1.0);
        controls.push(format!(
            "- name={} | type={} | point=({:.3},{:.3}) | source={}",
            name, control_type, x, y, source
        ));
    }
    controls
}

#[cfg(windows)]
fn system_tray_inventory() -> String {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, IsWindowVisible};

    let find_window = |class_name: &str| {
        let encoded = class_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        unsafe { FindWindowW(encoded.as_ptr(), std::ptr::null()) }
    };
    let mut controls = named_controls_for_window(find_window("Shell_TrayWnd"), "taskbar");
    let overflow = find_window("NotifyIconOverflowWindow");
    if !overflow.is_null() && unsafe { IsWindowVisible(overflow) } != 0 {
        controls.extend(named_controls_for_window(overflow, "tray-overflow"));
    }
    controls.sort();
    controls.dedup();
    format!(
        "[SYSTEM TRAY CONTROLS]\n{}",
        if controls.is_empty() {
            "(No named tray controls detected; inspect the bottom-right taskbar visually.)"
                .to_string()
        } else {
            controls.into_iter().take(96).collect::<Vec<_>>().join("\n")
        }
    )
}

#[cfg(not(windows))]
fn system_tray_inventory() -> String {
    "[SYSTEM TRAY CONTROLS] unavailable".to_string()
}

#[cfg(not(windows))]
fn visible_window_inventory() -> String {
    "[VISIBLE WINDOWS] unavailable".to_string()
}

#[cfg(windows)]
fn resolve_visible_window(target: &str) -> Result<VisibleWindowObservation, String> {
    let target = target.trim();
    let windows = visible_windows();
    if let Some(window) = windows
        .iter()
        .find(|window| window.id.eq_ignore_ascii_case(target))
    {
        return Ok(window.clone());
    }
    let exact = windows
        .iter()
        .filter(|window| window.title.eq_ignore_ascii_case(target))
        .cloned()
        .collect::<Vec<_>>();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }
    let target_lower = target.to_lowercase();
    let partial = windows
        .into_iter()
        .filter(|window| window.title.to_lowercase().contains(&target_lower))
        .collect::<Vec<_>>();
    match partial.len() {
        1 => Ok(partial.into_iter().next().expect("one window match exists")),
        0 => Err(format!("WINDOW_TARGET_NOT_FOUND:{target}")),
        _ => Err(format!("WINDOW_TARGET_AMBIGUOUS:{target}")),
    }
}

#[cfg(windows)]
fn focus_or_maximize_window(target: &str, maximize: bool) -> Result<&'static str, String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, SetForegroundWindow, ShowWindow, SW_MAXIMIZE, SW_RESTORE,
    };

    let window = resolve_visible_window(target)?;
    unsafe {
        ShowWindow(
            window.handle,
            if maximize { SW_MAXIMIZE } else { SW_RESTORE },
        );
        BringWindowToTop(window.handle);
        SetForegroundWindow(window.handle);
    }
    Ok(if maximize {
        "win32_maximize_existing_window"
    } else {
        "win32_activate_existing_window"
    })
}

#[cfg(windows)]
fn external_foreground_window() -> Result<windows_sys::Win32::Foundation::HWND, String> {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetLastActivePopup, GetWindow, GetWindowThreadProcessId,
        IsWindowVisible, GW_HWNDNEXT,
    };

    let current_process = unsafe { GetCurrentProcessId() };
    let foreground = unsafe { GetForegroundWindow() };
    let mut window = foreground;
    for _ in 0..48 {
        if window.is_null() {
            break;
        }
        let mut process_id = 0u32;
        unsafe {
            GetWindowThreadProcessId(window, &mut process_id);
        }
        if process_id != current_process && unsafe { IsWindowVisible(window) } != 0 {
            // When a normal app opens a modal dialog Windows keeps the owner as
            // foreground on some hosts. The active popup is the interactive one.
            let popup = unsafe { GetLastActivePopup(window) };
            if !popup.is_null() && popup != window && unsafe { IsWindowVisible(popup) } != 0 {
                return Ok(popup);
            }
            return Ok(window);
        }
        window = unsafe { GetWindow(window, GW_HWNDNEXT) };
    }
    Err("未找到可读取的前台应用窗口。".to_string())
}

#[cfg(windows)]
fn focus_external_window(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{BringWindowToTop, SetForegroundWindow};

    unsafe {
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
    }
}

fn semantic_control_type_supported(control_type: &str) -> bool {
    matches!(
        control_type,
        "Button"
            | "CheckBox"
            | "ComboBox"
            | "Edit"
            | "Hyperlink"
            | "ListItem"
            | "MenuItem"
            | "RadioButton"
            | "Slider"
            | "TabItem"
            | "TreeItem"
    )
}

fn semantic_control_id(
    control_type: &str,
    name: &str,
    automation_id: &str,
    occurrences: &mut HashMap<String, u16>,
) -> String {
    let base = if automation_id.is_empty() {
        format!("name:{control_type}:{name}")
    } else {
        format!("id:{automation_id}")
    };
    let occurrence = occurrences.entry(base.clone()).or_insert(0);
    *occurrence = occurrence.saturating_add(1);
    format!("{base}#{}", *occurrence)
}

fn semantic_snapshot_fingerprint(
    window_key: &str,
    window_name: &str,
    controls: &[SemanticControl],
) -> String {
    // Stable FNV-1a fingerprint: enough to detect meaningful UI changes without
    // retaining screenshots or leaking extra data through the MCP bridge.
    let mut hash = 0xcbf29ce484222325u64;
    let mut update = |value: &str| {
        for byte in value.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    };
    update(window_key);
    update(window_name);
    for control in controls {
        update(&control.id);
        update(&control.name);
        update(&control.control_type);
        update(&format!(
            "{:.2}:{:.2}:{}",
            control.x, control.y, control.enabled
        ));
    }
    format!("{hash:016x}")
}

#[cfg(windows)]
fn foreground_semantic_snapshot() -> Result<SemanticSnapshot, String> {
    use uiautomation::{types::Handle, UIAutomation, UIElement};

    let automation =
        UIAutomation::new().map_err(|error| format!("无法启动 Windows UI Automation: {error}"))?;
    let hwnd = external_foreground_window()?;
    let root = automation
        .element_from_handle(Handle::from(hwnd as isize))
        .map_err(|error| format!("无法读取前台窗口控件: {error}"))?;
    let window_name = root.get_name().unwrap_or_default().trim().to_string();
    let window_class = root.get_classname().unwrap_or_default().trim().to_string();
    let window_key = format!(
        "{}|{}",
        window_class.to_ascii_lowercase(),
        window_name.to_ascii_lowercase()
    );
    let walker = automation
        .get_control_view_walker()
        .map_err(|error| format!("无法读取 Windows 控件树: {error}"))?;
    let (_, _, screen_width, screen_height) = primary_screen_physical_bounds()?;
    let mut controls = Vec::new();
    let mut occurrences = HashMap::new();
    let mut stack: Vec<(UIElement, usize)> = vec![(root, 0)];
    while let Some((element, depth)) = stack.pop() {
        if depth > 8 || controls.len() >= 90 {
            continue;
        }
        let mut children = Vec::new();
        if let Ok(first) = walker.get_first_child(&element) {
            let mut child = first;
            loop {
                children.push(child.clone());
                match walker.get_next_sibling(&child) {
                    Ok(next) => child = next,
                    Err(_) => break,
                }
            }
        }
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
        if depth == 0
            || element.is_offscreen().unwrap_or(true)
            || !element.is_enabled().unwrap_or(false)
        {
            continue;
        }
        let name = element
            .get_name()
            .unwrap_or_default()
            .trim()
            .replace(['\r', '\n'], " ");
        if name.is_empty() {
            continue;
        }
        let control_type = element
            .get_control_type()
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|_| "Unknown".to_string());
        if !semantic_control_type_supported(&control_type) {
            continue;
        }
        let rect = match element.get_bounding_rectangle() {
            Ok(value) if value.get_width() > 3 && value.get_height() > 3 => value,
            _ => continue,
        };
        let automation_id = element
            .get_automation_id()
            .unwrap_or_default()
            .trim()
            .to_string();
        let id = semantic_control_id(&control_type, &name, &automation_id, &mut occurrences);
        let x = ((rect.get_left() + rect.get_width() / 2) as f64 / screen_width.max(1) as f64)
            .clamp(0.0, 1.0);
        let y = ((rect.get_top() + rect.get_height() / 2) as f64 / screen_height.max(1) as f64)
            .clamp(0.0, 1.0);
        controls.push(SemanticControl {
            id,
            name,
            control_type,
            x,
            y,
            enabled: true,
        });
    }
    Ok(SemanticSnapshot {
        fingerprint: semantic_snapshot_fingerprint(&window_key, &window_name, &controls),
        window_key,
        window_name,
        window_class,
        controls,
    })
}

#[cfg(windows)]
fn foreground_screen_text() -> Vec<DetectedScreenText> {
    use uiautomation::{types::Handle, UIAutomation, UIElement};

    let Ok(automation) = UIAutomation::new() else {
        return Vec::new();
    };
    let Ok(hwnd) = external_foreground_window() else {
        return Vec::new();
    };
    let Ok(root) = automation.element_from_handle(Handle::from(hwnd as isize)) else {
        return Vec::new();
    };
    let Ok(walker) = automation.get_control_view_walker() else {
        return Vec::new();
    };
    let Ok((screen_left, screen_top, screen_width, screen_height)) =
        primary_screen_physical_bounds()
    else {
        return Vec::new();
    };
    let allowed_types = [
        "Text",
        "Button",
        "CheckBox",
        "ComboBox",
        "Edit",
        "Hyperlink",
        "ListItem",
        "MenuItem",
        "RadioButton",
        "TabItem",
        "TreeItem",
    ];
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<(UIElement, usize)> = vec![(root, 0)];
    while let Some((element, depth)) = stack.pop() {
        if depth > 14 || result.len() >= 140 {
            continue;
        }
        let mut children = Vec::new();
        if let Ok(first) = walker.get_first_child(&element) {
            let mut child = first;
            loop {
                children.push(child.clone());
                match walker.get_next_sibling(&child) {
                    Ok(next) => child = next,
                    Err(_) => break,
                }
            }
        }
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
        if depth == 0 || element.is_offscreen().unwrap_or(true) {
            continue;
        }
        let control_type = element
            .get_control_type()
            .map(|value| format!("{value:?}"))
            .unwrap_or_default();
        if !allowed_types.contains(&control_type.as_str()) {
            continue;
        }
        let text = element
            .get_name()
            .unwrap_or_default()
            .trim()
            .replace(['\r', '\n', '|'], " ");
        let character_count = text.chars().count();
        if character_count == 0 || character_count > 300 {
            continue;
        }
        let rect = match element.get_bounding_rectangle() {
            Ok(rect) if rect.get_width() > 3 && rect.get_height() > 3 => rect,
            _ => continue,
        };
        let left =
            ((rect.get_left() - screen_left) as f64 / screen_width.max(1) as f64).clamp(0.0, 1.0);
        let top =
            ((rect.get_top() - screen_top) as f64 / screen_height.max(1) as f64).clamp(0.0, 1.0);
        let width = (rect.get_width() as f64 / screen_width.max(1) as f64)
            .min(1.0 - left)
            .max(0.001);
        let height = (rect.get_height() as f64 / screen_height.max(1) as f64)
            .min(1.0 - top)
            .max(0.001);
        if width > 0.88 && height > 0.28 && character_count > 80 {
            continue;
        }
        let signature = format!(
            "{}:{}:{}:{}",
            text.to_lowercase(),
            (left * 500.0).round(),
            (top * 500.0).round(),
            (width * 500.0).round()
        );
        if !seen.insert(signature) {
            continue;
        }
        result.push(DetectedScreenText {
            id: format!("t{}", result.len() + 1),
            text,
            x: left,
            y: top,
            width,
            height,
            font_size: (rect.get_height() as f64 * 0.58).clamp(11.0, 38.0),
        });
    }
    result
}

#[cfg(not(windows))]
fn foreground_screen_text() -> Vec<DetectedScreenText> {
    Vec::new()
}

#[cfg(not(windows))]
fn foreground_semantic_snapshot() -> Result<SemanticSnapshot, String> {
    Err("Windows UI Automation 仅可在 Windows 上使用。".to_string())
}

fn semantic_snapshot_text(snapshot: &SemanticSnapshot) -> String {
    let controls = snapshot
        .controls
        .iter()
        .map(|control| {
            format!(
                "- id={} | type={} | name={} | point=({:.3},{:.3}) | enabled={}",
                control.id,
                control.control_type,
                control.name,
                control.x,
                control.y,
                control.enabled
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[WINDOW SEMANTICS] title={} | class={} | key={}\n{}",
        snapshot.window_name,
        snapshot.window_class,
        snapshot.window_key,
        if controls.is_empty() {
            "(No actionable accessibility controls exposed; use visual grounding.)"
        } else {
            &controls
        }
    )
}

fn remembered_correction_text(snapshot: Option<&SemanticSnapshot>) -> String {
    let Some(snapshot) = snapshot else {
        return String::new();
    };
    let Ok(config) = load_config() else {
        return String::new();
    };
    let points = config
        .computer_corrections
        .iter()
        .filter(|correction| correction.window_key == snapshot.window_key)
        .take(4)
        .map(|correction| format!("({:.3},{:.3})", correction.x, correction.y))
        .collect::<Vec<_>>();
    if points.is_empty() {
        String::new()
    } else {
        format!("[USER CORRECTION MEMORY] The user previously marked these regions in this window for extra review: {}. Do not treat them as automatic click targets; inspect them carefully when relevant.", points.join(", "))
    }
}

fn semantic_target_point(target: &str) -> Result<(f64, f64), String> {
    let snapshot = foreground_semantic_snapshot()?;
    snapshot
        .controls
        .iter()
        .find(|control| control.id == target)
        .map(|control| (control.x, control.y))
        .ok_or_else(|| format!("SEMANTIC_TARGET_NOT_FOUND:{target}"))
}

#[cfg(windows)]
fn semantic_control_element(target: &str) -> Result<uiautomation::UIElement, String> {
    use uiautomation::{types::Handle, UIAutomation, UIElement};

    let automation = UIAutomation::new()
        .map_err(|error| format!("Unable to start Windows UI Automation: {error}"))?;
    let hwnd = external_foreground_window()?;
    focus_external_window(hwnd);
    let root = automation
        .element_from_handle(Handle::from(hwnd as isize))
        .map_err(|error| format!("Unable to read the target window controls: {error}"))?;
    let walker = automation
        .get_control_view_walker()
        .map_err(|error| format!("Unable to read the Windows control tree: {error}"))?;
    let mut occurrences = HashMap::new();
    let mut stack: Vec<(UIElement, usize)> = vec![(root, 0)];
    while let Some((element, depth)) = stack.pop() {
        if depth > 9 {
            continue;
        }
        let mut children = Vec::new();
        if let Ok(first) = walker.get_first_child(&element) {
            let mut child = first;
            loop {
                children.push(child.clone());
                match walker.get_next_sibling(&child) {
                    Ok(next) => child = next,
                    Err(_) => break,
                }
            }
        }
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
        if depth == 0
            || element.is_offscreen().unwrap_or(true)
            || !element.is_enabled().unwrap_or(false)
        {
            continue;
        }
        let name = element
            .get_name()
            .unwrap_or_default()
            .trim()
            .replace(['\r', '\n'], " ");
        if name.is_empty() {
            continue;
        }
        let control_type = element
            .get_control_type()
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|_| "Unknown".to_string());
        if !semantic_control_type_supported(&control_type) {
            continue;
        }
        if !matches!(
            element.get_bounding_rectangle(),
            Ok(rect) if rect.get_width() > 3 && rect.get_height() > 3
        ) {
            continue;
        }
        let automation_id = element
            .get_automation_id()
            .unwrap_or_default()
            .trim()
            .to_string();
        let id = semantic_control_id(&control_type, &name, &automation_id, &mut occurrences);
        if id == target {
            return Ok(element);
        }
    }
    Err(format!("SEMANTIC_TARGET_NOT_FOUND:{target}"))
}

#[cfg(windows)]
fn try_semantic_control_action(action: &ComputerAction) -> Result<Option<&'static str>, String> {
    use uiautomation::patterns::{UIInvokePattern, UISelectionItemPattern};

    let Some(target) = action.ui_target.as_deref() else {
        return Ok(None);
    };
    let element = semantic_control_element(target)?;
    let requested = action
        .ui_action
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let result = match requested.as_str() {
        "set_value" | "value" if action.action == "type" => element
            .set_focus()
            .map_err(|error| format!("UIA_FOCUS_FAILED:{error}"))
            .map(|_| "uia_focus_for_type"),
        "select" => element
            .get_pattern::<UISelectionItemPattern>()
            .and_then(|pattern| pattern.select())
            .map_err(|error| format!("UIA_SELECT_FAILED:{error}"))
            .map(|_| "uia_select"),
        "focus" => element
            .set_focus()
            .map_err(|error| format!("UIA_FOCUS_FAILED:{error}"))
            .map(|_| {
                if action.action == "type" {
                    "uia_focus_for_type"
                } else {
                    "uia_focus"
                }
            }),
        "" | "invoke" => {
            if action.action == "type" {
                element
                    .set_focus()
                    .map_err(|error| format!("UIA_FOCUS_FAILED:{error}"))
                    .map(|_| "uia_focus_for_type")
            } else if matches!(
                action.action.as_str(),
                "click" | "double_click" | "right_click"
            ) {
                element
                    .get_pattern::<UIInvokePattern>()
                    .and_then(|pattern| pattern.invoke())
                    .map_err(|error| format!("UIA_INVOKE_FAILED:{error}"))
                    .map(|_| "uia_invoke")
            } else {
                Ok("uia_not_applicable")
            }
        }
        _ => return Err(format!("UIA_UNKNOWN_ACTION:{requested}")),
    };
    result.map(Some)
}

fn remember_mark_correction(mark: ComputerMark, snapshot: Option<&SemanticSnapshot>) {
    let Some(snapshot) = snapshot else {
        return;
    };
    let Ok(mut config) = load_config() else {
        return;
    };
    let duplicate = config.computer_corrections.iter().any(|correction| {
        correction.window_key == snapshot.window_key
            && (correction.x - mark.x).abs() < 0.03
            && (correction.y - mark.y).abs() < 0.03
    });
    if !duplicate {
        config.computer_corrections.push(ComputerCorrection {
            window_key: snapshot.window_key.clone(),
            x: mark.x,
            y: mark.y,
        });
        if config.computer_corrections.len() > 48 {
            let remove = config.computer_corrections.len() - 48;
            config.computer_corrections.drain(0..remove);
        }
        let _ = save_config(&config);
    }
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn desktop_list_view() -> Result<windows_sys::Win32::Foundation::HWND, String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowExW, FindWindowW};

    let progman_class = wide("Progman");
    let worker_class = wide("WorkerW");
    let folder_view_class = wide("SHELLDLL_DefView");
    let list_view_class = wide("SysListView32");
    unsafe {
        let progman = FindWindowW(progman_class.as_ptr(), std::ptr::null());
        let mut folder_view = if !progman.is_null() {
            FindWindowExW(
                progman,
                std::ptr::null_mut(),
                folder_view_class.as_ptr(),
                std::ptr::null(),
            )
        } else {
            std::ptr::null_mut()
        };
        if folder_view.is_null() {
            let mut worker = std::ptr::null_mut();
            loop {
                worker = FindWindowExW(
                    std::ptr::null_mut(),
                    worker,
                    worker_class.as_ptr(),
                    std::ptr::null(),
                );
                if worker.is_null() {
                    break;
                }
                folder_view = FindWindowExW(
                    worker,
                    std::ptr::null_mut(),
                    folder_view_class.as_ptr(),
                    std::ptr::null(),
                );
                if !folder_view.is_null() {
                    break;
                }
            }
        }
        if folder_view.is_null() {
            return Err("无法定位 Windows 桌面图标视图。".to_string());
        }
        let list_view = FindWindowExW(
            folder_view,
            std::ptr::null_mut(),
            list_view_class.as_ptr(),
            std::ptr::null(),
        );
        (!list_view.is_null())
            .then_some(list_view)
            .ok_or_else(|| "无法读取 Windows 桌面图标。".to_string())
    }
}

#[cfg(windows)]
fn focus_desktop_list_view() -> Result<(), String> {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetActiveWindow, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow, GA_ROOT,
    };

    fn class_name(window: windows_sys::Win32::Foundation::HWND) -> String {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;
        if window.is_null() {
            return "<none>".to_string();
        }
        let mut buffer = [0u16; 128];
        let length = unsafe { GetClassNameW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
        String::from_utf16_lossy(&buffer[..length.max(0) as usize])
    }

    let list_view = desktop_list_view()?;
    unsafe {
        let root = GetAncestor(list_view, GA_ROOT);
        let current_thread = GetCurrentThreadId();
        let target_thread = GetWindowThreadProcessId(list_view, std::ptr::null_mut());
        let attached = current_thread != target_thread
            && target_thread != 0
            && AttachThreadInput(current_thread, target_thread, 1) != 0;

        let foreground_set = SetForegroundWindow(root) != 0;
        SetActiveWindow(root);
        SetFocus(list_view);

        let foreground = GetForegroundWindow();
        let focus = GetFocus();
        trace_computer_control(&format!(
            "desktop focus attached={attached} foreground_set={foreground_set} foreground={:?}/{} focus={:?}/{}",
            foreground,
            class_name(foreground),
            focus,
            class_name(focus)
        ));

        if attached {
            AttachThreadInput(current_thread, target_thread, 0);
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(90));
    Ok(())
}

#[cfg(windows)]
fn wait_for_application_window(
    previous_foreground: windows_sys::Win32::Foundation::HWND,
    timeout: std::time::Duration,
) -> bool {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, GA_ROOT,
    };

    let desktop_root = desktop_list_view()
        .map(|window| unsafe { GetAncestor(window, GA_ROOT) })
        .unwrap_or(std::ptr::null_mut());
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.is_null() || foreground == desktop_root || foreground == previous_foreground {
            continue;
        }
        let mut process_id = 0u32;
        unsafe {
            GetWindowThreadProcessId(foreground, &mut process_id);
        }
        if process_id == unsafe { GetCurrentProcessId() } {
            continue;
        }
        let mut class_buffer = [0u16; 128];
        let class_length = unsafe {
            GetClassNameW(
                foreground,
                class_buffer.as_mut_ptr(),
                class_buffer.len() as i32,
            )
        };
        let class_name = String::from_utf16_lossy(&class_buffer[..class_length.max(0) as usize]);
        if matches!(
            class_name.as_str(),
            "WorkerW" | "Progman" | "SysListView32" | "SHELLDLL_DefView"
        ) {
            continue;
        }
        trace_computer_control(&format!(
            "application window detected foreground={:?} class={class_name} process_id={process_id}",
            foreground
        ));
        return true;
    }
    false
}

#[cfg(windows)]
fn desktop_icons() -> Result<Vec<DesktopIcon>, String> {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, RECT};
    use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
    use windows_sys::Win32::System::Memory::{
        VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, SendMessageW};

    const LVM_FIRST: u32 = 0x1000;
    const LVM_GETITEMCOUNT: u32 = LVM_FIRST + 4;
    const LVM_GETITEMRECT: u32 = LVM_FIRST + 14;
    const LVM_GETITEMTEXTW: u32 = LVM_FIRST + 115;
    const LVIF_TEXT: u32 = 0x0001;
    const LVIR_ICON: i32 = 1;
    const TEXT_CAPACITY: usize = 320;
    const REMOTE_BYTES: usize = 2048;

    fn write_remote<T>(process: HANDLE, address: *mut c_void, value: &T) -> Result<(), String> {
        let mut written = 0usize;
        let success = unsafe {
            WriteProcessMemory(
                process,
                address,
                value as *const T as *const c_void,
                std::mem::size_of::<T>(),
                &mut written,
            )
        };
        (success != 0 && written == std::mem::size_of::<T>())
            .then_some(())
            .ok_or_else(|| "无法写入桌面图标查询缓冲区。".to_string())
    }

    fn read_remote<T: Copy>(process: HANDLE, address: *const c_void) -> Result<T, String> {
        let mut value = unsafe { std::mem::zeroed::<T>() };
        let mut read = 0usize;
        let success = unsafe {
            ReadProcessMemory(
                process,
                address,
                &mut value as *mut T as *mut c_void,
                std::mem::size_of::<T>(),
                &mut read,
            )
        };
        (success != 0 && read == std::mem::size_of::<T>())
            .then_some(value)
            .ok_or_else(|| "无法读取桌面图标查询结果。".to_string())
    }

    let list_view = desktop_list_view()?;
    let mut process_id = 0u32;
    unsafe {
        GetWindowThreadProcessId(list_view, &mut process_id);
    }
    let process = unsafe {
        OpenProcess(
            PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE,
            0,
            process_id,
        )
    };
    if process.is_null() {
        return Err("无法读取 Windows 桌面图标位置。".to_string());
    }
    let remote = unsafe {
        VirtualAllocEx(
            process,
            std::ptr::null(),
            REMOTE_BYTES,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    if remote.is_null() {
        unsafe {
            CloseHandle(process);
        }
        return Err("无法分配桌面图标查询缓冲区。".to_string());
    }

    let result = (|| -> Result<Vec<DesktopIcon>, String> {
        let text_address = unsafe { (remote as *mut u8).add(512) as *mut u16 };
        let rect_address = unsafe { (remote as *mut u8).add(1536) as *mut RECT };
        let count = unsafe { SendMessageW(list_view, LVM_GETITEMCOUNT, 0, 0) }.max(0) as usize;
        let count = count.min(512);
        let mut raw_icons = Vec::with_capacity(count);
        for index in 0..count {
            let item = RemoteLvItemW {
                mask: LVIF_TEXT,
                item: index as i32,
                sub_item: 0,
                state: 0,
                state_mask: 0,
                text: text_address,
                text_max: TEXT_CAPACITY as i32,
                image: 0,
                l_param: 0,
                indent: 0,
                group_id: 0,
                columns: 0,
                column_indices: std::ptr::null_mut(),
                column_formats: std::ptr::null_mut(),
                group: 0,
            };
            write_remote(process, remote, &item)?;
            unsafe {
                SendMessageW(list_view, LVM_GETITEMTEXTW, index, remote as isize);
            }
            let mut text = vec![0u16; TEXT_CAPACITY];
            let mut read = 0usize;
            let text_read = unsafe {
                ReadProcessMemory(
                    process,
                    text_address as *const c_void,
                    text.as_mut_ptr() as *mut c_void,
                    text.len() * std::mem::size_of::<u16>(),
                    &mut read,
                )
            };
            if text_read == 0 || read < std::mem::size_of::<u16>() {
                continue;
            }
            let end = text
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(text.len());
            let name = String::from_utf16_lossy(&text[..end]).trim().to_string();
            if name.is_empty() {
                continue;
            }
            // Target the icon image itself. LVIR_BOUNDS also includes the label and
            // can put its geometric center in the non-clickable gap between them.
            write_remote(
                process,
                rect_address as *mut c_void,
                &RECT {
                    left: LVIR_ICON,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
            )?;
            unsafe {
                SendMessageW(list_view, LVM_GETITEMRECT, index, rect_address as isize);
            }
            let rect = read_remote::<RECT>(process, rect_address as *const c_void)?;
            if rect.right <= rect.left || rect.bottom <= rect.top {
                continue;
            }
            raw_icons.push((name, rect));
        }

        let screen =
            Screen::from_point(0, 0).map_err(|error| format!("无法读取主屏幕: {error}"))?;
        let info = screen.display_info;
        let scale = info.scale_factor as f64;
        let physical_width = (info.width as f64 * scale).max(1.0);
        let physical_height = (info.height as f64 * scale).max(1.0);
        Ok(raw_icons
            .into_iter()
            .map(|(name, rect)| {
                let center_x = (rect.left + rect.right) as f64 / 2.0;
                let center_y = (rect.top + rect.bottom) as f64 / 2.0;
                DesktopIcon {
                    name,
                    x: (center_x / physical_width).clamp(0.0, 1.0),
                    y: (center_y / physical_height).clamp(0.0, 1.0),
                }
            })
            .collect())
    })();

    unsafe {
        VirtualFreeEx(process, remote, 0, MEM_RELEASE);
        CloseHandle(process);
    }
    result
}

#[cfg(not(windows))]
fn desktop_icons() -> Result<Vec<DesktopIcon>, String> {
    Ok(Vec::new())
}

fn desktop_target_key(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn desktop_icon_for_target(target: &str) -> Result<DesktopIcon, String> {
    let target = desktop_target_key(target);
    if target.is_empty() {
        return Err("桌面快捷方式名称为空。".to_string());
    }
    let icons = desktop_icons()?;
    let exact = icons
        .iter()
        .filter(|icon| desktop_target_key(&icon.name) == target)
        .cloned()
        .collect::<Vec<_>>();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }
    let partial = icons
        .iter()
        .filter(|icon| {
            let name = desktop_target_key(&icon.name);
            name.contains(&target) || target.contains(&name)
        })
        .cloned()
        .collect::<Vec<_>>();
    match partial.len() {
        1 => Ok(partial[0].clone()),
        0 => Err(format!("桌面上没有找到“{target}”快捷方式。")),
        _ => Err(format!(
            "桌面上有多个与“{target}”相近的快捷方式，请让 AI 使用完整名称。"
        )),
    }
}

#[cfg(windows)]
fn desktop_shortcut_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut add_directory = |directory: PathBuf| {
        if directory.is_dir() && !directories.contains(&directory) {
            directories.push(directory);
        }
    };

    if let Some(profile) = std::env::var_os("USERPROFILE") {
        add_directory(PathBuf::from(profile).join("Desktop"));
    }
    for variable in ["OneDrive", "OneDriveConsumer"] {
        if let Some(one_drive) = std::env::var_os(variable) {
            add_directory(PathBuf::from(one_drive).join("Desktop"));
        }
    }
    if let Some(public) = std::env::var_os("PUBLIC") {
        add_directory(PathBuf::from(public).join("Desktop"));
    }
    directories
}

#[cfg(windows)]
fn desktop_shortcut_for_target(target: &str) -> Result<PathBuf, String> {
    let target_key = desktop_target_key(target);
    if target_key.is_empty() {
        return Err("桌面快捷方式名称为空。".to_string());
    }

    let mut exact_matches = Vec::new();
    let mut partial_matches = Vec::new();
    for directory in desktop_shortcut_directories() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_shortcut = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("lnk") || extension.eq_ignore_ascii_case("url")
                });
            if !is_shortcut {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let shortcut_key = desktop_target_key(stem);
            if shortcut_key == target_key {
                exact_matches.push(path);
            } else if shortcut_key.contains(&target_key) || target_key.contains(&shortcut_key) {
                partial_matches.push(path);
            }
        }
    }

    let matches = if exact_matches.is_empty() {
        partial_matches
    } else {
        exact_matches
    };
    match matches.len() {
        1 => Ok(matches.into_iter().next().expect("shortcut match exists")),
        0 => Err(format!("在桌面目录中没有找到“{target}”的快捷方式文件。")),
        _ => Err(format!(
            "桌面目录中有多个与“{target}”相近的快捷方式，请使用完整名称。"
        )),
    }
}

#[cfg(windows)]
fn launch_desktop_shortcut_in_background(target: &str) -> Result<PathBuf, String> {
    use std::os::windows::process::CommandExt;

    let shortcut = desktop_shortcut_for_target(target)?;
    let escaped_path = shortcut.to_string_lossy().replace('\'', "''");
    let command = format!("Start-Process -FilePath '{escaped_path}'");
    Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
        ])
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // CREATE_NO_WINDOW: do not flash a console while PowerShell starts.
        .creation_flags(0x0800_0000)
        .spawn()
        .map_err(|error| format!("无法在后台启动桌面快捷方式: {error}"))?;
    trace_computer_control(&format!(
        "desktop shortcut launched through hidden powershell target={target:?} path={}",
        shortcut.display()
    ));
    Ok(shortcut)
}

fn normalize_pointer_coordinates(
    x: f64,
    y: f64,
    image_width: f64,
    image_height: f64,
) -> (f64, f64) {
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
        return (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
    }
    if x <= 1.0 && y <= 1.0 {
        return (x, y);
    }
    // A slightly out-of-range fractional value is a malformed normalized
    // coordinate, not a percentage or a screenshot pixel position.
    if x <= 2.0 || y <= 2.0 {
        return (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
    }

    // Compatible providers sometimes use percentages or screenshot pixels.
    // Normalize them rather than silently treating every value above 1 as 1.
    if x <= 100.0 && y <= 100.0 {
        return ((x / 100.0).clamp(0.0, 1.0), (y / 100.0).clamp(0.0, 1.0));
    }

    (
        (x / image_width.max(1.0)).clamp(0.0, 1.0),
        (y / image_height.max(1.0)).clamp(0.0, 1.0),
    )
}

fn computer_control_capture_dimensions() -> (f64, f64) {
    const MAX_WIDTH: f64 = 2048.0;
    const MAX_HEIGHT: f64 = 1280.0;
    let Ok(screen) = Screen::from_point(0, 0) else {
        return (MAX_WIDTH, MAX_HEIGHT);
    };
    let info = screen.display_info;
    let source_width = (info.width as f64 * info.scale_factor as f64).max(1.0);
    let source_height = (info.height as f64 * info.scale_factor as f64).max(1.0);
    let scale = (MAX_WIDTH / source_width).min(MAX_HEIGHT / source_height);
    (source_width * scale, source_height * scale)
}

fn first_complete_json_object(value: &str) -> Option<&str> {
    let start = value.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in value[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&value[start..start + offset + character.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_computer_action(value: &str) -> Result<ComputerAction, String> {
    let trimmed = value
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let json_text = first_complete_json_object(trimmed)
        .ok_or_else(|| "模型没有返回有效的电脑操作指令。".to_string())?;
    let mut action: ComputerAction = serde_json::from_str(json_text)
        .map_err(|error| format!("无法解析电脑操作指令: {error}"))?;
    let normalized_action = action
        .action
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
    action.action = match normalized_action.as_str() {
        "doubleclick" | "double_click" | "open_desktop_shortcut" => "double_click",
        "rightclick" | "right_click" => "right_click",
        "longpress" | "long_press" | "press_and_hold" => "long_press",
        "dragto" | "drag_to" | "drag" | "swipe" => "drag",
        "holdkeys" | "hold_keys" | "key_hold" => "hold_keys",
        "leftclick" | "left_click" | "singleclick" | "single_click" | "select_desktop_shortcut" => {
            "click"
        }
        "openapp" => "open_app",
        "activatewindow" | "activate_window" | "focuswindow" | "focus_window" => "activate_window",
        "maximizewindow" | "maximize_window" | "maximisewindow" | "maximise_window" => {
            "maximize_window"
        }
        "createfolder" | "create_folder" | "new_folder" => "create_folder",
        _ => normalized_action.as_str(),
    }
    .to_string();
    action.ui_action = action.ui_action.take().map(|value| {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "setvalue" | "set_value" | "value" => "set_value".to_string(),
            "select" => "select".to_string(),
            "focus" => "focus".to_string(),
            _ => "invoke".to_string(),
        }
    });
    if action.desktop_target.is_none()
        && action.x.is_none()
        && action.y.is_none()
        && matches!(action.action.as_str(), "click" | "double_click")
    {
        if let Some(target) = action.target.as_deref() {
            if desktop_icon_for_target(target).is_ok() {
                action.desktop_target = Some(target.to_string());
            }
        }
    }
    if let (Some(x), Some(y)) = (action.x, action.y) {
        let (image_width, image_height) = computer_control_capture_dimensions();
        let (x, y) = normalize_pointer_coordinates(x, y, image_width, image_height);
        action.x = Some(x);
        action.y = Some(y);
    } else {
        action.x = action.x.map(|value| value.clamp(0.0, 1.0));
        action.y = action.y.map(|value| value.clamp(0.0, 1.0));
    }
    if let (Some(x), Some(y)) = (action.end_x, action.end_y) {
        let (image_width, image_height) = computer_control_capture_dimensions();
        let (x, y) = normalize_pointer_coordinates(x, y, image_width, image_height);
        action.end_x = Some(x);
        action.end_y = Some(y);
    } else {
        action.end_x = action.end_x.map(|value| value.clamp(0.0, 1.0));
        action.end_y = action.end_y.map(|value| value.clamp(0.0, 1.0));
    }
    action.confidence = action.confidence.map(|value| value.clamp(0.0, 1.0));
    let risk_text = format!(
        "{} {} {}",
        action.description.as_deref().unwrap_or_default(),
        action.text.as_deref().unwrap_or_default(),
        action.message.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();
    if [
        "发送",
        "send",
        "删除",
        "支付",
        "付款",
        "购买",
        "密码",
        "验证码",
        "授权",
        "发布",
        "提交",
        "转账",
    ]
    .iter()
    .any(|keyword| risk_text.contains(keyword))
    {
        action.requires_confirmation = true;
    }
    Ok(action)
}

fn desktop_folder_name_from_task(task: &str) -> Option<String> {
    let request = task
        .split("[APPROVED EXECUTION PLAN]")
        .next()
        .unwrap_or(task)
        .trim();
    if !request.contains("桌面") || !request.contains("文件夹") {
        return None;
    }
    for marker in ["名字叫", "名称为", "命名为", "命名成", "名为", "叫"] {
        let Some((_, remainder)) = request.split_once(marker) else {
            continue;
        };
        let candidate = remainder
            .trim_start_matches(|character: char| {
                character.is_whitespace() || matches!(character, ':' | '：' | ',' | '，')
            })
            .split(|character: char| matches!(character, '\n' | '。' | '！' | '？' | ';' | '；'))
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches(|character| matches!(character, '"' | '\'' | '“' | '”' | '《' | '》'));
        let candidate = candidate
            .strip_suffix("的文件夹")
            .or_else(|| candidate.strip_suffix("文件夹"))
            .unwrap_or(candidate)
            .trim();
        if !candidate.is_empty() && candidate.chars().count() <= 80 {
            return Some(candidate.to_string());
        }
    }
    None
}

#[tauri::command]
async fn computer_next_action(
    app: AppHandle,
    task: String,
    history: Vec<String>,
) -> Result<ComputerAction, String> {
    if COMPUTER_CONTROL_STOPPED.load(Ordering::SeqCst) {
        return Err("电脑操控已停止。".to_string());
    }
    let config = load_config()?;
    if !config.computer_control_enabled {
        return Err("请先在设置中开启 AI 操控电脑。".to_string());
    }
    if !history
        .iter()
        .any(|entry| entry.contains("action=create_folder"))
    {
        if let Some(name) = desktop_folder_name_from_task(&task) {
            trace_computer_control(&format!(
                "desktop folder task routed directly to constrained PowerShell name={name:?}"
            ));
            return Ok(ComputerAction {
                action: "create_folder".to_string(),
                text: Some(name),
                description: Some("正在在桌面新建并命名文件夹".to_string()),
                expected_outcome: Some("桌面出现指定名称的文件夹".to_string()),
                confidence: Some(1.0),
                ..Default::default()
            });
        }
    }
    let request = ChatRequest {
        provider_id: None,
        messages: Vec::new(),
        attachments: Vec::new(),
        screen_image: None,
        web_search: false,
    };
    let (provider, key, _, _) = resolve_chat_provider(&request)?;
    if !matches!(provider.kind.as_str(), "openai" | "compatible") {
        return Err("当前电脑操控先支持 OpenAI 协议及其兼容中转站。".to_string());
    }
    let (mark, observation_sequence) = take_computer_mark_observation();
    let (screen_image, semantic_snapshot, desktop_index) =
        observe_computer_state(&app, mark).await?;
    if let Some(mark) = mark {
        if matches!(mark.kind, ComputerMarkKind::Mistake) {
            remember_mark_correction(mark, semantic_snapshot.as_ref());
        }
    }
    if mark.is_some() {
        let _ = app.emit_to("edge", "kero:computer-mark", json!({ "active": false }));
    }
    let mut history = history
        .into_iter()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    history.push_str("\n[CONTROL REVIEW RULE] Before every pointer action, re-locate the target in the newest screenshot. After every action, first inspect whether its expected visible result occurred before continuing. If a click missed, the pointer drifted, execution failed, or the result is uncertain, do not stop or repeat blindly: re-observe the screen, explain the changed plan through the next action description, and choose a corrected target.");
    if let Some(mark) = mark {
        let meaning = match mark.kind {
            ComputerMarkKind::Target => "TARGET: the user indicates the correct target is here; inspect this region first and prefer its relevant control.",
            ComputerMarkKind::Mistake => "MISTAKE: the user indicates this is a wrong or unsafe region; do not reuse this area or its prior target.",
        };
        history.push_str(&format!(
            "\n[USER MARK OVERRIDE] {meaning} Marked point=({:.3},{:.3}). This has priority over the prior plan and pointer target. Inspect the marked region and do not repeat the prior action blindly.",
            mark.x, mark.y
        ));
    }
    let focus_point = mark
        .map(|mark| (mark.x, mark.y))
        .or_else(|| last_history_point(&history));
    let screen_image = focus_point
        .and_then(|(x, y)| screen_with_focus_inset(&screen_image, x, y).ok())
        .unwrap_or(screen_image);
    let system = r#"你是 Kero 的 Windows 电脑操作代理。观察最新屏幕截图，每次只返回一个 JSON 动作，不能输出 Markdown 或解释。截图采集时 Kero 的胶囊、对话、设置和光效窗口都会被隐藏；即使由于系统合成延迟意外看到 Kero 自己的半透明界面、输入框或状态文字，也必须忽略它们，绝不能点击或把它们当成任务目标。
可用动作：
click: {"action":"click","x":0到1,"y":0到1,"description":"...","requiresConfirmation":false}
double_click: {"action":"double_click","x":0到1,"y":0到1,"description":"...","requiresConfirmation":false}
桌面快捷方式双击: {"action":"double_click","desktopTarget":"桌面索引中的完整名称","description":"双击桌面快捷方式...","requiresConfirmation":false}
激活已打开窗口: {"action":"activate_window","windowTarget":"可见窗口清单中的 id","description":"激活已打开的...","expectedOutcome":"目标窗口成为前台窗口"}
最大化已打开窗口: {"action":"maximize_window","windowTarget":"可见窗口清单中的 id","description":"最大化已打开的...","expectedOutcome":"目标窗口最大化并位于前台"}
right_click: {"action":"right_click","x":0到1,"y":0到1,"description":"...","requiresConfirmation":false}
type: {"action":"type","text":"要输入的完整文字","description":"..."}
key: {"action":"key","key":"enter|tab|escape|backspace|up|down|left|right|space","description":"..."}
hotkey: {"action":"hotkey","keys":["ctrl","l"],"description":"..."}
scroll: {"action":"scroll","amount":正数向上负数向下,"description":"..."}
wait: {"action":"wait","amount":毫秒数,"description":"等待界面"}
done: {"action":"done","message":"任务完成说明"}
桌面新建文件夹兜底: {"action":"create_folder","text":"文件夹名称","description":"桌面图形操作已连续失败，使用后台 PowerShell 新建并命名文件夹"}
鼠标规则：需要打开 Windows 右键菜单、网页右键菜单或图标上下文菜单时必须用 right_click。每个鼠标动作都只针对截图中清楚可见的目标。
桌面快捷方式规则（最高优先级）：用户要求从桌面打开软件时，优先使用下方“实时桌面索引”中完全一致的名称，返回 double_click 并填 desktopTarget。desktopTarget 存在时，Kero 会先按实际图标位置执行真实双击；若窗口没有出现，会自动在后台通过 PowerShell 启动对应快捷方式，因此不要估计 x/y。若索引没有目标，才可以根据截图寻找；名称模糊、相近名称有多个或索引没有目标时，不要猜测，使用菜单兜底。
已打开窗口规则（高于桌面快捷方式）：每次先检查 [VISIBLE WINDOWS]。只要目标应用的窗口已存在，即使它最小化、被遮挡、移到屏幕边缘或 visibleFraction 很小，也不得关闭当前窗口、回桌面、重新双击快捷方式或重新启动应用。目标窗口只露出少量边缘、最小化或不便操作时，优先用 maximize_window；窗口尺寸合适但未在前台时用 activate_window。windowTarget 必须使用清单中的精确 id，不要猜坐标。
系统托盘规则（高于桌面快捷方式和重新启动）：如果 [VISIBLE WINDOWS] 中没有目标应用，必须继续检查 [SYSTEM TRAY CONTROLS]。如果托盘清单已列出目标应用图标，使用该行给出的 point 单击以恢复窗口；只有界面明确需要双击时才双击。若清单中只有“显示隐藏的图标”“Show hidden icons”或同义入口，先按其 point 单击展开托盘，下一步重新截图观察，再从 tray-overflow 中寻找目标应用。只要目标应用在托盘中，就不得回桌面、搜索、关闭其他窗口或重新启动应用。只有 [VISIBLE WINDOWS] 和 [SYSTEM TRAY CONTROLS] 都确认没有目标后，才允许查找桌面快捷方式或使用 open_app。不得为了寻找托盘进程而关闭当前正常窗口。
打开软件规则：先从当前截图中用鼠标寻找桌面上的目标快捷方式或已可见的应用图标。当前未显示桌面时，只能点击截图里看得见的鼠标控件来回到桌面或最小化窗口，再重新观察；在确认桌面没有目标快捷方式前，绝对不能使用 Windows 键、Win+D、开始菜单、任务栏搜索、命令行，或通过输入应用名称来启动软件。若已经在桌面上仔细观察、确认没有目标快捷方式，才可以返回 {"action":"open_app","text":"应用名称","description":"桌面未发现快捷方式，正在使用开始菜单打开"} 作为兜底。不要在未确认桌面图标缺失时返回 open_app。
桌面文件夹规则（强制）：桌面新建后的重命名输入框很小，视觉模型不得猜测它。用户明确要求在 Windows 桌面新建并命名文件夹时，第一步必须返回 create_folder，不能返回 right_click、click、double_click、hotkey、type 或键盘快捷键；唯一例外是用户明确要求演示图形界面过程。text 必须是用户要求的纯文件夹名称，不能包含路径或命令。Kero 会在后台使用受限 PowerShell 创建它，绝不能将 create_folder 用于其他目录或其他类型的文件操作。
如果发送消息、提交/发布内容、删除、支付、转账、输入密码或验证码、授予权限，执行最终点击或 Enter 前必须把 requiresConfirmation 设为 true。不要仅凭记忆猜坐标；截图不清楚时先 wait。完成后必须返回 done。"#;
    let system = format!("{system}\n\nAdditional input actions for games and editing: drag: {{\"action\":\"drag\",\"x\":0.2,\"y\":0.4,\"endX\":0.8,\"endY\":0.4,\"duration\":700,\"description\":\"...\"}}. long_press: {{\"action\":\"long_press\",\"x\":0.5,\"y\":0.5,\"duration\":900,\"description\":\"...\"}}. hold_keys: {{\"action\":\"hold_keys\",\"keys\":[\"w\",\"shift\"],\"duration\":650,\"description\":\"...\"}}. Use hotkey for instantaneous editor shortcuts such as ctrl+z, ctrl+shift+s, ctrl+alt+delete, F1-F24, Home, End, Delete, PageUp and PageDown. Use drag for timeline clips, sliders, canvas selection, or game aiming; use hold_keys for simultaneous movement or sprint/jump controls. Use one action per response, re-observe after every drag or held input, and never claim success without visual confirmation.");
    let system = format!("{system}\nFor game tasks, classify the newest screenshot before choosing input. Treat a full-screen 2D or 3D scene with a player character, HUD, minimap, health bar, dialogue, level objective, or combat view as a game even when its title is not visible. If it is a typical Windows keyboard-and-mouse character game (a player-character, combat, exploration, or HUD scene without visible touch controls), treat WASD as the default movement hypothesis. Use a short 300-450ms hold_keys only for the FIRST input that verifies movement, for example {{\"action\":\"hold_keys\",\"keys\":[\"w\"],\"duration\":350,\"description\":\"briefly verify forward movement\"}}. Once the screenshot confirms that WASD works and the task is to keep walking, move a meaningful distance with a continuous 1200-2000ms hold_keys action, for example {{\"action\":\"hold_keys\",\"keys\":[\"w\"],\"duration\":1500,\"description\":\"continue walking forward\"}}; after that, inspect the new screenshot and repeat another continuous hold only if more travel is needed. Do not turn continuous movement into many repeated taps. Use Shift with W only when sprinting is needed, and use Space only when jumping is needed. Do not use WASD when the screen visibly shows arrow-key prompts, touch controls, click-to-move, racing controls, rhythm controls, or another explicit scheme; follow the visible scheme instead. Re-observe after every gameplay action and adapt from the new screenshot. For video editors and timelines, likewise choose actions from the visible UI rather than guessing.");
    let system = format!("{system}\nReliability contract: every pointer action must include confidence (0 to 1), description naming the visible target, and expectedOutcome describing the exact visible change expected after execution. Example: {{\"action\":\"click\",\"x\":0.72,\"y\":0.31,\"description\":\"click the visible blue Save button in the dialog footer\",\"confidence\":0.91,\"expectedOutcome\":\"the dialog closes and a saved-state indicator appears\"}}. Do not click a visually ambiguous target with confidence below 0.68: return a non-destructive wait action, or use a clearly visible control that exposes the correct target. On the screenshot following every action, explicitly compare the actual state with the prior expectedOutcome before selecting the next action. If the expectation was not met, identify why, select a different target or method, and never repeat the same coordinates merely because they were used before. A repeat of the same pointer target is allowed only when the newest screenshot visibly proves the earlier action did not work; include retryEvidence with that exact visible proof, for example \"the Save dialog is still open and the button remains enabled\". Kero permits at most ten attempts per target. For keyboard, drag, and game inputs, still provide expectedOutcome and choose a conservative duration that permits a new screenshot review.");
    let system = format!("{system}\nWindows semantic controls: the user text may contain a [WINDOW SEMANTICS] control list. For a normal application, prefer a listed control over guessed screenshot coordinates. When selecting a listed control, set uiTarget to its exact id and still include x/y from that same line for visual confirmation. Set uiAction to invoke for a button/link, setValue for a writable Edit, or select for a list/tab item. Kero re-resolves uiTarget against the live Windows accessibility tree immediately before clicking, so never invent an id. Browser skill: prefer named address, search, tab, download, and page controls. Explorer or file-dialog skill: prefer named navigation, filename, Open, Save, Cancel, and folder controls. Editor skill: prefer named timeline, track, export, save, undo, and dialog controls. When no semantic control is exposed, use the screenshot and retain the same review contract. Desktop launch boundary: desktopTarget is permitted only when the newest screenshot visibly shows the Windows desktop and the target shortcut is visible. Once the history says an app was opened or the newest screenshot shows any application window, never use desktopTarget again for that app; inspect and control the currently open application instead.");
    let system = format!("{system}\nK-mark correction rule: when the history contains [USER MARK OVERRIDE], the marked region is the user's explicit correction. The current screenshot's white-framed high-resolution inset is centered on that mark. Inspect the inset and nearby semantic controls first, discard the obsolete target, and choose a new action that directly addresses the marked region. Do not return done, wait indefinitely, or reuse the prior target until the correction has been considered.");
    let system = format!("{system}\nK-mark semantics: a TARGET mark means the correct control or destination is near the mark, so prioritize the marked region. A MISTAKE mark means the marked control/region was wrong, so avoid it and select a different control or method. Both kinds cancel any action proposed before the mark.");
    let system = format!("{system}\nAdaptive execution rule: before proposing every action, inspect the newest screenshot and the expectedOutcome of the preceding action. Do not create or follow a fixed multi-step plan. If the user's requested result is already visibly complete, return {{\"action\":\"done\",\"message\":\"...\",\"finalEvidence\":\"the exact visible proof that the user's requested result is complete\"}} immediately. The finalEvidence field is mandatory for done, including when the task was already complete before the first action. If the visible state differs from the prior expectation, discard that route and choose the best next action from the newest screen. Never click, submit, send, open, close, or select the same visible control twice in succession after its expected result has appeared. A repeated action is allowed only when the newest screenshot visibly proves that the prior action did not take effect, and must include retryEvidence describing that proof. planStepId and stepComplete are not needed.");
    let desktop_index = if desktop_index.is_empty() {
        "（当前无法读取桌面快捷方式索引）".to_string()
    } else {
        desktop_index
    };
    let semantic_text = semantic_snapshot
        .as_ref()
        .map(semantic_snapshot_text)
        .unwrap_or_else(|| {
            "[WINDOW SEMANTICS] unavailable; use visual grounding only.".to_string()
        });
    let correction_text = remembered_correction_text(semantic_snapshot.as_ref());
    let desktop_index = format!(
        "{desktop_index}\n\n{semantic_text}\n{correction_text}\n[VISUAL REVIEW] When a previous pointer point exists, the current screenshot includes a white-framed high-resolution inset around that point."
    );
    let mark_instruction = mark.map(|mark| format!(
        "用户刚刚按住 K 标记了屏幕坐标 ({:.3}, {:.3})。截图中该处有蓝白十字圆环，表示用户认为此前点击错误或希望你重点检查的位置。必须先观察该标记附近并据此纠正下一步；不要重复错误操作。",
        mark.x,
        mark.y,
    )).unwrap_or_default();
    let user_text = format!("用户任务：{task}\n已执行步骤：\n{history}\n{mark_instruction}\n实时桌面快捷方式与可见窗口清单（打开或恢复软件前必须先检查）：\n{desktop_index}\n请根据当前截图给出下一步。屏幕坐标必须使用 0 到 1 的比例。 ");
    let url = endpoint(
        &provider.base_url,
        "https://api.openai.com",
        "/v1/chat/completions",
    );
    let visual_content = vec![
        json!({ "type": "text", "text": user_text }),
        json!({ "type": "image_url", "image_url": { "url": screen_image, "detail": "high" } }),
    ];
    let payload = json!({
        "model": provider.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": visual_content }
        ],
        "stream": false,
        "max_tokens": 500
    });
    let response = shared_http_client()
        .post(url)
        .bearer_auth(key)
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("无法连接电脑操控模型: {error}"))?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| format!("无法读取电脑操控模型响应: {error}"))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    let content = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| "电脑操控模型没有返回动作。".to_string())?;
    let mut action = parse_computer_action(content)?;
    action.observation_sequence = Some(observation_sequence);
    trace_computer_control(&format!(
        "planned action={} desktop_target={:?} window_target={:?} x={:?} y={:?} observation_sequence={observation_sequence}",
        action.action, action.desktop_target, action.window_target, action.x, action.y
    ));
    Ok(action)
}

#[tauri::command]
fn selected_text_from_active() -> Result<String, String> {
    dictation::selected_text_from_active()
}

#[tauri::command]
fn replace_selected_text(text: String) -> Result<(), String> {
    dictation::replace_selected_text(text)
}

#[tauri::command]
fn insert_text_to_active(text: String) -> Result<(), String> {
    dictation::insert_text_to_active(text)
}

#[tauri::command]
fn replace_realtime_dictation_text(previous: String, text: String) -> Result<(), String> {
    dictation::replace_realtime_dictation_text(previous, text)
}

#[tauri::command]
fn clear_dictation_focus_target() {
    DICTATION_ACTIVE.store(false, Ordering::SeqCst);
    dictation::clear_focus_target();
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkArea {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

/// 主显示器工作区（去掉任务栏），用于把听写胶囊定位到屏幕底部居中。
#[cfg(windows)]
#[tauri::command]
fn get_work_area() -> Result<WorkArea, String> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};
    unsafe {
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let ok = SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut rect as *mut RECT as *mut core::ffi::c_void, 0);
        if ok == 0 {
            return Err("无法获取屏幕工作区".to_string());
        }
        Ok(WorkArea {
            x: rect.left,
            y: rect.top,
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
        })
    }
}

#[cfg(not(windows))]
#[tauri::command]
fn get_work_area() -> Result<WorkArea, String> {
    Err("仅支持 Windows".to_string())
}

/// 前端在模式/模型变化时同步：只有确认使用流式模型才允许 Alt 按下时预建连接。
#[tauri::command]
fn set_realtime_preconnect_hint(enabled: bool, vocabulary: Option<String>) {
    let previous_enabled = REALTIME_PRECONNECT_HINT.swap(enabled, Ordering::SeqCst);
    let previous_vocabulary = realtime_preconnect_vocabulary();
    if let Some(vocabulary) = vocabulary {
        let compact = vocabulary
            .split(['\n', ',', ';', '，', '；', '、'])
            .map(str::trim)
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>()
            .join(";");
        if let Ok(mut stored) = REALTIME_VOCABULARY.lock() {
            *stored = Some(compact.chars().take(400).collect());
        }
    }
    if previous_enabled != enabled || previous_vocabulary != realtime_preconnect_vocabulary() {
        invalidate_realtime_preconnect();
    }
}

#[cfg(windows)]
fn tap_virtual_key(key: u8) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP};
    unsafe {
        keybd_event(key, 0, 0, 0);
        keybd_event(key, 0, KEYEVENTF_KEYUP, 0);
    }
}

#[cfg(windows)]
fn virtual_key(name: &str) -> Option<u8> {
    match name.trim().to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(0x0d),
        "tab" => Some(0x09),
        "escape" | "esc" => Some(0x1b),
        "backspace" => Some(0x08),
        "space" => Some(0x20),
        "up" => Some(0x26),
        "down" => Some(0x28),
        "left" => Some(0x25),
        "right" => Some(0x27),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" | "page_up" => Some(0x21),
        "pagedown" | "page_down" => Some(0x22),
        "delete" | "del" => Some(0x2e),
        "insert" => Some(0x2d),
        "pause" => Some(0x13),
        "printscreen" | "print_screen" => Some(0x2c),
        "ctrl" | "control" => Some(0x11),
        "shift" => Some(0x10),
        "alt" => Some(0x12),
        "win" | "meta" => Some(0x5b),
        value if value.starts_with('f') => value[1..]
            .parse::<u8>()
            .ok()
            .filter(|number| (1..=24).contains(number))
            .map(|number| 0x70 + number - 1),
        value if value.len() == 1 => value.as_bytes().first().map(u8::to_ascii_uppercase),
        _ => None,
    }
}

#[cfg(windows)]
fn type_control_text(text: &str, fast: bool) -> Result<(), String> {
    // Native Unicode key events preserve the clipboard while remaining visibly readable.
    dictation::send_unicode_text_streamed(text, 4, if fast { 7 } else { 14 })
}

#[cfg(windows)]
fn primary_screen_physical_bounds() -> Result<(i32, i32, i32, i32), String> {
    let screen = Screen::from_point(0, 0).map_err(|error| format!("无法读取主屏幕: {error}"))?;
    let info = screen.display_info;
    let scale = info.scale_factor as f64;
    let left = (info.x as f64 * scale).round() as i32;
    let top = (info.y as f64 * scale).round() as i32;
    let width = (info.width as f64 * scale).round() as i32;
    let height = (info.height as f64 * scale).round() as i32;
    if width <= 0 || height <= 0 {
        return Err("主屏幕尺寸无效。".to_string());
    }
    Ok((left, top, width, height))
}

fn pointer_target_for_screen(
    x: f64,
    y: f64,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
) -> (i32, i32) {
    let target_x = left + (x.clamp(0.0, 1.0) * (width - 1).max(0) as f64).round() as i32;
    let target_y = top + (y.clamp(0.0, 1.0) * (height - 1).max(0) as f64).round() as i32;
    (target_x, target_y)
}

#[cfg(windows)]
fn smooth_move_pointer(x: f64, y: f64, fast: bool) -> Result<(i32, i32), String> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
    unsafe {
        let (left, top, width, height) = primary_screen_physical_bounds()?;
        let (target_x, target_y) = pointer_target_for_screen(x, y, left, top, width, height);
        let mut start = POINT {
            x: target_x,
            y: target_y,
        };
        GetCursorPos(&mut start);
        if (start.x - target_x).abs() <= 2 && (start.y - target_y).abs() <= 2 {
            trace_computer_control(&format!(
                "pointer already at target=({target_x},{target_y}); skipped movement"
            ));
            return Ok((target_x, target_y));
        }
        let steps = if fast { 10 } else { 24 };
        let interval_ms = if fast { 8 } else { 11 };
        for step in 1..=steps {
            if COMPUTER_CONTROL_STOPPED.load(Ordering::SeqCst) {
                return Err("电脑操控已停止。".to_string());
            }
            let t = step as f64 / steps as f64;
            let eased = 1.0 - (1.0 - t).powi(3);
            let px = start.x as f64 + (target_x - start.x) as f64 * eased;
            let py = start.y as f64 + (target_y - start.y) as f64 * eased;
            SetCursorPos(px.round() as i32, py.round() as i32);
            std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        }
        let settle_attempts = if fast { 1 } else { 8 };
        let settle_interval_ms = if fast { 3 } else { 18 };
        for _ in 0..settle_attempts {
            SetCursorPos(target_x, target_y);
            std::thread::sleep(std::time::Duration::from_millis(settle_interval_ms));
            let mut actual = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut actual) != 0
                && (actual.x - target_x).abs() <= 1
                && (actual.y - target_y).abs() <= 1
            {
                trace_computer_control(&format!(
                    "pointer arrived target=({target_x},{target_y}) actual=({},{})",
                    actual.x, actual.y
                ));
                return Ok((target_x, target_y));
            }
        }
        return Err(
            "POINTER_RECOVERY_REQUIRED: cursor did not settle at the requested target".to_string(),
        );
    }
}

#[cfg(windows)]
fn prime_pointer_target(target_x: i32, target_y: i32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

    unsafe {
        // The edge layer has just been hidden. Move away and back with absolute
        // coordinates so Explorer receives a fresh WM_MOUSEMOVE for this item.
        SetCursorPos(target_x.saturating_sub(2), target_y);
        std::thread::sleep(std::time::Duration::from_millis(28));
        SetCursorPos(target_x, target_y);
        std::thread::sleep(std::time::Duration::from_millis(62));
        let mut actual = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut actual) == 0
            || (actual.x - target_x).abs() > 1
            || (actual.y - target_y).abs() > 1
        {
            return Err("POINTER_RECOVERY_REQUIRED: cursor drifted before the click".to_string());
        }
        trace_computer_control(&format!(
            "desktop pointer primed target=({target_x},{target_y}) actual=({},{})",
            actual.x, actual.y
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn inject_pointer_double_click() -> Result<(), String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEINPUT,
    };

    let mouse_input = |flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let send = |flags| {
        let input = mouse_input(flags);
        let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
        (sent == 1)
            .then_some(())
            .ok_or_else(|| "Windows 未能注入完整的双击事件。".to_string())
    };
    send(MOUSEEVENTF_LEFTDOWN)?;
    std::thread::sleep(std::time::Duration::from_millis(36));
    send(MOUSEEVENTF_LEFTUP)?;
    std::thread::sleep(std::time::Duration::from_millis(82));
    send(MOUSEEVENTF_LEFTDOWN)?;
    std::thread::sleep(std::time::Duration::from_millis(36));
    send(MOUSEEVENTF_LEFTUP)?;
    trace_computer_control("timed SendInput double-click injected interval_ms=82");
    Ok(())
}

#[cfg(windows)]
fn desktop_target_is_mouse_reachable(target_x: i32, target_y: i32) -> bool {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsChild, WindowFromPoint};

    let Ok(list_view) = desktop_list_view() else {
        return false;
    };
    let hit = unsafe {
        WindowFromPoint(POINT {
            x: target_x,
            y: target_y,
        })
    };
    let reachable = hit == list_view || (!hit.is_null() && unsafe { IsChild(list_view, hit) } != 0);
    if !reachable {
        trace_computer_control(&format!(
            "desktop target occluded point=({target_x},{target_y}) hit={hit:?} list={list_view:?}"
        ));
    }
    reachable
}

#[cfg(windows)]
fn inject_desktop_list_view_double_click(target_x: i32, target_y: i32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageW, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    };

    const MK_LBUTTON: usize = 0x0001;
    let list_view = desktop_list_view()?;
    let mut point = POINT {
        x: target_x,
        y: target_y,
    };
    if unsafe { ScreenToClient(list_view, &mut point) } == 0 {
        return Err("无法把桌面图标坐标转换为 Explorer 坐标。".to_string());
    }
    let packed = ((point.y as u32 & 0xffff) << 16) | (point.x as u32 & 0xffff);
    let l_param = packed as isize;
    unsafe {
        SendMessageW(list_view, WM_LBUTTONDOWN, MK_LBUTTON, l_param);
    }
    std::thread::sleep(std::time::Duration::from_millis(36));
    unsafe {
        SendMessageW(list_view, WM_LBUTTONUP, 0, l_param);
    }
    std::thread::sleep(std::time::Duration::from_millis(82));
    unsafe {
        SendMessageW(list_view, WM_LBUTTONDBLCLK, MK_LBUTTON, l_param);
    }
    std::thread::sleep(std::time::Duration::from_millis(36));
    unsafe {
        SendMessageW(list_view, WM_LBUTTONUP, 0, l_param);
    }
    trace_computer_control(&format!(
        "Explorer desktop double-click delivered screen=({target_x},{target_y}) client=({},{})",
        point.x, point.y
    ));
    Ok(())
}

#[cfg(windows)]
fn inject_pointer_click(is_right_click: bool, is_double_click: bool) -> Result<(), String> {
    if is_double_click {
        return inject_pointer_double_click();
    }
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP,
    };

    let (down, up) = if is_right_click {
        (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
    } else {
        (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
    };
    let send_button_event = |flags| unsafe {
        mouse_event(flags, 0, 0, 0, 0);
    };

    send_button_event(down);
    std::thread::sleep(std::time::Duration::from_millis(58));
    send_button_event(up);
    Ok(())
}

#[cfg(windows)]
fn inject_long_press(x: f64, y: f64, duration_ms: i32) -> Result<(), String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    };

    smooth_move_pointer(x, y, false)?;
    unsafe {
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
    }
    std::thread::sleep(std::time::Duration::from_millis(
        duration_ms.clamp(120, 8_000) as u64,
    ));
    unsafe {
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
    }
    Ok(())
}

#[cfg(windows)]
fn inject_drag(
    start_x: f64,
    start_y: f64,
    end_x: f64,
    end_y: f64,
    duration_ms: i32,
) -> Result<(), String> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos;

    smooth_move_pointer(start_x, start_y, false)?;
    let (left, top, width, height) = primary_screen_physical_bounds()?;
    let (target_x, target_y) = pointer_target_for_screen(end_x, end_y, left, top, width, height);
    let mut start = POINT {
        x: target_x,
        y: target_y,
    };
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut start);
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
    }
    let duration = duration_ms.clamp(180, 8_000) as u64;
    let steps = (duration / 16).clamp(14, 120) as usize;
    for step in 1..=steps {
        if COMPUTER_CONTROL_STOPPED.load(Ordering::SeqCst) {
            unsafe {
                mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
            }
            return Err("Computer control stopped during drag".to_string());
        }
        let t = step as f64 / steps as f64;
        let eased = t * t * (3.0 - 2.0 * t);
        let x = start.x as f64 + (target_x - start.x) as f64 * eased;
        let y = start.y as f64 + (target_y - start.y) as f64 * eased;
        unsafe {
            SetCursorPos(x.round() as i32, y.round() as i32);
        }
        std::thread::sleep(std::time::Duration::from_millis(
            (duration / steps as u64).max(1),
        ));
    }
    unsafe {
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
    }
    Ok(())
}

#[cfg(windows)]
fn inject_held_keys(keys: &[String], duration_ms: i32) -> Result<(), String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP};

    let keys = keys
        .iter()
        .map(|key| virtual_key(key).ok_or_else(|| format!("Unsupported key: {key}")))
        .collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() {
        return Err("At least one key is required".to_string());
    }
    unsafe {
        for key in &keys {
            keybd_event(*key, 0, 0, 0);
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(
        duration_ms.clamp(60, 8_000) as u64,
    ));
    unsafe {
        for key in keys.iter().rev() {
            keybd_event(*key, 0, KEYEVENTF_KEYUP, 0);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn show_window_without_activation(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};

    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    unsafe {
        ShowWindow(hwnd.0 as _, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

#[cfg(windows)]
fn create_desktop_folder_with_powershell(name: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    let name = name.trim();
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.chars().any(|character| {
            matches!(
                character,
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        })
    {
        return Err("文件夹名称无效。".to_string());
    }
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference='Stop'; $name=$env:KERO_FOLDER_NAME; $desktop=[Environment]::GetFolderPath('Desktop'); New-Item -ItemType Directory -Force -Path (Join-Path -Path $desktop -ChildPath $name) | Out-Null",
        ])
        .env("KERO_FOLDER_NAME", name)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|error| format!("无法启动 PowerShell 创建文件夹: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        trace_computer_control(&format!(
            "PowerShell desktop folder failed status={} stderr={detail:?}",
            output.status
        ));
        return Err(format!(
            "PowerShell 创建桌面文件夹失败，退出码: {}; {detail}",
            output.status
        ));
    }
    Ok(())
}

#[tauri::command]
async fn computer_execute_action(
    app: AppHandle,
    action: ComputerAction,
    confirmed: Option<bool>,
) -> Result<String, String> {
    let mut action = action;
    if COMPUTER_CONTROL_STOPPED.load(Ordering::SeqCst) {
        return Err("电脑操控已停止。".to_string());
    }
    if action
        .observation_sequence
        .is_some_and(|sequence| sequence != COMPUTER_MARK_SEQUENCE.load(Ordering::SeqCst))
    {
        trace_computer_control(&format!(
            "stale action rejected observation_sequence={:?} current_sequence={}",
            action.observation_sequence,
            COMPUTER_MARK_SEQUENCE.load(Ordering::SeqCst)
        ));
        return Err("USER_MARK_REOBSERVE_REQUIRED".to_string());
    }
    trace_computer_control(&format!(
        "execute requested action={} desktop_target={:?} window_target={:?} x={:?} y={:?}",
        action.action, action.desktop_target, action.window_target, action.x, action.y
    ));
    if action.requires_confirmation
        && !load_config()?.computer_control_risk_mode
        && confirmed != Some(true)
    {
        return Err("此操作需要用户确认。".to_string());
    }
    #[cfg(windows)]
    if matches!(
        action.action.as_str(),
        "activate_window" | "maximize_window"
    ) {
        let target = action
            .window_target
            .as_deref()
            .ok_or_else(|| "缺少要激活的窗口标识。".to_string())?;
        let route = focus_or_maximize_window(target, action.action == "maximize_window")?;
        std::thread::sleep(std::time::Duration::from_millis(180));
        trace_computer_control(&format!(
            "existing window activated action={} target={target:?} route={route}",
            action.action
        ));
        return Ok(route.to_string());
    }
    #[cfg(windows)]
    if action.ui_target.is_some()
        && matches!(action.action.as_str(), "click" | "double_click" | "type")
    {
        match try_semantic_control_action(&action) {
            Ok(Some(route)) if route != "uia_not_applicable" && route != "uia_focus_for_type" => {
                trace_computer_control(&format!(
                    "semantic control complete action={} target={:?} route={route}",
                    action.action, action.ui_target
                ));
                let label = action
                    .description
                    .clone()
                    .unwrap_or_else(|| action.action.clone());
                return Ok(format!("{label} [{route}]"));
            }
            Ok(Some("uia_focus_for_type")) => {
                trace_computer_control(&format!(
                    "semantic input focused before native streamed typing target={:?}",
                    action.ui_target
                ));
            }
            Ok(_) => {}
            Err(error) => {
                // UI Automation is preferred for ordinary controls, but modern
                // canvas/web controls may not expose the required pattern.
                trace_computer_control(&format!(
                    "semantic control fallback action={} target={:?} error={error}",
                    action.action, action.ui_target
                ));
                if action.action == "type" {
                    if let Some(target) = action.ui_target.as_deref() {
                        if let Ok(element) = semantic_control_element(target) {
                            let _ = element.set_focus();
                        }
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    if matches!(
        action.action.as_str(),
        "move" | "click" | "double_click" | "right_click" | "long_press" | "drag"
    ) {
        if let Some(target) = action.ui_target.as_deref() {
            let (x, y) = semantic_target_point(target)?;
            trace_computer_control(&format!(
                "semantic target resolved id={target} normalized=({x:.4},{y:.4})"
            ));
            action.x = Some(x);
            action.y = Some(y);
        }
    }
    let description = action
        .description
        .clone()
        .unwrap_or_else(|| action.action.clone());
    #[cfg(windows)]
    match action.action.as_str() {
        "move" => {
            let x = action.x.ok_or_else(|| "Pointer move needs x".to_string())?;
            let y = action.y.ok_or_else(|| "Pointer move needs y".to_string())?;
            app.emit_to(
                "edge",
                "kero:computer-pointer",
                json!({ "x": x, "y": y, "active": true, "click": false }),
            )
            .ok();
            smooth_move_pointer(x, y, action.fast)?;
        }
        "create_folder" => {
            let name = action
                .text
                .as_deref()
                .ok_or_else(|| "缺少文件夹名称。".to_string())?;
            trace_computer_control(&format!(
                "PowerShell desktop folder requested name={name:?}"
            ));
            create_desktop_folder_with_powershell(name)?;
            trace_computer_control(&format!(
                "desktop folder created with PowerShell name={name:?}"
            ));
        }
        "open_app" => {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP};
            let name = action
                .text
                .as_deref()
                .ok_or_else(|| "缺少应用名称。".to_string())?;
            unsafe {
                keybd_event(0x5b, 0, 0, 0);
                keybd_event(0x5b, 0, KEYEVENTF_KEYUP, 0);
            }
            tokio::time::sleep(std::time::Duration::from_millis(260)).await;
            type_control_text(name, action.fast)?;
            tokio::time::sleep(std::time::Duration::from_millis(420)).await;
            tap_virtual_key(0x0d);
        }
        "click" | "double_click" | "right_click" => {
            let is_desktop_target = action.desktop_target.is_some();
            let (x, y) = if let Some(target) = action.desktop_target.as_deref() {
                let icon = desktop_icon_for_target(target)?;
                (icon.x, icon.y)
            } else {
                (
                    action.x.ok_or_else(|| "缺少鼠标横坐标。".to_string())?,
                    action.y.ok_or_else(|| "缺少鼠标纵坐标。".to_string())?,
                )
            };
            app.emit_to(
                "edge",
                "kero:computer-pointer",
                json!({ "x": x, "y": y, "active": true, "click": false }),
            )
            .ok();
            let (target_x, target_y) = match smooth_move_pointer(x, y, action.fast) {
                Ok(position) => position,
                Err(error) if is_desktop_target && action.action == "double_click" => {
                    let target = action
                        .desktop_target
                        .as_deref()
                        .ok_or_else(|| "缺少桌面快捷方式名称。".to_string())?;
                    trace_computer_control(&format!(
                        "desktop pointer could not settle error={error:?}; using hidden shortcut fallback"
                    ));
                    let shortcut = launch_desktop_shortcut_in_background(target)?;
                    trace_computer_control(&format!(
                        "desktop shortcut fallback accepted after pointer failure path={}",
                        shortcut.display()
                    ));
                    return Ok(description);
                }
                Err(error) => return Err(error),
            };
            std::thread::sleep(std::time::Duration::from_millis(34));
            let route = if action.action == "right_click" {
                inject_pointer_click(true, false)?;
                "send_input_right"
            } else {
                let edge = app.get_webview_window("edge");
                let restore_edge = edge
                    .as_ref()
                    .and_then(|window| window.is_visible().ok())
                    .unwrap_or(false);
                if restore_edge {
                    edge.as_ref()
                        .unwrap()
                        .hide()
                        .map_err(|error| error.to_string())?;
                    std::thread::sleep(std::time::Duration::from_millis(38));
                }
                let click_result = if is_desktop_target && action.action == "double_click" {
                    let mut opened = false;
                    // Give Explorer one complete physical double-click first. Repeating
                    // an ignored synthetic click only adds several seconds of latency;
                    // the hidden shortcut launch below is the reliable fallback.
                    for attempt in 1..=1 {
                        trace_computer_control(&format!("desktop double-click attempt={attempt}"));
                        prime_pointer_target(target_x, target_y)?;
                        focus_desktop_list_view()?;
                        let previous_foreground = unsafe {
                            windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow()
                        };
                        if !desktop_target_is_mouse_reachable(target_x, target_y) {
                            inject_desktop_list_view_double_click(target_x, target_y)?;
                            if wait_for_application_window(
                                previous_foreground,
                                std::time::Duration::from_millis(1800),
                            ) {
                                trace_computer_control(&format!(
                                    "desktop application appeared through Explorer fallback attempt={attempt}"
                                ));
                                opened = true;
                                break;
                            }
                            trace_computer_control(
                                "Explorer fallback did not expose an application window; trying physical double-click",
                            );
                        }
                        POINTER_TRACE_ACTIVE.store(true, Ordering::SeqCst);
                        if attempt == 1 {
                            trace_computer_control("desktop selection click");
                            inject_pointer_click(false, false)?;
                            let double_click_time = unsafe {
                                windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime(
                                )
                            };
                            std::thread::sleep(std::time::Duration::from_millis(
                                double_click_time.clamp(250, 800) as u64 + 90,
                            ));
                            prime_pointer_target(target_x, target_y)?;
                        }
                        let result = inject_pointer_click(false, true);
                        POINTER_TRACE_ACTIVE.store(false, Ordering::SeqCst);
                        result?;
                        if restore_edge {
                            let window = edge.as_ref().unwrap();
                            window
                                .set_ignore_cursor_events(true)
                                .map_err(|error| error.to_string())?;
                            show_window_without_activation(window)?;
                            app.emit_to(
                                "edge",
                                "kero:computer-pointer",
                                json!({
                                    "x": x, "y": y, "active": true, "click": true
                                }),
                            )
                            .ok();
                        }
                        if wait_for_application_window(
                            previous_foreground,
                            std::time::Duration::from_millis(1800),
                        ) {
                            trace_computer_control(&format!(
                                "desktop application appeared attempt={attempt}"
                            ));
                            opened = true;
                            break;
                        }
                        if restore_edge && attempt < 3 {
                            edge.as_ref()
                                .unwrap()
                                .hide()
                                .map_err(|error| error.to_string())?;
                            std::thread::sleep(std::time::Duration::from_millis(45));
                        }
                    }
                    if !opened {
                        let target = action
                            .desktop_target
                            .as_deref()
                            .ok_or_else(|| "缺少桌面快捷方式名称。".to_string())?;
                        trace_computer_control(
                            "desktop double-click was not confirmed; using hidden shortcut fallback",
                        );
                        let shortcut = launch_desktop_shortcut_in_background(target)?;
                        trace_computer_control(&format!(
                            "desktop shortcut fallback accepted path={}",
                            shortcut.display()
                        ));
                    }
                    Ok(())
                } else {
                    if is_desktop_target {
                        focus_desktop_list_view()?;
                    }
                    POINTER_TRACE_ACTIVE.store(true, Ordering::SeqCst);
                    let result = inject_pointer_click(false, action.action == "double_click");
                    POINTER_TRACE_ACTIVE.store(false, Ordering::SeqCst);
                    result
                };
                let restore_result = if restore_edge {
                    let window = edge.as_ref().unwrap();
                    window
                        .set_ignore_cursor_events(true)
                        .map_err(|error| error.to_string())
                        .and_then(|_| show_window_without_activation(window))
                } else {
                    Ok(())
                };
                click_result?;
                restore_result?;
                if !(is_desktop_target && action.action == "double_click") {
                    app.emit_to(
                        "edge",
                        "kero:computer-pointer",
                        json!({
                            "x": x, "y": y, "active": true, "click": true
                        }),
                    )
                    .ok();
                }
                if action.action == "double_click" {
                    if is_desktop_target {
                        "desktop_double_click_retry_until_visible"
                    } else {
                        "mouse_event_double_click_edge_no_activate"
                    }
                } else if is_desktop_target {
                    "mouse_event_desktop_click_edge_no_activate"
                } else {
                    "mouse_event_left_click_edge_no_activate"
                }
            };
            trace_computer_control(&format!(
                "pointer complete action={} route={} normalized=({x:.4},{y:.4})",
                action.action, route
            ));
        }
        "type" => type_control_text(
            action
                .text
                .as_deref()
                .ok_or_else(|| "缺少输入文字。".to_string())?,
            action.fast,
        )?,
        "key" => tap_virtual_key(
            virtual_key(action.key.as_deref().unwrap_or_default())
                .ok_or_else(|| "不支持这个按键。".to_string())?,
        ),
        "hotkey" => {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP};
            let keys = action
                .keys
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter_map(|key| virtual_key(key))
                .collect::<Vec<_>>();
            if keys.is_empty() {
                return Err("缺少快捷键。".to_string());
            }
            unsafe {
                for key in &keys {
                    keybd_event(*key, 0, 0, 0);
                }
                for key in keys.iter().rev() {
                    keybd_event(*key, 0, KEYEVENTF_KEYUP, 0);
                }
            }
        }
        "hold_keys" => {
            let keys = action.keys.as_deref().unwrap_or_default();
            inject_held_keys(keys, action.duration.or(action.amount).unwrap_or(520))?;
        }
        "long_press" => {
            let x = action.x.ok_or_else(|| "Long press needs x".to_string())?;
            let y = action.y.ok_or_else(|| "Long press needs y".to_string())?;
            app.emit_to(
                "edge",
                "kero:computer-pointer",
                json!({ "x": x, "y": y, "active": true, "click": false }),
            )
            .ok();
            inject_long_press(x, y, action.duration.or(action.amount).unwrap_or(720))?;
            app.emit_to(
                "edge",
                "kero:computer-pointer",
                json!({ "x": x, "y": y, "active": true, "click": true }),
            )
            .ok();
        }
        "drag" => {
            let x = action.x.ok_or_else(|| "Drag needs x".to_string())?;
            let y = action.y.ok_or_else(|| "Drag needs y".to_string())?;
            let end_x = action.end_x.ok_or_else(|| "Drag needs endX".to_string())?;
            let end_y = action.end_y.ok_or_else(|| "Drag needs endY".to_string())?;
            app.emit_to(
                "edge",
                "kero:computer-pointer",
                json!({ "x": x, "y": y, "active": true, "click": false }),
            )
            .ok();
            inject_drag(
                x,
                y,
                end_x,
                end_y,
                action.duration.or(action.amount).unwrap_or(680),
            )?;
            app.emit_to(
                "edge",
                "kero:computer-pointer",
                json!({ "x": end_x, "y": end_y, "active": true, "click": true }),
            )
            .ok();
        }
        "scroll" => {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{mouse_event, MOUSEEVENTF_WHEEL};
            let delta = action.amount.unwrap_or(-3).clamp(-10, 10) * 120;
            unsafe {
                mouse_event(MOUSEEVENTF_WHEEL, 0, 0, delta, 0);
            }
        }
        "wait" => {
            tokio::time::sleep(std::time::Duration::from_millis(
                action.amount.unwrap_or(700).clamp(100, 5000) as u64,
            ))
            .await
        }
        "done" => {}
        _ => return Err(format!("不支持的电脑操作：{}", action.action)),
    }
    #[cfg(not(windows))]
    return Err("电脑操控目前仅支持 Windows。".to_string());
    trace_computer_control(&format!("execute complete action={}", action.action));
    Ok(description)
}

#[tauri::command]
async fn stream_dictation_transform(
    app: AppHandle,
    request_id: String,
    request: DictationTransformRequest,
) -> Result<(), String> {
    let source = dictation::normalize_dictation_punctuation(request.text.trim());
    if source.is_empty() {
        emit_stream_event(&app, &request_id, None, true, Some("没有可整理的听写内容".to_string()));
        return Ok(());
    }
    if source.chars().count() > 4_000 {
        emit_stream_event(&app, &request_id, None, true, Some("单次听写内容不能超过 4000 个字符".to_string()));
        return Ok(());
    }
    let translate_to_english = request.target_language.as_deref() == Some("en");
    let vocabulary = request
        .vocabulary
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .take(32)
        .collect::<Vec<_>>()
        .join("; ");
    let memory = request
        .memory
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join("\n");
    let instruction = if translate_to_english {
        format!(
            "You are a precise Chinese voice-input translator. Translate the user's Chinese into natural, concise English. Return only the English text with no explanation, title, quotes, Markdown, or trailing period. Preserve names, numbers, URLs, paths, commands, constraints, negation, and intent exactly. Use English half-width punctuation. Context terms: {vocabulary}."
        )
    } else {
        let correction = if request.correct_typos.unwrap_or(true) {
            "Restore punctuation, fix only high-confidence ASR homophones, typos, product names, and adjacent stutters."
        } else {
            "Only normalize punctuation, spaces, casing, and adjacent stutters; do not replace words."
        };
        format!(
            "You are a Chinese voice-input proofreader, not a chat assistant. Return only the cleaned original text without explanation, title, quotes, or Markdown. {correction} Use English half-width punctuation only. Do not put periods in the middle of Chinese sentences; use commas for clause boundaries. Do not add a trailing period. Preserve all numbers, names, URLs, paths, commands, negation, temporal order, and intent. Context terms: {vocabulary}. Recent dictation context for terminology only: {memory}."
        )
    };
    let request = ChatRequest {
        provider_id: None,
        messages: Vec::new(),
        attachments: Vec::new(),
        screen_image: None,
        web_search: false,
    };
    let result = async {
        let (provider, key, _, _) = resolve_chat_provider(&request)?;
        let client = shared_http_client();
        let messages = vec![
            ChatMessage { role: "system".to_string(), content: instruction },
            ChatMessage { role: "user".to_string(), content: source },
        ];
        match provider.kind.as_str() {
            "anthropic" => stream_anthropic(&client, &provider, &key, &messages, &app, &request_id, None).await,
            "google" => stream_google(&client, &provider, &key, &messages, &app, &request_id, None).await,
            _ => stream_openai(&client, &provider, &key, &messages, &app, &request_id, None).await,
        }
    }
    .await;
    match result {
        Ok(()) => emit_stream_event(&app, &request_id, None, true, None),
        Err(error) => emit_stream_event(&app, &request_id, None, true, Some(error)),
    }
    Ok(())
}

#[tauri::command]
async fn chat_stream(
    app: AppHandle,
    request_id: String,
    request: ChatRequest,
) -> Result<(), String> {
    let result = async {
        let (provider, key, system_prompt, context_enabled) = resolve_chat_provider(&request)?;
        let client = shared_http_client();
        let mut messages = with_system_prompt(&request.messages, &system_prompt, context_enabled);
        if request.screen_image.is_some() {
            messages.insert(0, ChatMessage {
                role: "system".to_string(),
                content: "你正在根据用户刚刚提供的一张电脑屏幕截图回答问题。请只回答用户的问题，不要逐字转录屏幕内容；使用简洁、正常的中文。若画面无法识别，请明确说明，不要猜测或输出重复字符。".to_string(),
            });
        }
        match provider.kind.as_str() {
            _ if request.web_search => stream_openai_web_search(&client, &provider, &key, &messages, &app, &request_id, request.screen_image.as_deref()).await,
            "anthropic" => stream_anthropic(&client, &provider, &key, &messages, &app, &request_id, request.screen_image.as_deref()).await,
            "google" => stream_google(&client, &provider, &key, &messages, &app, &request_id, request.screen_image.as_deref()).await,
            _ => stream_openai(&client, &provider, &key, &messages, &app, &request_id, request.screen_image.as_deref()).await,
        }
    }
    .await;

    match result {
        Ok(()) => emit_stream_event(&app, &request_id, None, true, None),
        Err(error) => emit_stream_event(&app, &request_id, None, true, Some(error)),
    }
    Ok(())
}

fn show_edge_on_main_monitor(app: &AppHandle) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口尚未就绪".to_string())?;
    let edge = app
        .get_webview_window("edge")
        .ok_or_else(|| "光效窗口尚未就绪".to_string())?;
    if let Some(monitor) = main.current_monitor().map_err(|error| error.to_string())? {
        edge.set_position(*monitor.position())
            .map_err(|error| error.to_string())?;
        edge.set_size(*monitor.size())
            .map_err(|error| error.to_string())?;
    }
    edge.set_ignore_cursor_events(true)
        .map_err(|error| error.to_string())?;
    edge.show().map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
fn activate_assistant(app: AppHandle) -> Result<(), String> {
    show_edge_on_main_monitor(&app)
}

#[tauri::command]
fn hide_edge(app: AppHandle) -> Result<(), String> {
    app.get_webview_window("edge")
        .ok_or_else(|| "光效窗口尚未就绪".to_string())?
        .hide()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn open_settings(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("settings")
        .ok_or_else(|| "设置窗口尚未就绪".to_string())?;
    window
        .set_size(LogicalSize::new(880.0, 700.0))
        .map_err(|error| error.to_string())?;
    window.center().map_err(|error| error.to_string())?;
    window.show().map_err(|error| error.to_string())?;
    window
        .set_skip_taskbar(false)
        .map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
fn normalize_settings_window(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("settings")
        .ok_or_else(|| "设置窗口尚未就绪".to_string())?;
    window
        .set_size(LogicalSize::new(880.0, 700.0))
        .map_err(|error| error.to_string())?;
    window.center().map_err(|error| error.to_string())
}

#[tauri::command]
fn open_chat(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("chat")
        .ok_or_else(|| "对话窗口尚未就绪".to_string())?;
    window.show().map_err(|error| error.to_string())?;
    window
        .set_skip_taskbar(false)
        .map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
fn set_chat_size(app: AppHandle, width: f64, height: f64) -> Result<(), String> {
    let window = app
        .get_webview_window("chat")
        .ok_or_else(|| "Chat window is not ready".to_string())?;
    if window.is_maximized().map_err(|error| error.to_string())? {
        return Ok(());
    }
    window
        .set_size(LogicalSize::new(
            width.clamp(610.0, 900.0),
            height.clamp(720.0, 920.0),
        ))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn toggle_chat_maximize(app: AppHandle) -> Result<bool, String> {
    let window = app
        .get_webview_window("chat")
        .ok_or_else(|| "Chat window is not ready".to_string())?;
    if window.is_maximized().map_err(|error| error.to_string())? {
        window.unmaximize().map_err(|error| error.to_string())?;
        Ok(false)
    } else {
        window.maximize().map_err(|error| error.to_string())?;
        Ok(true)
    }
}

fn restore_main(app: &AppHandle) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口尚未就绪".to_string())?;
    main.show().map_err(|error| error.to_string())?;
    main.set_skip_taskbar(true)
        .map_err(|error| error.to_string())?;
    main.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
fn show_main(app: AppHandle) -> Result<(), String> {
    restore_main(&app)
}

#[tauri::command]
fn show_main_passive(app: AppHandle) -> Result<bool, String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window is not ready".to_string())?;
    let was_hidden = !main.is_visible().map_err(|error| error.to_string())?;
    main.show().map_err(|error| error.to_string())?;
    main.set_skip_taskbar(true)
        .map_err(|error| error.to_string())?;
    Ok(was_hidden)
}

#[tauri::command]
fn minimize_to_tray(app: AppHandle) -> Result<(), String> {
    for label in ["edge", "chat", "settings", "main"] {
        if let Some(window) = app.get_webview_window(label) {
            window.hide().map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    request_app_exit(&app);
}

fn request_app_exit(app: &AppHandle) {
    EXIT_REQUESTED.store(true, Ordering::SeqCst);
    SCREEN_TRANSLATION_ACTIVE.store(false, Ordering::SeqCst);
    SCREEN_TRANSLATION_SEQUENCE.fetch_add(1, Ordering::SeqCst);
    trace_runtime("explicit exit requested");
    #[cfg(windows)]
    restore_computer_cursor();
    app.exit(0);
}

#[tauri::command]
fn hide_window(app: AppHandle, label: String) -> Result<(), String> {
    app.get_webview_window(&label)
        .ok_or_else(|| "窗口尚未就绪".to_string())?
        .hide()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn minimize_window(app: AppHandle, label: String) -> Result<(), String> {
    app.get_webview_window(&label)
        .ok_or_else(|| "窗口尚未就绪".to_string())?
        .minimize()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_main_click_through(app: AppHandle, enabled: bool) -> Result<(), String> {
    apply_click_through(&app, enabled)
}

fn apply_click_through(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window is not ready".to_string())?;
    main.set_ignore_cursor_events(enabled)
        .map_err(|error| error.to_string())?;
    CLICK_THROUGH.store(enabled, Ordering::SeqCst);
    app.emit_to("main", "kero:click-through-changed", enabled)
        .map_err(|error| error.to_string())
}

fn locked_position() -> Option<WindowPosition> {
    LOCKED_POSITION
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|position| position.clone())
}

#[tauri::command]
fn set_main_position_lock(
    app: AppHandle,
    enabled: bool,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window is not ready".to_string())?;
    let position = if enabled {
        let position = match (x, y) {
            (Some(x), Some(y)) => PhysicalPosition::new(x, y),
            _ => main.outer_position().map_err(|error| error.to_string())?,
        };
        main.set_position(position)
            .map_err(|error| error.to_string())?;
        Some(WindowPosition {
            x: position.x,
            y: position.y,
        })
    } else {
        None
    };
    *LOCKED_POSITION
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Position lock is unavailable".to_string())? = position;
    app.emit_to("main", "kero:position-lock-changed", enabled)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_main_size(
    app: AppHandle,
    width: f64,
    height: f64,
    restore_x: Option<i32>,
    restore_y: Option<i32>,
    animate: Option<bool>,
) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口尚未就绪".to_string())?;
    let width = width.clamp(160.0, 760.0);
    let height = height.clamp(48.0, 800.0);
    let scale = main.scale_factor().map_err(|error| error.to_string())?;
    let current = main.outer_size().map_err(|error| error.to_string())?;
    let anchor = main.outer_position().map_err(|error| error.to_string())?;
    let destination = match (restore_x, restore_y) {
        (Some(x), Some(y)) => PhysicalPosition::new(x, y),
        _ => anchor,
    };
    let target_width = (width * scale).round() as u32;
    let target_height = (height * scale).round() as u32;
    // 听写条出现/收起不做形变动画：作废在跑的动画线程后直接切换尺寸和位置。
    if animate == Some(false) {
        SIZE_ANIMATION_ID.fetch_add(1, Ordering::SeqCst);
        main.set_size(PhysicalSize::new(target_width, target_height))
            .map_err(|error| error.to_string())?;
        main.set_position(destination)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    let animation_id = SIZE_ANIMATION_ID.fetch_add(1, Ordering::SeqCst) + 1;
    if current.width == target_width && current.height == target_height {
        main.set_position(destination)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }

    std::thread::spawn(move || {
        const STEPS: u32 = 18;
        for step in 1..=STEPS {
            if SIZE_ANIMATION_ID.load(Ordering::SeqCst) != animation_id {
                return;
            }
            let progress = step as f64 / STEPS as f64;
            let shifted = progress - 1.0;
            let eased = 1.0 + 2.70158 * shifted.powi(3) + 1.70158 * shifted.powi(2);
            let next_width = (current.width as f64
                + (target_width as f64 - current.width as f64) * eased)
                .round() as u32;
            let next_height = (current.height as f64
                + (target_height as f64 - current.height as f64) * eased)
                .round() as u32;
            let _ = main.set_size(PhysicalSize::new(next_width, next_height));
            let next_x =
                (anchor.x as f64 + (destination.x - anchor.x) as f64 * eased).round() as i32;
            let next_y =
                (anchor.y as f64 + (destination.y - anchor.y) as f64 * eased).round() as i32;
            let _ = main.set_position(PhysicalPosition::new(next_x, next_y));
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    });
    Ok(())
}

#[tauri::command]
fn get_main_position(app: AppHandle) -> Result<WindowPosition, String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window is not ready".to_string())?;
    let position = main.outer_position().map_err(|error| error.to_string())?;
    Ok(WindowPosition {
        x: position.x,
        y: position.y,
    })
}

#[tauri::command]
fn get_context_menu_placement(
    app: AppHandle,
    menu_height: i32,
) -> Result<ContextMenuPlacement, String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window is not ready".to_string())?;
    let position = main.outer_position().map_err(|error| error.to_string())?;
    let Some(monitor) = main.current_monitor().map_err(|error| error.to_string())? else {
        return Ok(ContextMenuPlacement {
            above: false,
            target_y: None,
        });
    };
    let monitor_top = monitor.position().y;
    let monitor_bottom = monitor.position().y + monitor.size().height as i32;
    let available_below = monitor_bottom - (position.y + 72);
    let content_below_capsule = menu_height.saturating_sub(72);
    if available_below >= content_below_capsule {
        return Ok(ContextMenuPlacement {
            above: false,
            target_y: None,
        });
    }

    // Keep both the expanded menu and the capsule on-screen when it opens upward.
    let minimum_y = monitor_top + 8;
    let maximum_y = (monitor_bottom - menu_height - 8).max(minimum_y);
    let target_y = (position.y - content_below_capsule).clamp(minimum_y, maximum_y);
    Ok(ContextMenuPlacement {
        above: true,
        target_y: Some(target_y),
    })
}

#[tauri::command]
fn set_main_position(
    app: AppHandle,
    x: i32,
    y: i32,
    update_locked_position: bool,
) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window is not ready".to_string())?;
    main.set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())?;
    if update_locked_position && locked_position().is_some() {
        *LOCKED_POSITION
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| "Position lock is unavailable".to_string())? =
            Some(WindowPosition { x, y });
    }
    Ok(())
}

#[tauri::command]
fn move_main_by(
    app: AppHandle,
    dx: i32,
    dy: i32,
    locked_x: Option<i32>,
    locked_y: Option<i32>,
) -> Result<WindowPosition, String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window is not ready".to_string())?;
    let position = main.outer_position().map_err(|error| error.to_string())?;
    let next = WindowPosition {
        x: position.x + dx,
        y: position.y + dy,
    };
    main.set_position(PhysicalPosition::new(next.x, next.y))
        .map_err(|error| error.to_string())?;
    if locked_position().is_some() {
        let persisted = match (locked_x, locked_y) {
            (Some(x), Some(y)) => WindowPosition { x, y },
            _ => next.clone(),
        };
        *LOCKED_POSITION
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| "Position lock is unavailable".to_string())? = Some(persisted);
    }
    Ok(next)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    STARTED_BY_AUTOSTART.store(
        std::env::args_os().any(|argument| {
            argument
                .to_string_lossy()
                .eq_ignore_ascii_case("--autostart")
        }),
        Ordering::SeqCst,
    );
    EXIT_REQUESTED.store(false, Ordering::SeqCst);
    #[cfg(windows)]
    if !acquire_instance_lock() {
        return;
    }
    install_runtime_diagnostics();
    let _runtime_session = RuntimeSessionGuard::begin();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if !EXIT_REQUESTED.load(Ordering::SeqCst) {
                    api.prevent_close();
                    let _ = window.hide();
                    trace_runtime(&format!(
                        "window close redirected to tray label={}",
                        window.label()
                    ));
                }
            }
        })
        .setup(|app| {
            let show_capsule = MenuItem::with_id(app, "tray-show", "显示胶囊", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "tray-settings", "打开设置", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "tray-quit", "退出 Kero", true, None::<&str>)?;
            let disable_click_through = MenuItem::with_id(
                app,
                "tray-disable-click-through",
                "取消鼠标穿透",
                true,
                None::<&str>,
            )?;
            let tray_menu = Menu::with_items(
                app,
                &[&show_capsule, &settings, &disable_click_through, &quit],
            )?;
            TrayIconBuilder::with_id("kero-tray")
                .icon(tauri::include_image!("./icons/32x32.png"))
                .tooltip("Kero AI 助手")
                .menu(&tray_menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "tray-show" => {
                        let _ = restore_main(app);
                    }
                    "tray-settings" => {
                        let _ = open_settings(app.clone());
                    }
                    "tray-disable-click-through" => {
                        let _ = apply_click_through(app, false);
                    }
                    "tray-quit" => request_app_exit(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            ..
                        } | TrayIconEvent::DoubleClick { .. }
                    ) {
                        let _ = restore_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // Auxiliary WebViews are controlled from the capsule and must not create
            // taskbar previews while hidden.
            for label in ["edge"] {
                if let Some(window) = app.get_webview_window(label) {
                    window.set_skip_taskbar(true)?;
                }
            }

            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(400));
                for label in ["edge"] {
                    if let Some(window) = app_handle.get_webview_window(label) {
                        let _ = window.set_skip_taskbar(true);
                    }
                }
            });

            // Transparent auxiliary windows can briefly inherit visibility on Windows.
            // Force the edge layer into its idle state before the main window is shown.
            if let Some(edge) = app.get_webview_window("edge") {
                edge.set_ignore_cursor_events(true)?;
                EDGE_CAPTURE_EXCLUDED
                    .store(exclude_edge_from_screen_capture(&edge), Ordering::SeqCst);
                edge.hide()?;
            }
            #[cfg(windows)]
            reset_system_cursor_scheme();
            #[cfg(windows)]
            install_keyboard_hook(app.handle().clone());
            #[cfg(windows)]
            start_mcp_bridge(app.handle().clone());
            trace_runtime("tauri setup completed");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_providers,
            save_provider,
            delete_provider,
            set_default_provider,
            get_image_generation_config,
            save_image_generation_config,
            get_dictation_asr_config,
            save_dictation_asr_config,
            generate_image,
            download_generated_image,
            get_system_prompt,
            set_system_prompt,
            get_context_enabled,
            set_context_enabled,
            get_screen_translation_settings,
            set_screen_translation_settings,
            start_screen_translation,
            stop_screen_translation,
            get_autostart_enabled,
            set_autostart_enabled,
            should_start_hidden,
            get_computer_control_enabled,
            set_computer_control_enabled,
            get_computer_control_risk_mode,
            set_computer_control_risk_mode,
            start_computer_control,
            stop_computer_control,
            computer_control_intent,
            computer_plan_task,
            computer_next_action,
            computer_execute_action,
            get_web_search_enabled,
            set_web_search_enabled,
            web_search_availability,
            get_web_search_proxy_enabled,
            set_web_search_proxy_enabled,
            chat_completion,
            optimize_image_prompt,
            chat_stream,
            stream_dictation_transform,
            optimize_dictation,
            transcribe_dictation_audio,
            transcribe_realtime_dictation_audio,
            start_realtime_dictation,
            push_realtime_dictation_audio,
            finish_realtime_dictation,
            warm_dictation_service,
            realtime_model_supported,
            capture_primary_screen,
            insert_text_to_active,
            replace_realtime_dictation_text,
            clear_dictation_focus_target,
            selected_text_from_active,
            replace_selected_text,
            get_work_area,
            set_realtime_preconnect_hint,
            is_alt_key_down,
            activate_assistant,
            hide_edge,
            mark_edge_ready,
            open_settings,
            normalize_settings_window,
            open_chat,
            set_chat_size,
            toggle_chat_maximize,
            hide_window,
            minimize_window,
            show_main,
            show_main_passive,
            minimize_to_tray,
            quit_app,
            set_main_click_through,
            set_main_position_lock,
            get_main_position,
            get_context_menu_placement,
            set_main_position,
            move_main_by,
            set_main_size
        ])
        .run(tauri::generate_context!())
        .expect("启动 Kero 时出现错误");
}
