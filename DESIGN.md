# agent-shit-cleaner 设计文档

> 扫描并清理各种 AI coding agent 在你机器上拉出的"屎"，带好看的终端可视化界面。
> 二进制短名：`asc`

---

## 1. 痛点（来自本机真实扫描，2026-10-21）

| Agent | 目录 | 总大小 | 主要构成 |
|---|---|---|---|
| Cursor | `~/.cursor` | **11 GB** | chats 7.7G（1636 个聊天文件！）、extensions 2.5G、ai-tracking 559M |
| Codex | `~/.codex` | 1.5 GB | packages 648M、plugins 308M、sessions 127M、logs.sqlite 70M |
| Trae | `~/.trae` | 822 MB | extensions 819M |
| Claude Code | `~/.claude` | 588 MB | projects 507M（584 份会话转录，21 份超过 180 天） |
| Gemini | `~/.gemini` | 202 MB | antigravity-browser-profile 101M |
| OpenCode | `~/.opencode` | 182 MB | bin 123M、node_modules 57M |
| **合计** | | **≈ 14.3 GB** | （不含 ollama 33G 模型，按决策不纳入） |

**战场二：工作区垃圾（agent 干过活的项目目录，~/Documents 实测，2026-10-21）**

| 类型 | 数量 | 说明 |
|---|---|---|
| `node_modules` | 250 个 | 每个从几十 MB 到 1G+ 不等 |
| `__pycache__` | 284 个 | Python 字节码缓存 |
| `.next` | 112 个 | Next.js 构建产物 |
| `.turbo` / `target` / `.venv` / `venv` / `.gradle` | 81 个 | monorepo 缓存、Rust 构建产物、Python 虚拟环境 |
| **合计** | **727 个目录** | **仅前 400 个就 ≈ 110 GB**（比战场一大近一个数量级） |

提炼成六个核心痛点：

- **P1 静默膨胀**：agent 从不告诉你它存了什么。Cursor 一个聊天目录干到 7.7G，用户毫无感知。
- **P2 隐私风险**：会话转录（jsonl/sqlite）包含完整代码、路径、常被讨论的密钥。机器转手 / 录屏 / 合规审计时是定时炸弹。
- **P3 不知道什么能删**：`packages`？`ai-tracking`？`shell-snapshots`？删错会把 agent 弄坏或登出，用户只能选择不动。
- **P4 分散无总览**：6+ 个 agent、几十个子目录，没有工具能回答"我的磁盘被哪些 agent 吃了、各吃多少"。
- **P5 删除不可逆恐惧**：`rm -rf` 手抖一次就完了，缺少"回收站"式的后悔药。
- **P6 项目坟场**：agent 一次性试验项目遍地开花，`node_modules`/构建产物随项目永存——项目本身死了几百 MB 到几个 G 的依赖还在。一个 `~/Documents` 就积了 110G+。

## 2. 产品定位

**一句话**：AI agent 时代的 `ncdu` + `bleachbit` —— 扫描出垃圾、可视化展示、安全清除、可反悔。

**两大垃圾战场**：
1. **Agent 大本营**：`~/.claude`、`~/.cursor` 等固定路径的会话/日志/缓存/扩展
2. **Agent 工作区**：项目目录里的 `node_modules`、`target`、`.venv`、构建产物等可再生依赖

**非目标（明确不做）**：
- 不卸载/管理 agent 本体
- 不管理模型文件（ollama/hf models，33G 那个坑不碰）
- 不碰用户配置、凭证、自建 skills/memory
- 不做后台驻留进程，纯按需运行

## 3. 设计原则（优先级从高到低）

1. **安全 > 空间 > 速度 > 好看**。宁可少删 1G，不可错删一个文件。
2. **白名单规则驱动**：只删内置/用户规则明确匹配的路径，永不"智能猜测"。
3. **默认 dry-run**：任何破坏性操作前，用户先看到将要发生什么。
4. **一切先进回收站**：删除 = 移动到 `~/.local/share/asc-trash/`，保留 7 天（可配），启动时自动清过期。
5. **零自动删除**：删除必须人工确认，不存在跳过确认的参数（没有 `--yes` / `--force`）。脚本和 CI 只能 dry-run。
6. **两档清理意图分离**：按保留策略清理（默认安全）vs 全量清理（激进，二次确认）。

## 4. 核心概念

| 概念 | 说明 |
|---|---|
| **Agent** | 一个 AI 编码工具（claude-code / codex / cursor / trae / gemini / opencode…） |
| **Workspace** | 一个项目目录。来源：显式路径参数 / 配置的扫描根 / 从 agent 会话元数据自动发现 |
| **Rule** | 一条清理规则：`home` 规则（固定路径）或 `workspace` 规则（目录名模式，作用于任意项目内） |
| **Category** | 垃圾类别：`sessions` / `logs` / `caches` / `telemetry` / `packages` / `temp` / `build` |
| **Risk level** | `safe`（可再生）/ `retention`（按保留期删）/ `redownload`（可再下载但影响体验） |
| **JunkItem** | 扫描出的一个可清理单元（文件或目录），带 size / mtime / 规则来源 |

### 风险分级（关键设计，解决 P3）

| 级别 | 含义 | 例子 | 默认策略 |
|---|---|---|---|
| `safe` | 删了下次自动再生 | shell-snapshots、paste-cache、tmp、cache | 直接可清 |
| `retention` | 有价值但会过期 | 会话转录、聊天记录、file-history、snapshots | 默认保留 90 天，超期可清 |
| `redownload` | 可重新下载 | extensions、packages、plugins、node_modules、bin | 单独分区展示，需显式勾选 |
| **NEVER** | 永不触碰 | `.credentials.json`、settings、config.toml、skills/、memory、`CLAUDE.md`、`.claude.json` | 规则引擎硬编码黑名单兜底 |

工作区垃圾全部归入 `build` 类别 / `redownload` 风险级——依赖和构建产物可通过包管理器/编译重生，但删除会拖慢下次开发，因此**默认不勾选，需显式确认**。

## 5. 内置规则表（首期 6 个 agent + 工作区规则）

### 5.1 Agent 大本营规则（固定路径）

```toml
# rules/ 内置包，编译时 embed，用户可在 ~/.config/asc/rules.d/ 追加或覆盖
[[rule]]
id = "claude-code/projects"
agent = "claude-code"
path = "~/.claude/projects"
pattern = "*.jsonl"
category = "sessions"
risk = "retention"
keep_days = 90
description = "会话转录（含完整对话与代码）"

[[rule]]
id = "claude-code/shell-snapshots"
agent = "claude-code"
path = "~/.claude/shell-snapshots"
category = "temp"
risk = "safe"
keep_days = 7

[[rule]]
id = "cursor/chats"
agent = "cursor"
path = "~/.cursor/chats"
category = "sessions"
risk = "retention"
keep_days = 90

[[rule]]
id = "cursor/extensions"
agent = "cursor"
path = "~/.cursor/extensions"
category = "packages"
risk = "redownload"

[[rule]]
id = "cursor/ai-tracking"
agent = "cursor"
path = "~/.cursor/ai-tracking"
category = "telemetry"
risk = "safe"
keep_days = 30

[[rule]]
id = "codex/sessions"
agent = "codex"
path = "~/.codex/sessions"
category = "sessions"
risk = "retention"
keep_days = 90

[[rule]]
id = "codex/logs-sqlite"
agent = "codex"
path = "~/.codex/logs_*.sqlite"
category = "logs"
risk = "retention"
keep_days = 14

[[rule]]
id = "codex/packages"
agent = "codex"
path = "~/.codex/packages"
category = "packages"
risk = "redownload"

# ... trae/extensions、gemini/tmp、gemini/browser-profile、
#     opencode/bin、opencode/node_modules 同理
```

### 5.2 工作区规则（scope = workspace，目录名模式匹配，作用于项目内任意层级）

```toml
[[rule]]
id = "workspace/node_modules"
scope = "workspace"
match_dir = "node_modules"
require_sibling = "package.json"   # 安全启发：同级有清单才认定可再生，无则标注 ⚠
category = "build"
risk = "redownload"
description = "npm/pnpm/yarn 依赖，可重装（npm install）"

[[rule]]
id = "workspace/rust-target"
scope = "workspace"
match_dir = "target"
require_sibling = "Cargo.toml"
category = "build"
risk = "redownload"

[[rule]]
id = "workspace/python-venv"
scope = "workspace"
match_dir = "{.venv,venv}"
require_sibling = "{pyproject.toml,requirements*.txt,setup.py}"
category = "build"
risk = "redownload"

[[rule]]
id = "workspace/pycache"
scope = "workspace"
match_dir = "__pycache__"
category = "build"
risk = "safe"          # 纯缓存，随时再生

[[rule]]
id = "workspace/js-build"
scope = "workspace"
match_dir = "{.next,.nuxt,.turbo,.gradle}"
category = "build"
risk = "redownload"
```

**两档模式列表**：

- **Tier A（默认开启）**：`node_modules`、`target`、`.venv`/`venv`、`__pycache__`、`.next`、`.nuxt`、`.turbo`、`.gradle` —— 目录名语义唯一，误伤概率极低
- **Tier B（默认关闭，配置中显式开启）**：`dist`、`build`、`out`、`coverage`、`.cache` —— 目录名有歧义（有的项目把 `dist`/`build` 当源码或发布物提交进仓库），宁可放过

**工作区 NEVER 补充**：`.git/**`（永不进入）、`*.lock`、`.env*`、`go.sum` 等锁文件与清单文件本身。

**NEVER 黑名单**（规则引擎最后兜底，任何规则命中也拒删）：
`**/.credentials*`、`**/settings*.json`、`**/config.toml`、`**/CLAUDE.md`、`**/memory/**`、`**/skills/**`、`**/hooks/**`、`**/agents/**`、`~/.claude.json`、`**/todos/**`

## 6. CLI 设计

```
asc                          # 默认进 TUI（交互模式）
asc scan                     # 非交互扫描两大战场，输出摘要表（CI/脚本友好）
asc scan ~/Documents         # 只扫某个工作区根目录
asc scan --agent-only        # 只扫 agent 大本营，不扫工作区
asc clean                    # 交互式清理（= TUI 的快捷入口），执行前必须 y/N 确认
asc clean --days 30          # 调整保留策略，仍需确认后才执行
asc clean --dry-run          # 只展示将删除的内容（脚本/CI 唯一可用模式）
asc clean --category sessions,build --agent cursor,codex
asc clean --privacy          # 快捷场景：清掉全部会话/聊天（含未过期），机器转手前用
asc trash list               # 回收站列表
asc trash restore <id>       # 恢复
asc trash empty              # 清空回收站（真正删除）
asc rules list               # 查看生效规则（内置 + 用户 + 覆盖关系）
asc doctor                   # 体检：各 agent 安装状态、目录大小、规则健康度
asc --version / --help
```

`scan` 输出示例：

```
AGENT        TOTAL    RECLAIMABLE  BREAKDOWN
cursor       11.0 GB  7.7 GB       chats 7.7G · tracking 559M · ext 2.5G*
codex         1.5 GB  527 MB       sessions 127M · logs 70M · packages 648M*
claude-code  588 MB   521 MB       sessions 507M · temp 12M
...
TOTAL        14.3 GB  8.8 GB       (* = redownload 级，需单独确认)

WORKSPACE    ~/Documents            727 个目录
node_modules ×250  ████████████████████████░░  71.2 GB
.next ×112         ████░░                            18.6 GB
pycache/venv/...   ██░░                               9.8 GB
TOP  ~/Documents/Personal/vibeProduct        3.2 GB
     ~/Documents/ai-space/openppt            2.8 GB
WORKSPACE TOTAL  110+ GB  (build 级，需显式勾选)
```

## 7. TUI 设计（ratatui）

### 7.1 Dashboard（首屏）

```
╭─ agent-shit-cleaner ──── 扫描 5.4s ── 193,507 个文件 ─────────────────────╮
│                                                                           │
│  大本营 8.8 GB · 工作区 110+ GB                        [c]lean  [?]help    │
│                                                                           │
│  ── Agent 大本营 ────────────────────────────────────────────────────────  │
│  cursor        ██████████████████████████████████░░░░░░  7.7 GB  1636项    │
│  codex         ██████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  527 MB   80项     │
│  claude-code   █████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  521 MB  584项     │
│  trae          ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  —        (仅扩展) │
│  gemini        ██░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  141 MB   38项     │
│  opencode      █░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  180 MB   12项     │
│                                                                           │
│  ── 工作区 ~/Documents ── 727 个垃圾目录 ────────────────────────────────  │
│  node_modules ×250 ████████████████████████████████░░░░  71.2 GB          │
│  .next ×112        ███████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  18.6 GB          │
│  其他 ×365         ███░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   9.8 GB          │
│  最肥: vibeProduct 3.2G · openppt 2.8G · captok 2.1G …                     │
╰───────────────────────────────────────────────────────────────────────────╯
  ↑↓选择  →进入  space勾选  w大本营/工作区切换  r排序  c清理  q退出
```

- 每个 agent 一个**彩色水平条**（比例 + 绝对值），红/黄/绿按风险级着色
- 扫描期间有进度动画（当前路径 + 文件计数滚动）

### 7.2 Agent 详情 / 文件表

```
╭─ cursor ── chats ────────────────────────────────────── 7.7 GB ──────────╮
│ ▸ ~/.cursor/chats/                                          [7.7 GB]      │
│     ├ 2025-03-session-8f3k.jsonl            14 天   212 MB                │
│     ├ 2025-03-session-9dla.jsonl            14 天   198 MB                │
│     ├ 2024-11-session-2kdm.jsonl           311 天   171 MB   ◂ 超期       │
│     └ ...                                                                  │
│                                                                            │
│  规则: cursor/chats · retention 90天 · 1,233/1,636 项超期可清               │
╰────────────────────────────────────────────────────────────────────────────╯
  a全选超期  space勾选  tab切类别  ←返回
```

### 7.3 工作区视图

```
╭─ ~/Documents ── build 110 GB ── 727 项 ──────────────────────────────────╮
│  ☐ ~/Documents/Personal/vibeProduct/node_modules     3.2 GB   46 天       │
│  ☑ ~/Documents/ai-space/openppt/node_modules         2.8 GB  118 天  死项目│
│  ☑ ~/Documents/Personal/captok/node_modules          2.1 GB  203 天  死项目│
│  ☑ ~/Documents/Personal/captok/server/node_modules   1.4 GB  203 天       │
│  ☐ ~/Documents/Temp/node_modules                     1.1 GB    3 天  ⚠活跃 │
│  ...                                                                      │
│                                                                           │
│  ⚠ 3 个 node_modules 无 lockfile（重装可能装出不同版本），默认不勾选         │
│  活跃判定: 项目源码 30 天内有改动 → 标记 ⚠活跃，默认跳过                    │
╰───────────────────────────────────────────────────────────────────────────╯
  space勾选  A勾选全部死项目  s按项目分组  ←返回  c清理
```

- 按"死项目/活跃"区分是工作区视图的灵魂：**死项目**（源码 N 天未改动）的依赖是最优质的垃圾——体积大、删了也不心疼
- 无 lockfile 的 node_modules 标 ⚠ 默认不选（重装版本可能漂移）

### 7.4 确认 & 结果

```
╭─ 确认清理 ─────────────────────────────────────────────────────╮
│  将删除 6.2 GB (2,054 项) → 回收站（保留 7 天）                  │
│    sessions   5.8 GB   1,871 项   ⚠ 含未过期项(--privacy 模式)   │
│    telemetry  402 MB     183 项                                 │
│                                                                  │
│        [ Enter 确认 ]    [ e 查看明细 ]    [ Esc 取消 ]           │
╰──────────────────────────────────────────────────────────────────╯

╭─ ✅ 清理完成 ───────────────────────────────────────────────────╮
│      ██  6.2 GB  释放██                                          │
│      2,054 个文件已进回收站 · asc trash restore 可反悔           │
╰──────────────────────────────────────────────────────────────────╯
```

## 8. 安全机制汇总

| 机制 | 实现 |
|---|---|
| 白名单 | 只删规则命中项，无"全盘智能识别" |
| NEVER 兜底黑名单 | 路径匹配器最后一道闸，凭证/配置/记忆永不删 |
| dry-run 默认 | 非交互模式只有 dry-run；**没有任何跳过人工确认的参数**，TUI 中 Enter / CLI 中 y/N 必须亲自按下 |
| 回收站 | 移动到 `~/.local/share/asc-trash/<run-id>/`，附 `manifest.toml`（原路径+时间+规则id）；**过期不自动清**，需 `asc trash empty --expired` 显式确认后清（M1 决策：回收站清除也属删除，遵循人工确认原则） |
| 激进操作二次确认 | `redownload` 级、`--privacy`、含未过期项的清理，均要求额外按键确认 |
| 工作区护栏 | `require_sibling` 启发式（node_modules 需同级 package.json）；无 lockfile 标 ⚠ 默认不选；**活跃项目**（源码 30 天内有改动）默认跳过；`.git` 永不进入；不跟随符号链接 |
| 大件直接粉碎 | build 类体积巨大，进回收站不省磁盘 → 对单个 >1GB 的项提供"粉碎（不进回收站）"选项，要求**键入 `DELETE` 短语**确认，比 Enter 更重 |
| 原子性 | 逐项移动，失败逐项记录并继续（M1 决策：大批量时单个锁定文件不应中断整批），结束时输出完整 已删/失败 清单 |
| 只读扫描 | 扫描阶段零写入 |

## 9. 架构

```
agent-shit-cleaner/
├── crates/
│   ├── asc-core/        # 规则解析、路径匹配、扫描器(walkdir+parallel)、trash
│   ├── asc-tui/         # ratatui 界面
│   └── asc-cli/         # clap 入口，组装 core/tui
├── rules/               # 内置规则包（TOML，embed 进二进制）
└── tests/               # golden 测试：用 fixture 目录树验证扫描/清理
```

关键库：`clap`（CLI）、`ratatui + crossterm`（TUI）、`jwalk`/`rayon`（并行扫描）、`dirs`（home 解析）、`toml`、`human_bytes`（3.2 GB 而非 3435989760）。

扫描器要点：
- **剪枝**：命中 workspace 规则的目录直接计大小、不再向内遍历（`find -prune` 语义），否则光遍历 250 个 node_modules 就要几分钟
- 工作区根来自：CLI 路径参数 + `config.toml` 的 `[[workspace]] root/ignore/max_depth` + agent 会话元数据自动发现（`~/.claude/projects/` 的目录名即路径编码，agent 自己交代了它在哪拉过屎）
- 性能目标：20 万文件扫描 < 5s；TUI 冷启动 < 100ms

## 10. 路线图

- **M0 扫描器**：`asc scan` 两大战场（agent 大本营 + 工作区 Tier A 规则）+ 摘要表 —— 第一天就能证明价值（光摘要就能吓人一跳）✅
- **M1 TUI + 清理**：dashboard / 详情 / 工作区视图 / 勾选 / 确认 / 回收站 / 大件粉碎 ✅
- **M2 策略与配置** ✅（2026-10-21 完成，27 测试全过）：
  - `~/.config/asc/config.toml`：`[[workspace]] root/ignore`、`[keep_days]` 按规则 id 覆盖保留期、`tier_b`、`active_days`（默认 30）、`trash_keep_days`（默认 7）；配置损坏回退默认值并告警，示例见 `config.example.toml`
  - 用户规则 `~/.config/asc/rules.d/*.toml`：与内置同 id 覆盖、新 id 追加；坏文件跳过出警告不阻断
  - Tier B 规则入包（dist/build/out/coverage/.cache，带 `tier = "b"` 字段），默认不参与扫描
  - 死项目/活跃判定：垃圾目录父目录（=项目）内源文件（剪掉垃圾目录与 .git、深度 ≤4）mtime < active_days → 活跃；CLI 默认跳过活跃项（`--include-active` 才清理）、TUI 标 ⚡活跃项目 且组勾选默认排除；根直下命中保守视为活跃；同项目缓存一次判定
  - 接线：`asc config` 查看生效配置；TUI/clean 自动合并 CLI 路径与 config 工作区根；`trash empty --expired` 用配置的保留天数；修复 TUI `r` 重扫不再起扫描线程的 M1 遗留 bug
- **M3 生态** ✅（2026-10-21 完成，33 测试全过；分发除外）：
  - 更多 agent 规则（全部官方文档/issue 核实后收录）：windsurf（`~/.codeium/windsurf/implicit` 聊天记录；memories 是记忆库**故意不加**）、continue（sessions/session.sqlite/index，社区实锤 30G 膨胀）、goose（`~/.local/share/goose/sessions` + `~/.local/state/goose/logs`；config+secrets 目录不碰）、crush（`~/.local/share/crush` 会话；config 不碰）、amp（`~/.local/share/amp/threads` 服务端会话的本地镜像；同目录 secrets.json 不在规则内）、copilot-cli（`~/.copilot/session-state`；根下配置文件不碰）
  - 项目内 agent 元数据：workspace 规则 `.crush`（crush 官方项目级数据目录，sessions/retention 90 天）
  - 工作区自动发现（asc-core/discover.rs）：① Claude Code `~/.claude/projects/` 目录名路径编码（`/`→`-`，歧义靠存在性校验兜底）② Codex sessions jsonl 头部 `cwd`（只读每文件前 4KB，新→旧最多 120 个）③ Cursor `workspaceStorage/*/workspace.json` 的 file:// URI（percent-decode）；深度护栏（拒绝 `/`、home、<3 级路径）、跨源去重、上限 30；接线到 scan（无路径时自动扫）、clean（无路径无 config 根时兜底）、TUI 根合并、doctor 预览
  - `asc doctor`：只读诊断——配置状态、生效规则统计（内置 46=41 Tier A+5 Tier B）、检测到的 agent、自动发现根预览、回收站状态
  - 待办（M4）：分发（cargo install / homebrew / 静态二进制，需先建 git 仓库与 CI）、`.qwen/`/`.genesis/` 等项目级元数据核实后追加

## 11. 后续待定问题

- `~/.claude.json`（含历史，可长到几百 MB）算配置还是垃圾？→ 倾向提供"裁剪 history 字段"的专项工具而非删除
- "死项目"阈值（源码多少天没动算死）默认 30 天是否合适，及判定是否要看 git 最近提交时间
- 回收站容量策略：build 类清理可能一次 100G+，trash 7 天保留期是否对 build 类单独缩短（如 24h）
- Tier B（dist/build/out）歧义目录是否可通过"git 是否跟踪该目录"自动判定
- 是否加 `--watch`/launchd 定期自动清理（与"不驻留"原则冲突，暂缓）
