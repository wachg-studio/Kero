//! 本地离线语音识别（sherpa-onnx + SenseVoice-Small int8）。
//! 模型按需下载到应用数据目录；识别器惰性加载，可显式卸载释放内存。
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Serialize;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

// SenseVoice-Small int8 量化模型（sherpa-onnx 官方发布，压缩包约 230MB）。
// 官方以 tar.bz2 分发；Windows 10+ 自带的 tar 可直接解压。
const MODEL_BASE_NAME: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17";
const MODEL_ARCHIVE: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17.tar.bz2";
const MODEL_FILE: &str = "model.int8.onnx";
const TOKENS_FILE: &str = "tokens.txt";
const MODEL_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17.tar.bz2";

static LOCAL_ASR_DOWNLOADING: AtomicBool = AtomicBool::new(false);
static LOCAL_ASR_CANCEL: AtomicBool = AtomicBool::new(false);
static LOCAL_ASR_RECOGNIZER: Mutex<Option<sherpa_rs::sense_voice::SenseVoiceRecognizer>> =
    Mutex::new(None);
static LOCAL_ASR_BUSY: AtomicBool = AtomicBool::new(false);
static LOCAL_ASR_LAST_USED: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrState {
    installed: bool,
    model_dir: String,
    downloading: bool,
    loaded: bool,
}

fn local_asr_dir() -> Result<std::path::PathBuf, String> {
    let dirs = directories::ProjectDirs::from("", "", "Kero")
        .ok_or_else(|| "无法定位应用数据目录".to_string())?;
    Ok(dirs.data_dir().join("local-asr").join(MODEL_BASE_NAME))
}

fn model_installed(dir: &std::path::Path) -> bool {
    dir.join(MODEL_FILE).is_file() && dir.join(TOKENS_FILE).is_file()
}

#[tauri::command]
pub fn get_local_asr_state() -> LocalAsrState {
    let model_dir = local_asr_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let installed = local_asr_dir()
        .map(|dir| model_installed(&dir))
        .unwrap_or(false);
    let loaded = LOCAL_ASR_RECOGNIZER.lock().map(|guard| guard.is_some()).unwrap_or(false);
    LocalAsrState {
        installed,
        model_dir,
        downloading: LOCAL_ASR_DOWNLOADING.load(Ordering::SeqCst),
        loaded,
    }
}

#[tauri::command]
pub fn cancel_local_asr_download() {
    LOCAL_ASR_CANCEL.store(true, Ordering::SeqCst);
}

/// 下载 SenseVoice 模型（增量写入 + 进度事件 kero:local-asr-download）。
#[tauri::command]
pub async fn download_local_asr_model(app: AppHandle) -> Result<(), String> {
    if LOCAL_ASR_DOWNLOADING.swap(true, Ordering::SeqCst) {
        return Err("模型正在下载中".to_string());
    }
    LOCAL_ASR_CANCEL.store(false, Ordering::SeqCst);
    let result = download_model_inner(&app).await;
    LOCAL_ASR_DOWNLOADING.store(false, Ordering::SeqCst);
    let _ = app.emit_to("settings", "kero:local-asr-download", serde_json::json!({ "done": true }));
    result
}

async fn download_model_inner(app: &AppHandle) -> Result<(), String> {
    let dir = local_asr_dir()?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("无法创建模型目录: {error}"))?;
    let client = super::shared_http_client();
    let archive_path = dir.join(MODEL_ARCHIVE);
    if !model_installed(&dir) {
        if !archive_path.is_file() {
            download_to_file(&client, MODEL_URL, &archive_path, MODEL_ARCHIVE, app).await?;
        }
        // 系统 tar（Windows 10+ 自带 bsdtar）解压到临时目录，再取出两个模型文件。
        let extract_dir = dir.join("extracted");
        let _ = std::fs::remove_dir_all(&extract_dir);
        std::fs::create_dir_all(&extract_dir).map_err(|error| format!("无法创建解压目录: {error}"))?;
        let output = std::process::Command::new("tar")
            .args(["-xjf", archive_path.to_string_lossy().as_ref()])
            .arg("-C")
            .arg(&extract_dir)
            .output()
            .map_err(|error| format!("无法启动解压程序: {error}"))?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&extract_dir);
            return Err(format!("模型解压失败: {}", String::from_utf8_lossy(&output.stderr).trim()));
        }
        // 压缩包内顶层目录与包同名；把所需文件移到模型目录。
        let inner = extract_dir.join(MODEL_BASE_NAME);
        let source_dir = if inner.is_dir() { inner } else { extract_dir.clone() };
        for file_name in [MODEL_FILE, TOKENS_FILE] {
            std::fs::rename(source_dir.join(file_name), dir.join(file_name))
                .map_err(|error| format!("模型文件整理失败: {error}"))?;
        }
        let _ = std::fs::remove_dir_all(&extract_dir);
        let _ = std::fs::remove_file(&archive_path);
    }
    Ok(())
}

async fn download_to_file(
    client: &reqwest::Client,
    url: &str,
    target: &std::path::Path,
    file_name: &str,
    app: &AppHandle,
) -> Result<(), String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("连接模型下载源失败: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("模型下载源返回 {status}", status = response.status()));
    }
    let total = response.content_length().unwrap_or(0);
    let temp_path = target.with_extension("downloading");
    let mut file = std::fs::File::create(&temp_path).map_err(|error| format!("无法写入模型文件: {error}"))?;
    let mut downloaded: u64 = 0;
    let mut next_report = 0u64;
    let mut stream = response;
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|error| format!("模型下载中断: {error}"))?
    {
        if LOCAL_ASR_CANCEL.load(Ordering::SeqCst) {
            drop(file);
            let _ = std::fs::remove_file(&temp_path);
            return Err("下载已取消".to_string());
        }
        file.write_all(&chunk).map_err(|error| format!("模型写入失败: {error}"))?;
        downloaded += chunk.len() as u64;
        if downloaded >= next_report {
            next_report = downloaded + (2 << 20);
            let _ = app.emit_to(
                "settings",
                "kero:local-asr-download",
                serde_json::json!({
                    "file": file_name,
                    "downloaded": downloaded,
                    "total": total,
                }),
            );
        }
    }
    file.flush().map_err(|error| format!("模型写入失败: {error}"))?;
    drop(file);
    std::fs::rename(&temp_path, target).map_err(|error| format!("模型文件保存失败: {error}"))?;
    Ok(())
}

fn ensure_recognizer() -> Result<(), String> {
    let dir = local_asr_dir()?;
    if !model_installed(&dir) {
        return Err("本地语音模型尚未下载，请先在设置中下载".to_string());
    }
    let mut guard = LOCAL_ASR_RECOGNIZER.lock().map_err(|_| "本地识别器状态异常".to_string())?;
    if guard.is_some() {
        return Ok(());
    }
    let config = sherpa_rs::sense_voice::SenseVoiceConfig {
        model: dir.join(MODEL_FILE).to_string_lossy().into_owned(),
        tokens: dir.join(TOKENS_FILE).to_string_lossy().into_owned(),
        language: "zh".into(),
        use_itn: true,
        num_threads: Some(2),
        provider: Some("cpu".into()),
        debug: false,
    };
    let recognizer = sherpa_rs::sense_voice::SenseVoiceRecognizer::new(config)
        .map_err(|error| format!("本地语音模型加载失败: {error}"))?;
    *guard = Some(recognizer);
    Ok(())
}

/// 卸载识别器，释放约 300~500MB 内存。
#[tauri::command]
pub fn unload_local_asr() {
    if let Ok(mut guard) = LOCAL_ASR_RECOGNIZER.lock() {
        *guard = None;
    }
}

/// 用本地 SenseVoice 模型转写 16kHz PCM（base64）。
#[tauri::command]
pub async fn transcribe_local_dictation(pcm_base64: String, sample_rate: u32) -> Result<String, String> {
    if LOCAL_ASR_BUSY.swap(true, Ordering::SeqCst) {
        return Err("本地识别正在进行中".to_string());
    }
    let audio = BASE64
        .decode(pcm_base64.trim())
        .map_err(|_| "无法读取录音数据".to_string())?;
    let result = tokio::task::spawn_blocking(move || {
        LOCAL_ASR_LAST_USED.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_secs())
                .unwrap_or(0),
            Ordering::SeqCst,
        );
        ensure_recognizer()?;
        let samples_total = audio.len() / 2;
        let mut samples = Vec::with_capacity(samples_total);
        for chunk in audio.chunks_exact(2) {
            samples.push(i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32768.0);
        }
        let text = {
            let mut guard = LOCAL_ASR_RECOGNIZER.lock().map_err(|_| "本地识别器状态异常".to_string())?;
            let recognizer = guard.as_mut().ok_or_else(|| "本地语音模型未加载".to_string())?;
            recognizer.transcribe(sample_rate, &samples).text
        };
        let text = super::dictation::normalize_dictation_punctuation(text.trim());
        Ok(text)
    })
    .await
    .map_err(|error| format!("本地识别任务异常: {error}"))?;
    LOCAL_ASR_BUSY.store(false, Ordering::SeqCst);
    result
}
