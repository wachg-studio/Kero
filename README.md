# Kero

Kero 是一个 Windows 桌面 AI 助手：以可拖动的胶囊为入口，支持文本对话、语音听写、实时通话、屏幕边缘光效、图片生成，以及可选的电脑操控和 MCP 工具桥接。

当前版本：`2.0.0`

## 功能

- 胶囊式桌面入口：可拖动、托盘驻留、位置锁定、鼠标穿透和透明度/玻璃效果调节。
- AI 对话：支持流式输出、Markdown、上下文开关、系统提示词和多个 OpenAI 兼容/Anthropic/Google 提供商。
- 语音输入：AI 实时听写（沉浸式黑色听写条：实时转写逐字上屏、实时声波、松开 `Alt` 后自动润色并原位替换），也可切换系统原生识别作为免配置备用；文字实时写入当前外部输入框，标点统一为英文半角且不在结尾追加句号。
- 语音增强：可选 AI 润色、错字纠正、词汇表和听写记忆；听写条内支持取消（`Esc`）、确认写入、失败重试与复制。
- 实时通话：结合屏幕画面进行提问，支持语音回复和电脑操控切换。
- 图片生成：支持自定义模型、三种画幅、批量生成、参考图片和提示词优化。
- 电脑操控：基于屏幕观察和 Windows UI Automation，可执行鼠标、键盘、窗口和 PowerShell 辅助操作，并支持确认模式/风险模式。
- MCP：提供电脑操控、屏幕观察、UI 控件检查、流式文本输入和图片生成工具，供 Agent 调用。
- 屏幕能力：全屏翻译、屏幕边缘光效、电脑操控指针提示和标记纠错。

## 安装

1. 从 [Releases](../../releases) 下载 `Kero-Setup-2.0.0.exe`，或打开仓库 `downloads/` 目录下载。
2. 双击安装程序，按中文向导完成安装。
3. 首次启动后打开设置，分别配置聊天提供商、API Key、模型名称；图片生成和 AI 语音识别的 Key 可单独配置。
4. 安装后 Kero 默认只显示胶囊，不在任务栏占用窗口；可从托盘打开设置、完整对话或退出。

## 模型与 API Key（解锁完整功能）

Kero 的语音听写、润色和对话能力依赖你自行配置的大模型服务。推荐前往 **[千问AI开放平台](https://platform.qianwenai.com/home/)** 注册并创建 API Key——这个网址中可以获得**完整使用本应用功能所需的模型**（例如实时语音识别模型 `qwen3-asr-flash-realtime`、语音识别 `qwen-audio-3.0-asr-flash` 以及对话/润色用的 Qwen 系列模型）。

获取 Key 后填入 Kero 即可：

1. 设置 → 语音识别：服务地址填 `https://dashscope.aliyuncs.com/compatible-mode/v1`，模型填 `qwen3-asr-flash-realtime`，粘贴 API Key 并保存，即可启用实时转写听写。
2. 设置 → 提供商：将 Qwen 系列模型配置为聊天提供商，用于对话、听写润色和实时通话。

## 常用操作

- 按住 `Alt`：开始中文语音听写；松开 `Alt`：流式整理后写入当前外部输入框。
- 按住 `Alt + E`：开始英译语音听写；松开后把说出的中文翻译成英文并写入当前外部输入框。
- 选中一段文本后按 `Ctrl + E + F`：将选区流式翻译成英文并直接覆盖原文。
- `Esc + K`：解除鼠标穿透。
- `Alt + K`：触发语音输入快捷流程。
- 胶囊右键：打开位置、透明度、玻璃效果、鼠标穿透、电脑操控和托盘相关选项。
- 完整对话中可直接发送文本、图片或文件；需要电脑操作时，Kero 会根据意图切换到电脑操控。

## MCP 配置

仓库同时包含 MCP 源码（`mcp/`）和 Windows 运行时（`downloads/KeroComputerMcp.exe`）。先启动 Kero，再在 Agent 的 MCP 配置中添加：

```toml
[mcp_servers.kero]
command = "C:/path/to/KeroComputerMcp.exe"
```

启动后，Agent 可以使用屏幕截图、UI 控件检查、鼠标键盘操作、Unicode 流式输入和图片生成工具。工具只提供能力，具体决策仍由 Agent 完成。Windows 用户可以直接将 `downloads/KeroComputerMcp.exe` 下载到本地后填入绝对路径。

MCP 只通过本机 `127.0.0.1:47821` 与 Kero 通信，不接受局域网连接；它不会读取或返回 Kero 保存的 API Key。

## 从源码运行

环境要求：Windows、Node.js、pnpm、Rust stable、WebView2 和 Tauri 2 工具链。

```powershell
pnpm install
pnpm tauri dev
```

正式构建：

```powershell
pnpm tauri build --no-bundle
& "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" installer\Kero.iss
```

## 许可证

本项目使用 [Kero 非商业源码许可 1.0](LICENSE.md)。源码可以查看、运行和修改，但禁止商业使用、收费分发、商业部署、SaaS/API 运营和商业产品集成。需要商业授权请联系版权所有者。

## 免责声明

Kero 会调用用户自行配置的第三方 AI 服务，也可以执行电脑操控。请检查 API 费用、隐私政策和操作结果；本项目按“现状”提供，不对第三方服务、自动化结果或数据损失承担责任。
