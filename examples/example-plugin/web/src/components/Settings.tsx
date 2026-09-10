// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect, type FormEvent } from "react";
import type { PluginSettings } from "../types";
import { getSettings, updateSettings } from "../api";
import { listPath } from "../router";

export function Settings() {
  const [settings, setSettings] = useState<PluginSettings | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  // Form state
  const [maxPages, setMaxPages] = useState("");
  const [userAgent, setUserAgent] = useState("");
  const [timeout, setTimeout] = useState("");
  const [crawlDelay, setCrawlDelay] = useState("");

  useEffect(() => {
    (async () => {
      try {
        const s = await getSettings();
        setSettings(s);
        setMaxPages(String(s.default_max_pages));
        setUserAgent(s.user_agent);
        setTimeout(String(s.request_timeout_secs));
        setCrawlDelay(String(s.crawl_delay_ms));
      } catch (e) {
        setError(e instanceof Error ? e.message : "Failed to load settings");
      }
    })();
  }, []);

  const handleSave = async (e: FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setError(null);
    setSuccess(false);

    try {
      const updated = await updateSettings({
        default_max_pages: maxPages ? parseInt(maxPages, 10) : undefined,
        user_agent: userAgent || undefined,
        request_timeout_secs: timeout ? parseInt(timeout, 10) : undefined,
        crawl_delay_ms: crawlDelay ? parseInt(crawlDelay, 10) : undefined,
      });
      setSettings(updated);
      setSuccess(true);
      globalThis.setTimeout(() => setSuccess(false), 3000);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to save settings");
    } finally {
      setSaving(false);
    }
  };

  if (!settings && !error) {
    return (
      <div style={{ display: "flex", alignItems: "center", gap: "0.5rem", padding: "2rem", justifyContent: "center" }}>
        <span className="spinner" />
        <span style={{ color: "var(--text-muted)" }}>Loading settings...</span>
      </div>
    );
  }

  return (
    <>
      <a href={listPath()} className="back-link">&larr; Back to Reports</a>

      <div className="header">
        <h2>Plugin Settings</h2>
      </div>

      {error && (
        <div style={{ color: "var(--danger)", fontSize: "0.8125rem", marginBottom: "0.75rem" }}>
          {error}
        </div>
      )}

      {success && (
        <div style={{ color: "var(--success)", fontSize: "0.8125rem", marginBottom: "0.75rem" }}>
          Settings saved successfully.
        </div>
      )}

      <form onSubmit={handleSave}>
        <div className="settings-grid">
          <div className="setting-field">
            <label htmlFor="max-pages">Default max pages</label>
            <input
              id="max-pages"
              type="number"
              min="1"
              value={maxPages}
              onChange={(e) => setMaxPages(e.target.value)}
              disabled={saving}
            />
            <div className="hint">
              Number of pages to crawl when not specified per-analysis.
            </div>
          </div>

          <div className="setting-field">
            <label htmlFor="user-agent">User-Agent</label>
            <input
              id="user-agent"
              type="text"
              value={userAgent}
              onChange={(e) => setUserAgent(e.target.value)}
              disabled={saving}
            />
            <div className="hint">
              Sent with every crawl request. Identify your bot to site operators.
            </div>
          </div>

          <div className="setting-field">
            <label htmlFor="timeout">Request timeout (seconds)</label>
            <input
              id="timeout"
              type="number"
              min="1"
              max="120"
              value={timeout}
              onChange={(e) => setTimeout(e.target.value)}
              disabled={saving}
            />
            <div className="hint">
              How long to wait for each page to respond before giving up.
            </div>
          </div>

          <div className="setting-field">
            <label htmlFor="crawl-delay">Crawl delay (ms)</label>
            <input
              id="crawl-delay"
              type="number"
              min="0"
              value={crawlDelay}
              onChange={(e) => setCrawlDelay(e.target.value)}
              disabled={saving}
            />
            <div className="hint">
              Delay between requests to avoid overwhelming the target server.
              Set to 0 for no delay.
            </div>
          </div>
        </div>

        <div style={{ marginTop: "1rem" }}>
          <button type="submit" className="btn-primary" disabled={saving}>
            {saving ? (
              <>
                <span className="spinner" /> Saving...
              </>
            ) : (
              "Save Settings"
            )}
          </button>
        </div>
      </form>
    </>
  );
}
