# agent-shit-cleaner

> How much shit have AI agents taken in your house? Scan and find out — `asc`
> Scan and reclaim the gigabytes AI coding agents leave behind.

A Rust tool for cleaning up after AI coding agents: **TUI + CLI**, read-only
scanning, human confirmation, deletion goes to the **OS trash**.

```
$ asc scan ~/Documents
── workspace ~/Documents ── 727 junk dirs · 173.2 GB
node_modules ×250       ████████████████░░░░░░░░  96.4 GB
.next ×112             ████░░░░░░░░░░░░░░░░░░░░  21.7 GB
__pycache__ ×284       ██░░░░░░░░░░░░░░░░░░░░░░   8.9 GB
```

## Two battlefields

| Battlefield | Targets | Typical size |
|---|---|---|
| **Agent homes** | sessions/logs/caches/telemetry of 13 agents: `~/.claude`, `~/.codex`, `~/.cursor`, … | 14.3 GB measured on one machine |
| **Agent workspaces** | `node_modules` / `target` / `.venv` / `.next` / `__pycache__` inside projects | 727 dirs / 170 GB+ |

**66 built-in rules** (61 home + 5 workspace Tier B), plus **workspace
auto-discovery**: project roots are reverse-engineered from claude/codex/cursor
session metadata, no manual path config needed (macOS/Linux only; on Windows
set `[[workspace]]` manually).

## Install

```bash
# from source (Rust 1.75+)
cargo install --git https://github.com/Viyozc/agent-shit-cleaner

# or grab a static binary from the Releases page (macOS / Linux / Windows)
# https://github.com/Viyozc/agent-shit-cleaner/releases
```

## Quick start

```bash
asc                     # TUI: dashboard → select → confirm → clean
asc clean ~/code/foo    # interactive clean scoped to one directory (TUI)
asc scan                # read-only: agent homes + discovered project roots
asc scan ~/Documents    # read-only: a specific workspace
asc clean --agent workbuddy    # batch flow with filters (y/N in a terminal;
                               # in scripts it only prints the plan — deletion is impossible)
asc ports               # read-only: listening ports + process + project path
asc ports --kill 3000,5173   # terminate dev servers (per-process y/N; SIGTERM,
                             # escalate with --force; refuses without a TTY)
asc doctor              # read-only diagnostics: agents / rules / discovery
```

## Safety design (why you can trust it)

- **Zero auto-deletion**: every deletion requires human confirmation (y/N);
  shredding big items (>1GB) requires typing `DELETE` letter by letter. There
  is no `--yes` / `--force`; without an interactive terminal only the plan is
  printed — deletion is impossible
- **NEVER blacklist as the final gate**: `.git`, `CLAUDE.md`, `settings*`,
  `.credentials*`, `*.lock`, `memory/`, `skills/` … are blocked at the
  execution layer — even a miswritten rule cannot delete them
- **OS trash as the safety net**: deletions go to the system trash (Finder /
  freedesktop.org / Windows Recycle Bin); recover via Finder "Put Back"
- **Risk levels**: `safe` (regenerable) / `retention` (90-day default) /
  `redownload` (needs explicit opt-in, unselected by default)
- **Workspace guardrails**: `node_modules` requires a sibling `package.json`;
  manifest-less hits are marked ⚠ and skipped by default; **active projects**
  (source modified within 30 days) are skipped by default
- **Never touched**: model files, agent binaries, configs/credentials/memories
  (windsurf memories, `~/.config/goose` secrets, project-level `~/.qwen`
  prompts — all deliberately excluded from rules)

## Config (all optional)

`~/.config/asc/config.toml`, see [config.example.toml](config.example.toml):

```toml
tier_b = false          # ambiguous dirs like dist/build/out, off by default
active_days = 30        # project activity window

[[workspace]]           # workspace roots (auto-discovery also works)
root = "~/Documents"

[keep_days]             # per-rule retention overrides
"claude-code/sessions" = 30
```

Custom rules go in `~/.config/asc/rules.d/*.toml`; same-id rules override
builtins.

## Supported agents

claude-code · codex · cursor · trae · gemini/antigravity · opencode · windsurf · continue · goose · crush · amp · copilot-cli · qwen-code · tencent-workbuddy · qwen-work
Project-level: `.crush` · `.specstory` · `node_modules` · `target` · `.venv` · `__pycache__` · `.next` · `.nuxt` · `.turbo` · `.gradle` (Tier B: `dist` · `build` · `out` · `coverage` · `.cache`)

Every rule was verified against official docs/issues before inclusion; PRs
adding rules are welcome.

## License

MIT
