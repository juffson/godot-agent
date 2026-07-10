# Godot Agent 使用指南

在 Godot 编辑器和运行中的游戏里内嵌 MCP server，让 AI 助手（Claude Code 等）
直接读取、操作、调试你的项目。零 Node 依赖，一个 Rust 动态库搞定。

## 安装

```bash
git clone https://github.com/juffson/godot-agent.git
cd godot-agent
./install.sh /path/to/你的项目 --release
```

用 Godot 打开项目即完成：插件自动加载（无需手动启用），并自动注册游戏侧
autoload。编辑器 Output 面板会打印：

```
[MCP] Editor MCP server listening on http://127.0.0.1:6010/mcp
```

**更新插件后需要重启编辑器**（动态库在启动时加载）。

## 两个端点

| 端点 | 进程 | 什么时候可用 | 用途 |
|---|---|---|---|
| `:6010/mcp` | 编辑器 | 编辑器开着就有 | 改场景、看编辑中的节点树、启动/停止游戏、编辑器侧脚本 |
| `:6011/mcp` | 游戏 | 游戏运行时才有 | 模拟输入、截图、运行时节点树、游戏侧脚本 |

端口可用环境变量改：`GODOT_MCP_HTTP_PORT`（编辑器）、`GODOT_AGENT_GAME_PORT`（游戏）。

## 接入 Claude Code

```bash
claude mcp add --transport http godot-editor http://127.0.0.1:6010/mcp
claude mcp add --transport http godot-game   http://127.0.0.1:6011/mcp
```

注意：MCP 工具列表在**会话启动时**获取。想让会话用上游戏侧工具，先把游戏
跑起来再开新会话（编辑器里的 AI Chat 面板同理——先跑游戏再点 New）。

## AI Chat 面板

编辑器右侧和「检查器」同排的 **AI Chat** 标签。输入即聊，Enter 发送：

- 用你本机已登录的 Claude Code 账号（无需 API key）
- 会话自动配好两个 MCP 端点，工作目录就是项目根目录（能直接读写 GDScript 文件）
- 工具调用以 `⚙ 工具名` 灰色行显示；**New** 结束当前对话开新会话
- 权限：可用全部编辑器/游戏工具 + 文件编辑；Bash 等命令被禁用

## 典型调试场景

### 1. 检查 UI 结构（结构化读取，别用截图猜）

```
"列出当前场景所有按钮的文本、坐标和禁用状态"
```

AI 会用 `execute_script` 精确拿到数据。适合查：控件为什么点不到、布局树
挂在哪、信号连了没有。

### 2. 复现玩家操作

```
"启动游戏，点击登录按钮，确认场景切换到了角色选择"
```

`simulate_input` 走真实输入管线（事件自动分帧，press/release 成对），
支持：`key`（按键）、`text`（打字）、`mouse_click` / `mouse_move`、
`action`（输入映射动作如 ui_accept）、`wait`（等待）。

### 3. 视觉验证

```
"截图看看现在的画面，标题和按钮有没有重叠"
```

截图返回渲染像素（自动缩到 1280 宽）——布局错位、贴图丢失、主题不生效
这类问题只有"看"才能发现。逻辑状态用结构化读取，视觉效果用截图，互补。

### 4. 读写运行时状态

```
"读一下 GameState 里当前角色的数据"
"把玩家传送到坐标 (100, 200)"
```

`execute_script`（游戏侧）在游戏进程里跑任意 GDScript，
用 `Engine.get_main_loop()` 拿 SceneTree，autoload 单例直接用名字访问。

## 已知边界

- **脚本运行时错误会立刻返回错误信息**（数组越界、空引用等），不会挂起请求
- 按键暂不支持修饰键组合（Ctrl/Shift/Alt + 键）
- 截图需要真实渲染上下文（游戏正常带窗口运行即可）
- 联机流程里需要服务端配合的部分（如真实对战判定），AI 能操作输入、读状态、
  看画面，但无法替代服务端逻辑
- 游戏重启后 `:6011` 会重新监听；已建立的 MCP 会话无需重连（HTTP 无状态）

## 安全

两个 server 都只绑定 `127.0.0.1`。`execute_script` 是任意代码执行，
**不要端口转发或暴露到局域网**。
