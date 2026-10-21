# agent-shit-cleaner

> AI agent 在你家拉了多少屎？扫一下就知道 —— `asc`
> Scan and reclaim the gigabytes AI coding agents leave behind.

一个 Rust 写的 agent 垃圾清理工具：**TUI + CLI 双模式**，只读扫描、人工确认、进回收站。

```
$ asc scan ~/Documents
── 工作区 ~/Documents ── 727 个垃圾目录 · 173.2 GB
node_modules ×250       ████████████████░░░░░░░░  96.4 GB
.next ×112             ████░░░░░░░░░░░░░░░░░░░░  21.7 GB
__pycache__ ×284       ██░░░░░░░░░░░░░░░░░░░░░░   8.9 GB
```

## 两大战场

| 战场 | 目标 | 典型体积 |
|---|---|---|
| **Agent 大本营** | `~/.claude`、`~/.codex`、`~/.cursor` 等 12 家 agent 的 sessions/logs/caches/telemetry | 单机实测 14.3 GB |
| **Agent 工作区** | 项目里的 `node_modules` / `target` / `.venv` / `.next` / `__pycache__` … | 727 个目录 / 170 GB+ |

内置 **51 条规则**（46 大本营 + 5 项目级 Tier B），另有**工作区自动发现**：从 claude/codex/cursor 的会话元数据反推你所有项目根，不用手动配路径。

## 安装

```bash
# 从源码（需要 Rust 1.75+）
cargo install --git https://github.com/Viyozc/agent-shit-cleaner

# 或从 Release 下载静态二进制（macOS / Linux / Windows）
# https://github.com/Viyozc/agent-shit-cleaner/releases
```

## 快速开始

```bash
asc                # TUI：仪表盘 → 勾选 → 确认 → 清理
asc scan           # 只读：大本营 + 自动发现的项目根
asc scan ~/Documents   # 只读：指定工作区
asc clean --dry-run    # 计划预览（脚本/CI 唯一可用模式）
asc doctor         # 只读诊断：检测到的 agent / 规则 / 自动发现 / 回收站
asc trash list     # 回收站（删除先进回收站，7 天可反悔）
```

## 安全设计（为什么敢用）

- **零自动删除**：删除必须人工确认（y/N），大件（>1GB）粉碎要逐字输入 `DELETE`。不存在 `--yes` / `--force`，非交互环境只能 `--dry-run`
- **NEVER 黑名单终审**：`.git`、`CLAUDE.md`、`settings*`、`.credentials*`、`*.lock`、`memory/`、`skills/` 等在执行层强制拦截，规则写错也删不掉
- **回收站兜底**：删除 = 移入 `~/.local/share/asc-trash/`（manifest 记录原路径，`asc trash restore` 一键恢复，冲突落 `.restored-<id>`）
- **风险分级**：`safe`（可再生）/ `retention`（默认 90 天保留期）/ `redownload`（需显式勾选，默认不选）
- **工作区护栏**：`node_modules` 必须同级有 `package.json`；无清单标 ⚠ 默认跳过；**活跃项目**（源码 30 天内有改动）默认跳过
- **不碰的东西**：模型文件、agent 本体、配置/凭证/记忆（windsurf 的 memories、`~/.config/goose` 的 secrets、`~/.qwen` 项目级提示词——全部故意不在规则内）

## 配置（全部可省略）

`~/.config/asc/config.toml`，示例见 [config.example.toml](config.example.toml)：

```toml
tier_b = false          # dist/build/out 等歧义目录名，默认关
active_days = 30        # 项目活跃判定窗口
trash_keep_days = 7     # 回收站保留期

[[workspace]]           # 工作区根（也可靠自动发现）
root = "~/Documents"

[keep_days]             # 按规则 id 覆盖保留期
"claude-code/sessions" = 30
```

自定义规则放 `~/.config/asc/rules.d/*.toml`，与内置规则同 id 则覆盖。

## 支持的 agent

claude-code · codex · cursor · trae · gemini/antigravity · opencode · windsurf · continue · goose · crush · amp · copilot-cli · qwen-code
项目级：`.crush` · `.specstory` · `node_modules` · `target` · `.venv` · `__pycache__` · `.next` · `.nuxt` · `.turbo` · `.gradle`（Tier B：`dist` · `build` · `out` · `coverage` · `.cache`）

规则全部经官方文档/issue 核实后收录，PR 加规则欢迎。

## License

MIT
