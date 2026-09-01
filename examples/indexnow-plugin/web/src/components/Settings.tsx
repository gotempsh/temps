// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from "react";
import * as api from "../api";
import type { PluginSettings } from "../types";

export function Settings() {
  const [settings, setSettings] = useState<PluginSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  // Form fields
  const [apiKey, setApiKey] = useState("");
  const [searchEngine, setSearchEngine] = useState("");
  const [autoSubmit, setAutoSubmit] = useState(true);
  const [maxPages, setMaxPages] = useState(100);
  const [resubmitAfterHours, setResubmitAfterHours] = useState(48);
  const [userAgent, setUserAgent] = useState("");

  useEffect(() => {
    (async () => {
      setLoading(true);
      try {
        const s = await api.getSettings();
        setSettings(s);
        setApiKey(s.apiKey ?? "");
        setSearchEngine(s.searchEngine);
        setAutoSubmit(s.autoSubmit);
        setMaxPages(s.maxPages);
        setResubmitAfterHours(s.resubmitAfterHours);
        setUserAgent(s.userAgent);
      } catch (e) {
        setError(e instanceof Error ? e.message : "Failed to load settings");
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setError(null);
    setSuccess(null);

    try {
      const updated = await api.updateSettings({
        apiKey,
        searchEngine,
        autoSubmit,
        maxPages,
        resubmitAfterHours,
        userAgent,
      });
      setSettings(updated);
      setSuccess("Settings saved");
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to save");
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="empty">
        <span className="spinner" />
        <p>Loading settings...</p>
      </div>
    );
  }

  return (
    <div>
      <div className="header">
        <h2>Settings</h2>
      </div>

      {error && <div className="error-banner">{error}</div>}
      {success && <div className="success-banner">{success}</div>}

      {settings && (
        <form className="settings-grid" onSubmit={handleSave}>
          <div className="setting-field">
            <label htmlFor="apiKey">IndexNow API Key</label>
            <input
              id="apiKey"
              type="text"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder="e.g. a1b2c3d4e5f6..."
            />
            <span className="hint">
              8-128 character hex key. Get one from{" "}
              <a href="https://www.indexnow.org/documentation" target="_blank" rel="noreferrer">
                indexnow.org
              </a>
              . Required for submissions to work.
            </span>
          </div>

          <div className="setting-field">
            <label htmlFor="searchEngine">Search Engine Endpoint</label>
            <input
              id="searchEngine"
              type="text"
              value={searchEngine}
              onChange={(e) => setSearchEngine(e.target.value)}
            />
            <span className="hint">
              IndexNow-compatible endpoint (e.g., api.indexnow.org, www.bing.com, yandex.com)
            </span>
          </div>

          <div className="setting-field">
            <div className="checkbox-row">
              <input
                id="autoSubmit"
                type="checkbox"
                checked={autoSubmit}
                onChange={(e) => setAutoSubmit(e.target.checked)}
              />
              <label htmlFor="autoSubmit">Auto-submit on deployment</label>
            </div>
            <span className="hint">
              Automatically crawl and submit changed pages when a deployment succeeds
            </span>
          </div>

          <div className="setting-field">
            <label htmlFor="maxPages">Max Pages per Crawl</label>
            <input
              id="maxPages"
              type="number"
              min={1}
              max={10000}
              value={maxPages}
              onChange={(e) => setMaxPages(Number(e.target.value))}
            />
            <span className="hint">
              Maximum number of pages to discover when crawling a site
            </span>
          </div>

          <div className="setting-field">
            <label htmlFor="resubmitAfterHours">Resubmit After (hours)</label>
            <input
              id="resubmitAfterHours"
              type="number"
              min={1}
              max={8760}
              value={resubmitAfterHours}
              onChange={(e) => setResubmitAfterHours(Number(e.target.value))}
            />
            <span className="hint">
              Hours after which a previously-submitted page is considered stale and eligible for resubmission
            </span>
          </div>

          <div className="setting-field">
            <label htmlFor="userAgent">User-Agent</label>
            <input
              id="userAgent"
              type="text"
              value={userAgent}
              onChange={(e) => setUserAgent(e.target.value)}
            />
            <span className="hint">
              HTTP User-Agent string used when crawling sites
            </span>
          </div>

          <div>
            <button className="btn-primary" type="submit" disabled={saving}>
              {saving ? "Saving..." : "Save Settings"}
            </button>
          </div>
        </form>
      )}
    </div>
  );
}
