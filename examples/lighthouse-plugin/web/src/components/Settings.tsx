// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect } from "react";
import { getSettings, updateSettings, getStatus } from "../api";
import type { PluginSettings } from "../types";
import { listPath } from "../router";

export function Settings() {
  const [settings, setSettings] = useState<PluginSettings | null>(null);
  const [lighthouseAvailable, setLighthouseAvailable] = useState<boolean | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  // Form state
  const [autoAudit, setAutoAudit] = useState(true);
  const [scoreThreshold, setScoreThreshold] = useState(80);
  const [timeoutSecs, setTimeoutSecs] = useState(60);
  const [chromeFlags, setChromeFlags] = useState("");
  const [device, setDevice] = useState("mobile");

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const [s, status] = await Promise.all([getSettings(), getStatus()]);
        if (!cancelled) {
          setSettings(s);
          setLighthouseAvailable(status.lighthouse_available);
          setAutoAudit(s.auto_audit_on_deploy);
          setScoreThreshold(s.score_threshold);
          setTimeoutSecs(s.timeout_secs);
          setChromeFlags(s.chrome_flags);
          setDevice(s.device);
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    }
    load();
    return () => { cancelled = true; };
  }, []);

  async function handleSave() {
    setSaving(true);
    setSaved(false);
    try {
      const updated = await updateSettings({
        auto_audit_on_deploy: autoAudit,
        score_threshold: scoreThreshold,
        timeout_secs: timeoutSecs,
        chrome_flags: chromeFlags,
        device,
      });
      setSettings(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } finally {
      setSaving(false);
    }
  }

  if (loading || !settings) {
    return (
      <div className="empty">
        <span className="spinner" /> Loading settings...
      </div>
    );
  }

  return (
    <div>
      <a href={listPath()} className="back-link">Back to audits</a>

      <div className="header">
        <h2>Settings</h2>
        <div style={{ display: "flex", gap: "0.5rem", alignItems: "center" }}>
          {saved && <span style={{ color: "var(--success)", fontSize: "0.8125rem" }}>Saved</span>}
          <button className="btn-primary" onClick={handleSave} disabled={saving}>
            {saving ? "Saving..." : "Save"}
          </button>
        </div>
      </div>

      {/* Status */}
      <div className="section" style={{ padding: "0.75rem 1rem", background: "var(--bg-card)", border: "1px solid var(--border)", borderRadius: "var(--radius)", fontSize: "0.8125rem" }}>
        Lighthouse CLI:{" "}
        {lighthouseAvailable === null ? (
          <span className="spinner" style={{ width: "0.65rem", height: "0.65rem" }} />
        ) : lighthouseAvailable ? (
          <span style={{ color: "var(--success)" }}>Available</span>
        ) : (
          <span style={{ color: "var(--danger)" }}>
            Not found. Install with: <code>npm install -g lighthouse</code>
          </span>
        )}
      </div>

      <div className="settings-grid" style={{ marginTop: "1rem" }}>
        <div className="setting-field">
          <label className="setting-checkbox">
            <input
              type="checkbox"
              checked={autoAudit}
              onChange={(e) => setAutoAudit(e.target.checked)}
            />
            Auto-audit after deployments
          </label>
          <span className="hint">
            Automatically run Lighthouse after deployment.succeeded and deployment.ready events.
          </span>
        </div>

        <div className="setting-field">
          <label>Score Threshold</label>
          <input
            type="number"
            min={0}
            max={100}
            value={scoreThreshold}
            onChange={(e) => setScoreThreshold(Number(e.target.value))}
          />
          <span className="hint">Scores below this value are flagged as needing attention (0-100).</span>
        </div>

        <div className="setting-field">
          <label>Default Device</label>
          <select value={device} onChange={(e) => setDevice(e.target.value)}>
            <option value="mobile">Mobile</option>
            <option value="desktop">Desktop</option>
          </select>
          <span className="hint">Device emulation for Lighthouse audits.</span>
        </div>

        <div className="setting-field">
          <label>Timeout (seconds)</label>
          <input
            type="number"
            min={10}
            max={600}
            value={timeoutSecs}
            onChange={(e) => setTimeoutSecs(Number(e.target.value))}
          />
          <span className="hint">Maximum time to wait for Lighthouse CLI to complete.</span>
        </div>

        <div className="setting-field">
          <label>Chrome Flags</label>
          <input
            type="text"
            value={chromeFlags}
            onChange={(e) => setChromeFlags(e.target.value)}
          />
          <span className="hint">
            Flags passed to Chrome (e.g. --headless --no-sandbox --disable-gpu).
          </span>
        </div>
      </div>
    </div>
  );
}
