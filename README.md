# Kero

Kero 是一个 Windows 桌面 AI 助手：以可拖动的胶囊为入口，支持文本对话、语音听写、实时通话、屏幕边缘光效、图片生成，以及可选的电脑操控和 MCP 工具桥接。

当前版本：`2.0.0`

## 功能

- 胶囊式桌面入口：可拖动、托盘驻留、位置锁定、鼠标穿透和透明度/玻璃效果调节。
- AI 对话：支持流式输出、Markdown、上下文开关、系统提示词和多个 OpenAI 兼容/Anthropic/Google 提供商。
- 语音输入：原生语音识别和 AI 语音识别两种模式；按住 `Alt` 听写，松开后自动写入当前外部输入框。
- 语音增强：可选 AI 润色、错字纠正、标点整理、词汇表和听写记忆。
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

## 常用操作

- 按住 `Alt`：开始语音听写；松开 `Alt`：停止并写入当前外部输入框。
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
