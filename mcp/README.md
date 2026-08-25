# Kero Computer MCP

这是 Kero 的本地 MCP 服务。它通过 `stdio` 与 MCP 客户端通信，并通过
`127.0.0.1:47821` 连接正在运行的 Kero。端口只监听本机，不接受局域网连接。

MCP 只负责协议和参数转换，截图、鼠标、键盘、边缘光效、鼠标贴图和图片生成均由 Kero 执行。

## 提供的能力

- 排除 Kero 自身窗口的屏幕截图
- 鼠标移动、单击、双击、右键、长按、拖动和滚轮
- Unicode 文本输入、按键、快捷键和按键长按
- 启动应用、创建桌面文件夹
- 启动屏幕边缘光效和操控期间的鼠标提示
- 调用 Kero 已配置的图片模型生成图片并返回给 Agent

`kero_generate_image` 不读取也不返回图片 API Key。Agent 负责提示词，Kero 使用本机加密配置。

## 配置

先启动 Kero，再将 MCP 可执行文件加入 Agent 的 MCP 配置：

```toml
[mcp_servers.kero]
command = "C:/path/to/KeroComputerMcp.exe"
```

Windows 用户可以直接使用仓库 `downloads/KeroComputerMcp.exe`，或从 GitHub Release 下载同名文件。
客户端应先调用 `kero_start_control`，根据截图逐步操作，完成或放弃任务后始终调用
`kero_stop_control`。
