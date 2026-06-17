<div align="center">

# 📊 Agent Usage Monitor

### A lightweight macOS **menubar widget** that tracks your Claude Code & GLM CLI usage in real time.

Session / weekly / Opus limits · token spend · cost estimates · per-session history — all read from local logs, refreshed on a timer, living quietly in your menu bar.

<br/>

![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)
![Rust](https://img.shields.io/badge/Rust-backend-CE412B?logo=rust&logoColor=white)
![React](https://img.shields.io/badge/React-18-61DAFB?logo=react&logoColor=black)
![TypeScript](https://img.shields.io/badge/TypeScript-strict-3178C6?logo=typescript&logoColor=white)
![Platform](https://img.shields.io/badge/macOS-menubar-000000?logo=apple&logoColor=white)
![Tests](https://img.shields.io/badge/tests-12%20passing-3FB950)

<br/>

<img src="docs/screenshots/overview.png" alt="Overview tab" width="380"/>

</div>

---

## ✨ Features

- **What's left, at a glance.** Mirrors Claude's `/usage` view — three meters for **Session (5h)**, **Week · all models**, and **Week · Opus**, each with real usage and a live reset countdown.
- **Real token data.** Exact per-request token counts parsed straight from Claude Code's session logs — input, output, and cache read/write.
- **Cost estimates** per model (Opus / Sonnet / Haiku) from standard-tier pricing.
- **7-day spark chart** + all-time model breakdown + recent-session history.
- **Live vendor data (optional).** Add a GLM Coding Plan or Anthropic admin API key — stored **encrypted, machine-bound** — for real GLM 5h/weekly quota and org-level Anthropic cost.
- **Always current.** A Rust timer re-scans and pushes fresh data to the UI — **auto-refresh interval is configurable in Settings (default 30s)**, applied live without a restart. No frozen snapshots.
- **Stays out of the way.** Menubar-only (`LSUIElement`), click-to-toggle dropdown, single-instance, launch-at-login.

---

## 🖼️ Screens

<table>
  <tr>
    <td align="center" width="50%">
      <img src="docs/screenshots/overview.png" alt="Overview" width="320"/><br/>
      <b>Overview</b> — limits, reset timers, week chart, model split
    </td>
    <td align="center" width="50%">
      <img src="docs/screenshots/sessions.png" alt="Sessions" width="320"/><br/>
      <b>Sessions</b> — recent sessions with project, model, tokens, cost
    </td>
  </tr>
  <tr>
    <td align="center" width="50%">
      <img src="docs/screenshots/providers.png" alt="Providers" width="320"/><br/>
      <b>Providers</b> — connection status + live vendor usage
    </td>
    <td align="center" width="50%">
      <img src="docs/screenshots/settings.png" alt="Settings" width="320"/><br/>
      <b>Settings</b> — plan tier + encrypted API keys
    </td>
  </tr>
</table>

---

## 🏗️ Architecture

A **thin React frontend** that only renders, and a **rich Rust backend** that does all the work.

```
┌──────────────────────────────┐        ┌─────────────────────────────────────┐
│  React frontend (src/)        │        │  Rust backend (src-tauri/src/)        │
│                               │        │                                       │
│  useUsage / useTauriCommand   │◀──────▶│  commands/   invoke handlers          │
│  Meter · WeekChart · Settings │ invoke │  scanner/    log aggregation          │
│                               │ + event│  vendors/    z.ai + Anthropic clients │
│  renders the snapshot         │◀──────▶│  encryption/ AES-256-GCM key vault    │
│                               │ usage- │  settings/ · state/ · storage/        │
│                               │ updated│  tray.rs     menubar dropdown         │
└──────────────────────────────┘        └─────────────────────────────────────┘
                                            ▲ timer (cfg) ▲ on-demand refresh
                                            │            │
                            ~/.claude/projects/**/*.jsonl · ~/.zai/zai-mcp-*.log
```

The backend scans logs (off-thread via `spawn_blocking`), optionally fetches live vendor data, merges one `UsageSnapshot`, and emits `usage-updated` to the UI.

---

## 📡 Data sources — what's real vs. estimated

| Metric | Source | Real? |
| --- | --- | --- |
| Claude token usage (session/week/model) | `~/.claude/projects/**/*.jsonl` | ✅ exact |
| Claude cost | derived from per-model pricing | ≈ estimated |
| Reset countdowns | computed from log timestamps | ✅ real |
| **"% left" ceilings** | editable plan tier (Pro / Max 5× / Max 20×) | ≈ estimated* |
| GLM token/cost (local) | `~/.zai/*.log` — lifecycle only | ❌ shown as `—` |
| GLM 5h/weekly quota (with key) | z.ai monitor API (`/api/monitor/usage/quota/limit`) | ✅ real |
| Anthropic cost (with admin key) | Anthropic Admin Cost API | ✅ real (org-level) |

\* The Pro/Max subscription "weekly % left" has **no public API**, so ceilings are estimates you set by picking your plan. The Anthropic admin key reports **org-level** cost, not the subscription quota.

---

## 🚀 Quick start

```bash
npm install
npm run tauri dev      # develop
npm run tauri build    # bundle the .app
```

> **Heads-up:** if your shell sets `NODE_ENV=production`, install with
> `NODE_ENV=development npm install --include=dev` so the dev toolchain is included.

---

## ⚙️ Configuration

- **Auto-refresh interval** — choose 10s / 15s / 30s / 1m / 2m / 5m in Settings (default **30s**); takes effect on the next cycle.
- **Plan tier** — pick Pro / Max 5× / Max 20× from the header dropdown; it sets the limit ceilings and persists.
- **API keys** (Settings tab) — optional z.ai and Anthropic admin (`sk-ant-admin…`) keys for live vendor data.
- **z.ai endpoint** — editable; confirm it against your account's billing API.

### 🔒 Security

API keys are encrypted with **AES-256-GCM** using an **Argon2id**-derived key whose password is this machine's UID — so a `settings.json` copied elsewhere can't be decrypted. Keys never leave Rust in plaintext and are never exposed to the frontend (which only sees `…KeySet` booleans).

---

## 📁 Project structure

```
agent-status/
├── src/                      # React frontend (thin)
│   ├── hooks/                # useTauriCommand, useUsage
│   ├── components/           # Meter, WeekChart, VendorCard, Settings
│   └── styles/app.css
└── src-tauri/                # Rust backend (rich)
    └── src/
        ├── commands/         # invoke handlers (collect = scan + vendor)
        ├── scanner/          # log → UsageSnapshot aggregation
        ├── vendors/          # z.ai + Anthropic API clients
        ├── encryption/       # at-rest key vault
        ├── settings/ state/ storage/
        └── tray.rs           # menubar icon + dropdown
```

---

## 🧪 Tests

```bash
cd src-tauri && cargo test --all     # 12 tests: scanner, encryption, vendor parsers
```

CI runs the suite on macOS / Windows / Ubuntu (`.github/workflows/unit-tests.yml`).

---

## 📝 Notes / TODO

- **Icon** is a generated placeholder bar-chart — replace `src-tauri/icons/icon.png` (1024×1024) and run `npx @tauri-apps/cli icon src-tauri/icons/icon.png`.
- **Bundle identifier** is `com.dennisrongo.agentstatus` — change in `src-tauri/tauri.conf.json` before distributing.
- Live vendor endpoints are best-effort and unverified offline — confirm against your accounts on first run.
