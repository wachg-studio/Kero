using System.Diagnostics;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace KeroComputerMcp;

internal static class Program
{
    private const string ProtocolVersion = "2025-06-18";
    private static readonly BridgeClient Bridge = new();
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        WriteIndented = false
    };

    private static readonly JsonArray Tools = JsonNode.Parse("""
    [
      {
        "name": "kero_start_control",
        "description": "Start a Kero computer-control session. This activates Kero's screen-edge shader and replaces the system pointer with Kero's black cursor with a white outline when the agent decides a visible control session is appropriate.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "color_mode": { "type": "string", "enum": ["rainbow", "blue"], "default": "rainbow", "description": "Kero edge-glow colour mode." }
          },
          "additionalProperties": false
        }
      },
      {
        "name": "kero_screenshot",
        "description": "Capture the primary display while temporarily excluding Kero's capsule and edge window. Returns a JPEG image for visual grounding when it is useful; applications may instead be inspected through kero_inspect_ui.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "max_width": { "type": "integer", "minimum": 320, "maximum": 2560, "default": 1600 },
            "max_height": { "type": "integer", "minimum": 240, "maximum": 1600, "default": 1080 }
          },
          "additionalProperties": false
        }
      },
      {
        "name": "kero_inspect_ui",
        "description": "Read the foreground Windows application's accessibility control tree through Microsoft UI Automation. Prefer this before a screenshot for ordinary desktop controls. Use a returned control id as ui_target with kero_click or kero_type_text; this directly invokes or focuses the control without visual coordinate guessing. If no suitable control is returned, fall back to kero_screenshot.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
      },
      {
        "name": "kero_wait_for_ui",
        "description": "Wait and verify a semantic UI condition inside one tool call. Use after an action when a window or control must appear or disappear; it polls Microsoft UI Automation instead of making the agent repeatedly inspect screenshots.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "window_contains": { "type": "string", "maxLength": 240, "description": "Required window-title or window-class fragment, unless control_contains is provided." },
            "control_contains": { "type": "string", "maxLength": 240, "description": "Required accessible control name or id fragment, unless window_contains is provided." },
            "control_type": { "type": "string", "maxLength": 80, "description": "Optional control type, such as Button, Edit, MenuItem, or ListItem." },
            "state": { "type": "string", "enum": ["present", "absent"], "default": "present" },
            "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 15000, "default": 3000 },
            "poll_ms": { "type": "integer", "minimum": 80, "maximum": 1000, "default": 180 }
          },
          "additionalProperties": false
        }
      },
      {
        "name": "kero_generate_image",
        "description": "Generate images through Kero's configured image model. Write the complete image prompt yourself: include subject, composition, visual style, lighting, materials, important text restrictions, and any required details. Kero keeps the image API key private. The result contains the saved local image path and image content so you can inspect it before using it in later work.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "prompt": { "type": "string", "minLength": 1, "maxLength": 6000, "description": "A complete, standalone image-generation prompt written by the agent." },
            "aspect_ratio": { "type": "string", "enum": ["1:1", "16:9", "9:16"], "default": "1:1" },
            "count": { "type": "integer", "minimum": 1, "maximum": 4, "default": 1 }
          },
          "required": ["prompt"],
          "additionalProperties": false
        }
      },
      {
        "name": "kero_report_decision",
        "description": "Optionally show a concise, user-visible AI decision in the Kero capsule. Report observable evidence and the chosen next step, not private chain-of-thought.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "phase": { "type": "string", "enum": ["observe", "decide", "act", "verify", "complete"], "default": "decide" },
            "summary": { "type": "string", "minLength": 1, "maxLength": 120 },
            "detail": { "type": "string", "maxLength": 520 },
            "next_action": { "type": "string", "maxLength": 180 }
          },
          "required": ["summary"],
          "additionalProperties": false
        }
      },
      {
        "name": "kero_move_pointer",
        "description": "Move the pointer smoothly. Coordinates are normalized over the primary display: left/top is 0,0 and right/bottom is 1,1.",
        "inputSchema": { "type": "object", "properties": { "x": { "type": "number", "minimum": 0, "maximum": 1 }, "y": { "type": "number", "minimum": 0, "maximum": 1 } }, "required": ["x", "y"], "additionalProperties": false }
      },
      {
        "name": "kero_click",
        "description": "Click at normalized primary-display coordinates. Kero shows pointer movement and a click flash. Use click_count=2 for a real Windows double-click.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "x": { "type": "number", "minimum": 0, "maximum": 1 },
            "y": { "type": "number", "minimum": 0, "maximum": 1 },
            "button": { "type": "string", "enum": ["left", "right"], "default": "left" },
            "click_count": { "type": "integer", "enum": [1, 2], "default": 1 },
            "ui_target": { "type": "string", "description": "Control id returned by kero_inspect_ui. When present, coordinates are unnecessary." },
            "ui_action": { "type": "string", "enum": ["invoke", "focus", "select"], "default": "invoke" },
            "description": { "type": "string" }
          },
          "additionalProperties": false
        }
      },
      {
        "name": "kero_drag",
        "description": "Drag from one normalized point to another while holding the left mouse button.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "x": { "type": "number", "minimum": 0, "maximum": 1 }, "y": { "type": "number", "minimum": 0, "maximum": 1 },
            "end_x": { "type": "number", "minimum": 0, "maximum": 1 }, "end_y": { "type": "number", "minimum": 0, "maximum": 1 },
            "duration_ms": { "type": "integer", "minimum": 180, "maximum": 8000, "default": 650 }
          },
          "required": ["x", "y", "end_x", "end_y"], "additionalProperties": false
        }
      },
      {
        "name": "kero_long_press",
        "description": "Hold the left mouse button at a normalized point for a duration.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "x": { "type": "number", "minimum": 0, "maximum": 1 }, "y": { "type": "number", "minimum": 0, "maximum": 1 },
            "duration_ms": { "type": "integer", "minimum": 120, "maximum": 8000, "default": 720 }
          },
          "required": ["x", "y"], "additionalProperties": false
        }
      },
      {
        "name": "kero_scroll",
        "description": "Scroll the active window. Positive amount scrolls up and negative amount scrolls down.",
        "inputSchema": { "type": "object", "properties": { "amount": { "type": "integer", "minimum": -20, "maximum": 20 } }, "required": ["amount"], "additionalProperties": false }
      },
      {
        "name": "kero_type_text",
        "description": "Type Unicode text through native Windows keyboard events in short visible chunks; it does not use or replace the clipboard. With ui_target from kero_inspect_ui, Kero focuses that edit control before streaming the text.",
        "inputSchema": { "type": "object", "properties": { "text": { "type": "string", "minLength": 1, "maxLength": 20000 }, "ui_target": { "type": "string", "description": "Edit control id returned by kero_inspect_ui." }, "ui_action": { "type": "string", "enum": ["focus"], "default": "focus" } }, "required": ["text"], "additionalProperties": false }
      },
      {
        "name": "kero_send_chat_message",
        "description": "Fast path for a currently open chat: types text into the focused chat composer and presses Enter in one MCP call. The agent decides when the recipient and focused composer are sufficiently established.",
        "inputSchema": { "type": "object", "properties": { "text": { "type": "string", "minLength": 1, "maxLength": 20000 } }, "required": ["text"], "additionalProperties": false }
      },
      {
        "name": "kero_run_sequence",
        "description": "Execute a predictable chain of up to 8 low-risk UI actions in one call, avoiding one tool call per click, type, or key. The agent decides whether a sequence is appropriate and how to verify the result. Typical chat sequence: click a clearly identified conversation, wait 180ms, type text, press Enter. Avoid using it for ambiguous or sensitive final actions.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "steps": {
              "type": "array", "minItems": 1, "maxItems": 8,
              "items": {
                "type": "object",
                "properties": {
                  "action": { "type": "string", "enum": ["move", "click", "double_click", "right_click", "drag", "long_press", "scroll", "type", "key", "hotkey", "hold_keys", "wait"] },
                  "x": { "type": "number", "minimum": 0, "maximum": 1 }, "y": { "type": "number", "minimum": 0, "maximum": 1 },
                  "end_x": { "type": "number", "minimum": 0, "maximum": 1 }, "end_y": { "type": "number", "minimum": 0, "maximum": 1 },
                  "text": { "type": "string", "maxLength": 20000 }, "key": { "type": "string", "maxLength": 20 },
                  "ui_target": { "type": "string" }, "ui_action": { "type": "string", "enum": ["invoke", "focus", "select"] },
                  "keys": { "type": "array", "items": { "type": "string" }, "maxItems": 6 }, "amount": { "type": "integer", "minimum": -20, "maximum": 20 },
                  "duration_ms": { "type": "integer", "minimum": 50, "maximum": 8000 }, "description": { "type": "string", "maxLength": 180 }
                }, "required": ["action"], "additionalProperties": false
              }
            },
            "verify": {
              "type": "object",
              "description": "Optional semantic condition to wait for after all steps complete.",
              "properties": {
                "window_contains": { "type": "string", "maxLength": 240 },
                "control_contains": { "type": "string", "maxLength": 240 },
                "control_type": { "type": "string", "maxLength": 80 },
                "state": { "type": "string", "enum": ["present", "absent"], "default": "present" },
                "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 15000, "default": 3000 },
                "poll_ms": { "type": "integer", "minimum": 80, "maximum": 1000, "default": 180 }
              },
              "additionalProperties": false
            }
          }, "required": ["steps"], "additionalProperties": false
        }
      },
      {
        "name": "kero_press_key",
        "description": "Press one key, for example enter, escape, tab, backspace, delete, up, down, left, right, home, end, space, f1 through f12, or a letter/digit.",
        "inputSchema": { "type": "object", "properties": { "key": { "type": "string", "minLength": 1, "maxLength": 20 } }, "required": ["key"], "additionalProperties": false }
      },
      {
        "name": "kero_hotkey",
        "description": "Press a keyboard shortcut. Modifier names include ctrl, alt, shift, and win. Example: [\"ctrl\", \"l\"].",
        "inputSchema": { "type": "object", "properties": { "keys": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 6 } }, "required": ["keys"], "additionalProperties": false }
      },
      {
        "name": "kero_hold_keys",
        "description": "Hold one or more keys simultaneously for a duration, suitable for games and timeline controls.",
        "inputSchema": { "type": "object", "properties": { "keys": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 6 }, "duration_ms": { "type": "integer", "minimum": 60, "maximum": 8000, "default": 520 } }, "required": ["keys"], "additionalProperties": false }
      },
      {
        "name": "kero_open_app",
        "description": "Open an installed application by name through Kero's Windows application-launch flow. The agent can choose an appropriate verification method afterward.",
        "inputSchema": { "type": "object", "properties": { "name": { "type": "string", "minLength": 1, "maxLength": 200 } }, "required": ["name"], "additionalProperties": false }
      },
      {
        "name": "kero_create_desktop_folder",
        "description": "Create a named folder on the Windows desktop through Kero's hidden PowerShell fallback.",
        "inputSchema": { "type": "object", "properties": { "name": { "type": "string", "minLength": 1, "maxLength": 180 } }, "required": ["name"], "additionalProperties": false }
      },
      {
        "name": "kero_wait",
        "description": "Wait briefly for an application or animation when the agent determines that the next operation needs it.",
        "inputSchema": { "type": "object", "properties": { "duration_ms": { "type": "integer", "minimum": 50, "maximum": 8000, "default": 800 } }, "additionalProperties": false }
      },
      {
        "name": "kero_stop_control",
        "description": "Finish a Kero computer-control session. This fades and hides the edge shader and restores the user's original Windows mouse cursor when the agent decides the visible session is complete.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
      }
    ]
    """)!.AsArray();

    public static async Task Main()
    {
        Console.InputEncoding = new UTF8Encoding(false);
        Console.OutputEncoding = new UTF8Encoding(false);
        AppDomain.CurrentDomain.ProcessExit += (_, _) => Bridge.TryStopOnExit();

        string? line;
        while ((line = await Console.In.ReadLineAsync()) is not null)
        {
            line = line.TrimStart('\uFEFF');
            if (string.IsNullOrWhiteSpace(line)) continue;
            JsonObject? request = null;
            try
            {
                request = JsonNode.Parse(line)?.AsObject();
                if (request is null) continue;
                var response = await HandleRequestAsync(request);
                if (response is not null)
                {
                    await Console.Out.WriteLineAsync(response.ToJsonString(JsonOptions));
                    await Console.Out.FlushAsync();
                }
            }
            catch (Exception error)
            {
                await Console.Error.WriteLineAsync($"Kero MCP error: {error}");
                if (request?["id"] is not null)
                {
                    var response = RpcError(request["id"]!.DeepClone(), -32603, error.Message);
                    await Console.Out.WriteLineAsync(response.ToJsonString(JsonOptions));
                    await Console.Out.FlushAsync();
                }
            }
        }
    }

    private static async Task<JsonObject?> HandleRequestAsync(JsonObject request)
    {
        var method = request["method"]?.GetValue<string>() ?? string.Empty;
        var id = request["id"]?.DeepClone();
        if (id is null) return null;

        return method switch
        {
            "initialize" => RpcResult(id, new JsonObject
            {
                ["protocolVersion"] = ProtocolVersion,
                ["capabilities"] = new JsonObject { ["tools"] = new JsonObject { ["listChanged"] = false } },
                ["serverInfo"] = new JsonObject { ["name"] = "kero-computer", ["version"] = "1.1.0" },
                ["instructions"] = "Kero provides visible Windows observation and input capabilities. The agent remains responsible for choosing tools, sequence, verification, and when to end control. Microsoft UI Automation via kero_inspect_ui is usually the quickest and most reliable option for ordinary exposed application controls; screenshots remain useful for visual-only targets such as games, canvas/web content, desktop icons, or unavailable accessibility data. kero_wait_for_ui and the optional kero_run_sequence verify field can wait for semantic UI changes without an extra model round trip when the agent judges that useful. kero_run_sequence and kero_send_chat_message reduce round trips for stable low-risk flows. kero_report_decision can surface concise evidence and the next step to the user without exposing private chain-of-thought."
            }),
            "ping" => RpcResult(id, new JsonObject()),
            "tools/list" => RpcResult(id, new JsonObject { ["tools"] = Tools.DeepClone() }),
            "tools/call" => RpcResult(id, await CallToolAsync(request["params"]?.AsObject() ?? new JsonObject())),
            _ => RpcError(id, -32601, $"Method not found: {method}")
        };
    }

    private static async Task<JsonObject> CallToolAsync(JsonObject parameters)
    {
        var name = parameters["name"]?.GetValue<string>() ?? string.Empty;
        var args = parameters["arguments"]?.AsObject() ?? new JsonObject();
        try
        {
            if (name == "kero_start_control")
            {
                var mode = StringArg(args, "color_mode", "rainbow");
                await Bridge.StartSessionAsync(mode);
                return TextResult($"Kero control started with {mode} edge glow. The Kero cursor skin is active.");
            }
            if (name == "kero_stop_control")
            {
                await Bridge.StopSessionAsync();
                return TextResult("Kero control stopped. Edge glow is hidden and the original Windows cursor has been restored.");
            }
            if (name == "kero_generate_image")
            {
                var prompt = RequiredString(args, "prompt");
                var response = await Bridge.RequestAsync(new JsonObject
                {
                    ["op"] = "generate_image",
                    ["prompt"] = prompt,
                    ["aspectRatio"] = StringArg(args, "aspect_ratio", "1:1"),
                    ["count"] = IntArg(args, "count", 1)
                });
                return await ImageResultAsync(response, prompt);
            }

            await Bridge.EnsureSessionAsync();
            if (name == "kero_inspect_ui")
            {
                var response = await Bridge.RequestAsync(new JsonObject { ["op"] = "inspect_ui" });
                return UiInspectionResult(response);
            }
            if (name == "kero_wait_for_ui")
            {
                return TextResult(await WaitForUiAsync(args));
            }
            if (name == "kero_send_chat_message")
            {
                var text = RequiredString(args, "text");
                var chatResult = await Bridge.RequestAsync(new JsonObject
                {
                    ["op"] = "execute_batch",
                    ["actions"] = new JsonArray
                    {
                        new JsonObject { ["action"] = "type", ["text"] = text, ["requiresConfirmation"] = false, ["fast"] = true },
                        new JsonObject { ["action"] = "key", ["key"] = "enter", ["requiresConfirmation"] = false, ["fast"] = true }
                    }
                });
                return TextResult($"Chat message sent. Completed {chatResult["messages"]?.AsArray().Count ?? 2} actions.");
            }
            if (name == "kero_run_sequence")
            {
                var steps = args["steps"]?.AsArray() ?? throw new ArgumentException("steps is required");
                if (steps.Count is < 1 or > 8) throw new ArgumentException("steps must contain 1 to 8 actions");
                var actions = new JsonArray();
                foreach (var step in steps)
                {
                    if (step is not JsonObject item) throw new ArgumentException("each step must be an object");
                    actions.Add(BuildSequenceAction(item));
                }
                var sequenceResult = await Bridge.RequestAsync(new JsonObject { ["op"] = "execute_batch", ["actions"] = actions });
                var message = $"Kero completed {sequenceResult["messages"]?.AsArray().Count ?? actions.Count} actions in one sequence.";
                if (args["verify"] is JsonObject verification)
                    message += "\n" + await WaitForUiAsync(verification);
                return TextResult(message);
            }
            if (name == "kero_report_decision")
            {
                var summary = RequiredString(args, "summary");
                await Bridge.RequestAsync(new JsonObject
                {
                    ["op"] = "decision",
                    ["phase"] = StringArg(args, "phase", "decide"),
                    ["summary"] = summary,
                    ["detail"] = args["detail"]?.DeepClone(),
                    ["nextAction"] = args["next_action"]?.DeepClone()
                });
                return TextResult($"Decision shown in Kero: {summary}");
            }
            if (name == "kero_screenshot")
            {
                await Bridge.RequestAsync(new JsonObject
                {
                    ["op"] = "decision",
                    ["phase"] = "observe",
                    ["summary"] = "正在观察当前屏幕",
                    ["nextAction"] = "读取画面并识别可交互目标"
                });
                var response = await Bridge.RequestAsync(new JsonObject
                {
                    ["op"] = "capture",
                    ["maxWidth"] = IntArg(args, "max_width", 1600),
                    ["maxHeight"] = IntArg(args, "max_height", 1080),
                    ["jpegQuality"] = 80
                });
                var dataUrl = response["image"]?.GetValue<string>() ?? throw new InvalidOperationException("Kero returned no screenshot.");
                var comma = dataUrl.IndexOf(',');
                var base64 = comma >= 0 ? dataUrl[(comma + 1)..] : dataUrl;
                return new JsonObject
                {
                    ["content"] = new JsonArray
                    {
                        new JsonObject { ["type"] = "text", ["text"] = "Primary display screenshot. Coordinates for Kero tools are normalized from 0 to 1." },
                        new JsonObject { ["type"] = "image", ["data"] = base64, ["mimeType"] = "image/jpeg" }
                    }
                };
            }

            var action = BuildAction(name, args);
            var result = await Bridge.RequestAsync(new JsonObject { ["op"] = "execute", ["action"] = action });
            return TextResult(result["message"]?.GetValue<string>() ?? "Kero action completed.");
        }
        catch (Exception error)
        {
            return new JsonObject
            {
                ["isError"] = true,
                ["content"] = new JsonArray { new JsonObject { ["type"] = "text", ["text"] = error.Message } }
            };
        }
    }

    private static JsonObject BuildAction(string tool, JsonObject args)
    {
        // MCP actions already have an explicit target. Keep their cursor trail visible,
        // but avoid the slower presentation timing used by manual in-app control.
        var action = new JsonObject { ["requiresConfirmation"] = false, ["fast"] = true };
        switch (tool)
        {
            case "kero_move_pointer":
                action["action"] = "move"; AddPoint(action, args); break;
            case "kero_click":
                var button = StringArg(args, "button", "left");
                var count = IntArg(args, "click_count", 1);
                action["action"] = button == "right" ? "right_click" : count == 2 ? "double_click" : "click";
                action["description"] = StringArg(args, "description", action["action"]!.GetValue<string>());
                AddPoint(action, args); break;
            case "kero_drag":
                action["action"] = "drag"; AddPoint(action, args);
                action["endX"] = RequiredDouble(args, "end_x"); action["endY"] = RequiredDouble(args, "end_y");
                action["duration"] = IntArg(args, "duration_ms", 650); break;
            case "kero_long_press":
                action["action"] = "long_press"; AddPoint(action, args);
                action["duration"] = IntArg(args, "duration_ms", 720); break;
            case "kero_scroll":
                action["action"] = "scroll"; action["amount"] = IntArg(args, "amount", 0); break;
            case "kero_type_text":
                action["action"] = "type"; action["text"] = RequiredString(args, "text"); break;
            case "kero_press_key":
                action["action"] = "key"; action["key"] = RequiredString(args, "key"); break;
            case "kero_hotkey":
                action["action"] = "hotkey"; action["keys"] = args["keys"]?.DeepClone() ?? throw new ArgumentException("keys is required"); break;
            case "kero_hold_keys":
                action["action"] = "hold_keys"; action["keys"] = args["keys"]?.DeepClone() ?? throw new ArgumentException("keys is required");
                action["duration"] = IntArg(args, "duration_ms", 520); break;
            case "kero_open_app":
                action["action"] = "open_app"; action["text"] = RequiredString(args, "name"); break;
            case "kero_create_desktop_folder":
                action["action"] = "create_folder"; action["text"] = RequiredString(args, "name"); break;
            case "kero_wait":
                action["action"] = "wait"; action["duration"] = IntArg(args, "duration_ms", 800); break;
            default:
                throw new ArgumentException($"Unknown Kero tool: {tool}");
        }
        CopySemanticTarget(action, args);
        return action;
    }

    private static JsonObject BuildSequenceAction(JsonObject step)
    {
        var action = RequiredString(step, "action");
        return action switch
        {
            "move" => BuildAction("kero_move_pointer", step),
            "click" => BuildAction("kero_click", step),
            "double_click" => BuildAction("kero_click", With(step, "click_count", 2)),
            "right_click" => BuildAction("kero_click", With(step, "button", "right")),
            "drag" => BuildAction("kero_drag", step),
            "long_press" => BuildAction("kero_long_press", step),
            "scroll" => BuildAction("kero_scroll", step),
            "type" => BuildAction("kero_type_text", step),
            "key" => BuildAction("kero_press_key", step),
            "hotkey" => BuildAction("kero_hotkey", step),
            "hold_keys" => BuildAction("kero_hold_keys", step),
            "wait" => BuildAction("kero_wait", step),
            _ => throw new ArgumentException($"Unknown sequence action: {action}")
        };
    }

    private static JsonObject With(JsonObject source, string property, JsonNode value)
    {
        var copy = source.DeepClone().AsObject();
        copy[property] = value;
        return copy;
    }

    private static void AddPoint(JsonObject target, JsonObject args)
    {
        if (args["x"] is not null && args["y"] is not null)
        {
            target["x"] = RequiredDouble(args, "x");
            target["y"] = RequiredDouble(args, "y");
            return;
        }
        if (args["ui_target"] is null)
            throw new ArgumentException("x and y are required when ui_target is not provided");
    }

    private static void CopySemanticTarget(JsonObject target, JsonObject args)
    {
        if (args["ui_target"] is not null) target["uiTarget"] = args["ui_target"]!.DeepClone();
        if (args["ui_action"] is not null) target["uiAction"] = args["ui_action"]!.DeepClone();
    }

    private static string AutomaticDecision(string tool, JsonObject args) => tool switch
    {
        "kero_move_pointer" => $"移动鼠标到屏幕位置 ({RequiredDouble(args, "x"):P0}, {RequiredDouble(args, "y"):P0})",
        "kero_click" => StringArg(args, "description", IntArg(args, "click_count", 1) == 2 ? "双击当前目标" : "点击当前目标"),
        "kero_drag" => "拖动当前目标到新的位置",
        "kero_long_press" => "长按当前目标",
        "kero_scroll" => IntArg(args, "amount", 0) >= 0 ? "向上滚动当前页面" : "向下滚动当前页面",
        "kero_type_text" => "向当前输入框输入文字",
        "kero_press_key" => $"按下 {RequiredString(args, "key")} 键",
        "kero_hotkey" => "执行键盘快捷键",
        "kero_hold_keys" => "持续按住指定按键",
        "kero_open_app" => $"打开应用：{RequiredString(args, "name")}",
        "kero_create_desktop_folder" => $"创建桌面文件夹：{RequiredString(args, "name")}",
        "kero_wait" => "等待界面完成响应后复查",
        _ => "执行下一步电脑操作"
    };

    private static double RequiredDouble(JsonObject args, string name) =>
        args[name]?.GetValue<double>() ?? throw new ArgumentException($"{name} is required");
    private static string RequiredString(JsonObject args, string name) =>
        args[name]?.GetValue<string>() ?? throw new ArgumentException($"{name} is required");
    private static string StringArg(JsonObject args, string name, string fallback) => args[name]?.GetValue<string>() ?? fallback;
    private static int IntArg(JsonObject args, string name, int fallback) => args[name]?.GetValue<int>() ?? fallback;

    private static JsonObject TextResult(string text) => new()
    {
        ["content"] = new JsonArray { new JsonObject { ["type"] = "text", ["text"] = text } }
    };

    private static JsonObject UiInspectionResult(JsonObject response) => TextResult(UiInspectionText(response));

    private static string UiInspectionText(JsonObject response)
    {
        var window = response["window"]?.AsObject() ?? new JsonObject();
        var title = window["name"]?.GetValue<string>() ?? "(unnamed window)";
        var windowId = window["id"]?.GetValue<string>() ?? "";
        var windowClass = window["class"]?.GetValue<string>() ?? "";
        var fingerprint = response["fingerprint"]?.GetValue<string>() ?? "";
        var changed = Bridge.NoteUiFingerprint(fingerprint);
        var changeState = string.IsNullOrWhiteSpace(fingerprint) ? "unknown" : changed ? "changed" : "unchanged";
        var lines = new List<string> { $"Foreground window: {title} | class={windowClass} | id={windowId} | state={changeState}" };
        var controls = response["controls"]?.AsArray() ?? new JsonArray();
        foreach (var node in controls.Take(100))
        {
            if (node is not JsonObject control) continue;
            if (control["enabled"]?.GetValue<bool>() != true) continue;
            var id = control["id"]?.GetValue<string>() ?? "";
            var name = control["name"]?.GetValue<string>() ?? "";
            var type = control["controlType"]?.GetValue<string>() ?? "Unknown";
            var x = control["x"]?.GetValue<double>() ?? 0;
            var y = control["y"]?.GetValue<double>() ?? 0;
            lines.Add($"- id={id} | type={type} | name={name} | point=({x:F3},{y:F3})");
        }
        if (lines.Count == 1)
            lines.Add("No actionable accessibility controls were exposed. Use kero_screenshot for visual grounding.");
        else
            lines.Add("Use a listed id as ui_target with kero_click or kero_type_text. Do not screenshot just to click one of these controls.");
        return string.Join("\n", lines);
    }

    private static async Task<string> WaitForUiAsync(JsonObject args)
    {
        var windowNeedle = StringArg(args, "window_contains", "").Trim();
        var controlNeedle = StringArg(args, "control_contains", "").Trim();
        var controlType = StringArg(args, "control_type", "").Trim();
        if (windowNeedle.Length == 0 && controlNeedle.Length == 0)
            throw new ArgumentException("window_contains or control_contains is required");
        var expectedPresent = StringArg(args, "state", "present").Equals("present", StringComparison.OrdinalIgnoreCase);
        var timeoutMs = Math.Clamp(IntArg(args, "timeout_ms", 3000), 100, 15000);
        var pollMs = Math.Clamp(IntArg(args, "poll_ms", 180), 80, 1000);
        var deadline = Stopwatch.GetTimestamp() + (long)(Stopwatch.Frequency * timeoutMs / 1000d);
        JsonObject? last = null;
        do
        {
            last = await Bridge.RequestAsync(new JsonObject { ["op"] = "inspect_ui" });
            var matched = UiConditionMatches(last, windowNeedle, controlNeedle, controlType);
            if (matched == expectedPresent)
            {
                var state = expectedPresent ? "present" : "absent";
                return $"UI condition is {state}.\n{UiInspectionText(last)}";
            }
            await Task.Delay(pollMs);
        }
        while (Stopwatch.GetTimestamp() < deadline);
        var expected = expectedPresent ? "appear" : "disappear";
        throw new TimeoutException($"Timed out waiting for the requested UI condition to {expected}. Last observation: {UiInspectionText(last ?? new JsonObject())}");
    }

    private static bool UiConditionMatches(JsonObject response, string windowNeedle, string controlNeedle, string controlType)
    {
        var window = response["window"]?.AsObject() ?? new JsonObject();
        var windowText = string.Join(" ", new[]
        {
            window["name"]?.GetValue<string>() ?? "",
            window["class"]?.GetValue<string>() ?? "",
            window["id"]?.GetValue<string>() ?? ""
        });
        var windowMatches = windowNeedle.Length == 0 || windowText.Contains(windowNeedle, StringComparison.OrdinalIgnoreCase);
        var controls = response["controls"]?.AsArray() ?? new JsonArray();
        var controlMatches = controlNeedle.Length == 0 && controlType.Length == 0;
        if (!controlMatches)
        {
            controlMatches = controls.OfType<JsonObject>().Any(control =>
            {
                var text = string.Join(" ", new[]
                {
                    control["id"]?.GetValue<string>() ?? "",
                    control["name"]?.GetValue<string>() ?? ""
                });
                var type = control["controlType"]?.GetValue<string>() ?? "";
                return (controlNeedle.Length == 0 || text.Contains(controlNeedle, StringComparison.OrdinalIgnoreCase))
                    && (controlType.Length == 0 || type.Equals(controlType, StringComparison.OrdinalIgnoreCase));
            });
        }
        return windowMatches && controlMatches;
    }

    private static async Task<JsonObject> ImageResultAsync(JsonObject response, string prompt)
    {
        var images = response["images"]?.AsArray() ?? throw new InvalidOperationException("Kero returned no generated images.");
        var content = new JsonArray();
        foreach (var node in images)
        {
            if (node is not JsonObject image) continue;
            var path = image["path"]?.GetValue<string>() ?? throw new InvalidOperationException("Kero returned an image without a path.");
            if (!File.Exists(path)) throw new FileNotFoundException("Kero generated image file is unavailable.", path);
            var extension = Path.GetExtension(path).ToLowerInvariant();
            var mimeType = extension switch
            {
                ".jpg" or ".jpeg" => "image/jpeg",
                ".webp" => "image/webp",
                _ => "image/png"
            };
            var base64 = Convert.ToBase64String(await File.ReadAllBytesAsync(path));
            content.Add(new JsonObject
            {
                ["type"] = "text",
                ["text"] = $"Kero generated image saved at: {path}\nPrompt: {image["prompt"]?.GetValue<string>() ?? prompt}"
            });
            content.Add(new JsonObject { ["type"] = "image", ["data"] = base64, ["mimeType"] = mimeType });
        }
        if (content.Count == 0) throw new InvalidOperationException("Kero returned an empty image list.");
        return new JsonObject { ["content"] = content };
    }

    private static JsonObject RpcResult(JsonNode id, JsonNode result) => new()
    {
        ["jsonrpc"] = "2.0", ["id"] = id, ["result"] = result
    };

    private static JsonObject RpcError(JsonNode id, int code, string message) => new()
    {
        ["jsonrpc"] = "2.0", ["id"] = id,
        ["error"] = new JsonObject { ["code"] = code, ["message"] = message }
    };
}

internal sealed class BridgeClient
{
    private const int Port = 47821;
    private readonly SemaphoreSlim gate = new(1, 1);
    private bool sessionActive;
    private string colorMode = "rainbow";
    private string? lastUiFingerprint;
    private TcpClient? bridgeClient;
    private StreamReader? bridgeReader;
    private StreamWriter? bridgeWriter;

    public bool NoteUiFingerprint(string fingerprint)
    {
        if (string.IsNullOrWhiteSpace(fingerprint)) return true;
        var changed = lastUiFingerprint is null || !StringComparer.Ordinal.Equals(lastUiFingerprint, fingerprint);
        lastUiFingerprint = fingerprint;
        return changed;
    }

    public async Task StartSessionAsync(string requestedColorMode)
    {
        colorMode = requestedColorMode == "blue" ? "blue" : "rainbow";
        await EnsureKeroAsync();
        await RequestCoreAsync(new JsonObject { ["op"] = "start", ["colorMode"] = colorMode });
        sessionActive = true;
        lastUiFingerprint = null;
    }

    public async Task EnsureSessionAsync()
    {
        if (sessionActive) return;
        await StartSessionAsync(colorMode);
    }

    public async Task StopSessionAsync()
    {
        if (!await CanConnectAsync())
        {
            sessionActive = false;
            return;
        }
        await RequestCoreAsync(new JsonObject { ["op"] = "stop" });
        sessionActive = false;
        lastUiFingerprint = null;
    }

    public async Task<JsonObject> RequestAsync(JsonObject request)
    {
        await EnsureKeroAsync();
        return await RequestCoreAsync(request);
    }

    public void TryStopOnExit()
    {
        if (!sessionActive) return;
        try { StopSessionAsync().Wait(TimeSpan.FromSeconds(2)); } catch { }
    }

    private async Task EnsureKeroAsync()
    {
        // A running control session already proved the bridge is available.
        // Avoid a separate TCP ping before every small UI operation.
        if (sessionActive) return;
        if (await CanConnectAsync()) return;
        var path = LocateKero();
        Process.Start(new ProcessStartInfo(path) { UseShellExecute = true, WorkingDirectory = Path.GetDirectoryName(path)! });
        for (var attempt = 0; attempt < 60; attempt++)
        {
            await Task.Delay(250);
            if (await CanConnectAsync()) return;
        }
        throw new InvalidOperationException($"Kero started but its MCP bridge did not become available: {path}");
    }

    private async Task<bool> CanConnectAsync()
    {
        try
        {
            var result = await RequestCoreAsync(new JsonObject { ["op"] = "ping" }, 1200);
            return result["bridgeVersion"]?.GetValue<int>() == 1;
        }
        catch { return false; }
    }

    private async Task<JsonObject> RequestCoreAsync(JsonObject request, int timeoutMs = 185000)
    {
        await gate.WaitAsync();
        try
        {
            using var timeout = new CancellationTokenSource(timeoutMs);
            await EnsureBridgeConnectionAsync(timeout.Token);
            await bridgeWriter!.WriteLineAsync(request.ToJsonString());
            var responseLine = await bridgeReader!.ReadLineAsync(timeout.Token) ?? throw new IOException("Kero bridge closed without a response.");
            var response = JsonNode.Parse(responseLine)?.AsObject() ?? throw new IOException("Kero bridge returned invalid JSON.");
            if (response["ok"]?.GetValue<bool>() != true)
                throw new InvalidOperationException(response["error"]?.GetValue<string>() ?? "Kero bridge operation failed.");
            return response["result"]?.AsObject() ?? new JsonObject();
        }
        catch (InvalidOperationException)
        {
            // A valid bridge response may report that an action was rejected.
            // Keep the local transport open for the agent's next decision.
            throw;
        }
        catch
        {
            DisposeBridgeConnection();
            sessionActive = false;
            throw;
        }
        finally { gate.Release(); }
    }

    private async Task EnsureBridgeConnectionAsync(CancellationToken cancellationToken)
    {
        if (bridgeClient?.Connected == true && bridgeReader is not null && bridgeWriter is not null) return;
        DisposeBridgeConnection();
        var client = new TcpClient();
        try
        {
            await client.ConnectAsync("127.0.0.1", Port, cancellationToken);
            var stream = client.GetStream();
            bridgeClient = client;
            bridgeWriter = new StreamWriter(stream, new UTF8Encoding(false), leaveOpen: true) { AutoFlush = true };
            bridgeReader = new StreamReader(stream, new UTF8Encoding(false), leaveOpen: true);
        }
        catch
        {
            client.Dispose();
            throw;
        }
    }

    private void DisposeBridgeConnection()
    {
        try { bridgeWriter?.Dispose(); } catch { }
        try { bridgeReader?.Dispose(); } catch { }
        try { bridgeClient?.Dispose(); } catch { }
        bridgeWriter = null;
        bridgeReader = null;
        bridgeClient = null;
    }

    private static string LocateKero()
    {
        var candidates = new[]
        {
            Environment.GetEnvironmentVariable("KERO_APP_PATH"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Programs", "Kero", "Kero.exe"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "Kero", "Kero.exe"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Kero", "kero.exe"),
            @"C:\Users\32910\Desktop\桌面资料\01_开发项目\Kero\src-tauri\target\release\kero.exe",
            @"D:\软件\Kero\kero.exe"
        };
        return candidates.FirstOrDefault(path => !string.IsNullOrWhiteSpace(path) && File.Exists(path))
            ?? throw new FileNotFoundException("Kero.exe was not found. Set KERO_APP_PATH to the current Kero executable.");
    }
}
