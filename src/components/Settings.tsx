import { useState } from "react";

import type { SettingsView } from "../types";

interface Props {
  settings: SettingsView;
  setApiKey: (provider: "glm" | "anthropic", key: string) => Promise<SettingsView | null>;
  clearApiKey: (provider: "glm" | "anthropic") => Promise<void>;
  setGlmEndpoint: (endpoint: string) => Promise<void>;
  keyError: string | null;
}

export function Settings({ settings, setApiKey, clearApiKey, setGlmEndpoint, keyError }: Props) {
  return (
    <section className="panel">
      <div className="sec-head">
        <h2>API keys</h2>
        <span className="meta">stored encrypted</span>
      </div>

      <KeyRow
        label="z.ai (GLM)"
        hint="sk-… — for real GLM balance/usage"
        isSet={settings.glmKeySet}
        onSave={(k) => setApiKey("glm", k)}
        onClear={() => clearApiKey("glm")}
      />

      <KeyRow
        label="Anthropic admin"
        hint="sk-ant-admin… — org usage/cost (not the subscription %)"
        isSet={settings.anthropicKeySet}
        onSave={(k) => setApiKey("anthropic", k)}
        onClear={() => clearApiKey("anthropic")}
      />

      {keyError && <p className="key-err">{keyError}</p>}

      <div className="sec-head">
        <h2>z.ai endpoint</h2>
        <span className="meta">verify for your account</span>
      </div>
      <EndpointRow value={settings.glmEndpoint} onSave={setGlmEndpoint} />

      <div className="note">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round">
          <circle cx={12} cy={12} r={10} />
          <path d="M12 16v-4M12 8h.01" />
        </svg>
        <p>
          Keys are encrypted (AES-256-GCM) and bound to this machine — they never
          leave Rust in plaintext. The Anthropic admin API reports org-level
          token/cost, which is not the Pro/Max weekly limit.
        </p>
      </div>
    </section>
  );
}

function KeyRow({
  label,
  hint,
  isSet,
  onSave,
  onClear,
}: {
  label: string;
  hint: string;
  isSet: boolean;
  onSave: (key: string) => Promise<unknown>;
  onClear: () => Promise<void>;
}) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);

  return (
    <div className="key-row">
      <div className="key-top">
        <span className="key-label">{label}</span>
        <span className={`key-status ${isSet ? "set" : ""}`}>
          {isSet ? "● set" : "○ not set"}
        </span>
      </div>
      <div className="key-input">
        <input
          type="password"
          placeholder={isSet ? "••••••• (saved) — enter to replace" : hint}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          autoComplete="off"
          spellCheck={false}
        />
        <button
          className="btn primary"
          disabled={busy || value.trim().length === 0}
          onClick={async () => {
            setBusy(true);
            const ok = await onSave(value.trim());
            setBusy(false);
            if (ok) setValue("");
          }}
        >
          Save
        </button>
        {isSet && (
          <button
            className="btn"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              await onClear();
              setBusy(false);
            }}
          >
            Clear
          </button>
        )}
      </div>
    </div>
  );
}

function EndpointRow({ value, onSave }: { value: string; onSave: (v: string) => Promise<void> }) {
  const [v, setV] = useState(value);
  const [busy, setBusy] = useState(false);
  return (
    <div className="key-input">
      <input
        type="text"
        value={v}
        onChange={(e) => setV(e.target.value)}
        spellCheck={false}
        autoComplete="off"
      />
      <button
        className="btn"
        disabled={busy || v.trim().length === 0 || v === value}
        onClick={async () => {
          setBusy(true);
          await onSave(v.trim());
          setBusy(false);
        }}
      >
        Save
      </button>
    </div>
  );
}
