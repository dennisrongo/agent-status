# Agent Usage Monitor

A lightweight macOS **menubar widget** (Tauri 2 + React) that tracks your local
Claude Code and GLM/z.ai CLI usage — session/weekly/Opus limits, token spend,
cost estimates, and per-session history — by scanning local logs on a timer.

## Architecture

- **Rust backend** (`src-tauri/`) does all the work: scans
  `~/.claude/projects/**/*.jsonl` and `~/.zai/zai-mcp-*.log`, aggregates usage
  per 5-hour session window / rolling week / Opus week, and pushes a snapshot to
  the UI on a 60s timer (`scanner/`, `commands/`, `state/`, `settings/`).
- **React frontend** (`src/`) is thin — it only invokes commands through typed
  hooks (`hooks/useTauriCommand.ts`, `hooks/useUsage.ts`) and renders.
- Menubar tray + click-to-toggle dropdown positioned under the icon, single
  instance, launch-at-login.

## Develop

```bash
npm install
npm run tauri dev
```

## Build

```bash
npm run tauri build
```

## Notes / TODO

- **Plan ceilings are estimates.** Usage and reset times are real (from your
  logs); the "% left" denominators default to the **Max 5×** tier. Pick your
  plan from the header dropdown — it persists.
- **Icon is a generated placeholder.** Replace `src-tauri/icons/icon.png` with a
  1024×1024 source and run `npx @tauri-apps/cli icon src-tauri/icons/icon.png`.
- **Bundle identifier** is `com.dennisrongo.agentstatus` — change in
  `src-tauri/tauri.conf.json` before distributing.
- GLM/z.ai token/cost is shown as `—`: local z.ai logs record MCP lifecycle
  only, not token usage.
