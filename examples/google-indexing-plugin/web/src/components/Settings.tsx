// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect, useCallback } from "react";
import { api } from "../api";
import type { PluginSettings } from "../types";

export default function Settings() {
  const [settings, setSettings] = useState<PluginSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  // Form fields
  const [autoSubmit, setAutoSubmit] = useState(true);
  const [maxUrlsPerDeploy, setMaxUrlsPerDeploy] = useState(50);
  const [dailyQuota, setDailyQuota] = useState(200);

  // Service account
  const [keyJson, setKeyJson] = useState("");
  const [uploadingKey, setUploadingKey] = useState(false);
  const [deletingKey, setDeletingKey] = useState(false);

  const loadSettings = useCallback(async () => {
    try {
      const s = await api.getSettings();
      setSettings(s);
      setAutoSubmit(s.autoSubmit);
      setMaxUrlsPerDeploy(s.maxUrlsPerDeploy);
      setDailyQuota(s.dailyQuota);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load settings");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadSettings();
  }, [loadSettings]);

  async function handleSaveSettings(e: React.FormEvent) {
    e.preventDefault();
    setSaving(true);
    setError(null);
    setSuccess(null);

    try {
      const updated = await api.updateSettings({
        autoSubmit,
        maxUrlsPerDeploy,
        dailyQuota,
      });
      setSettings(updated);
      setSuccess("Settings saved successfully");
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to save settings");
    } finally {
      setSaving(false);
    }
  }

  async function handleUploadKey(e: React.FormEvent) {
    e.preventDefault();
    if (!keyJson.trim()) return;

    setUploadingKey(true);
    setError(null);
    setSuccess(null);

    try {
      const info = await api.uploadServiceAccount(keyJson.trim());
      setSuccess(
        `Service account connected: ${info.clientEmail} (project: ${info.projectId})`
      );
      setKeyJson("");
      await loadSettings();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to upload key");
    } finally {
      setUploadingKey(false);
    }
  }

  async function handleDeleteKey() {
    if (!confirm("Remove the service account key? This will disable all API submissions.")) {
      return;
    }

    setDeletingKey(true);
    setError(null);
    setSuccess(null);

    try {
      await api.deleteServiceAccount();
      setSuccess("Service account key removed");
      await loadSettings();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to delete key");
    } finally {
      setDeletingKey(false);
    }
  }

  async function handleFileDrop(e: React.DragEvent) {
    e.preventDefault();
    const file = e.dataTransfer.files[0];
    if (!file) return;
    const text = await file.text();
    setKeyJson(text);
  }

  async function handleFileSelect(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    if (!file) return;
    const text = await file.text();
    setKeyJson(text);
  }

  if (loading) {
    return (
      <div className="loading-center">
        <div className="spinner" />
      </div>
    );
  }

  return (
    <div>
      {error && <div className="error-banner">{error}</div>}
      {success && <div className="success-banner">{success}</div>}

      {/* Service Account Section */}
      <div className="card">
        <h3>Google Service Account</h3>

        {settings?.serviceAccountConfigured ? (
          <div>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                padding: "0.75rem",
                background: "rgba(34, 197, 94, 0.1)",
                border: "1px solid rgba(34, 197, 94, 0.3)",
                borderRadius: "var(--radius)",
                marginBottom: "0.75rem",
              }}
            >
              <div>
                <div style={{ fontWeight: 500, fontSize: "0.875rem" }}>
                  Connected
                </div>
                <div className="mono" style={{ fontSize: "0.8125rem", color: "var(--text-muted)" }}>
                  {settings.serviceAccountEmail}
                </div>
              </div>
              <button
                type="button"
                className="btn btn-danger btn-sm"
                onClick={handleDeleteKey}
                disabled={deletingKey}
              >
                {deletingKey ? "Removing..." : "Remove"}
              </button>
            </div>
            <div className="hint">
              The service account must be added as an owner in Google Search
              Console for each site you want to index.
            </div>
          </div>
        ) : (
          <div>
            <div className="warning-banner">
              No service account configured. Upload your Google Cloud service
              account key to enable the Indexing API.
            </div>

            <form onSubmit={handleUploadKey}>
              <label
                className="upload-area"
                htmlFor="key-file-input"
                onDragOver={(e) => e.preventDefault()}
                onDrop={handleFileDrop}
              >
                <div style={{ fontSize: "1.5rem", marginBottom: "0.5rem" }}>
                  JSON
                </div>
                <p>
                  Drop your service account JSON key file here, or click to
                  browse
                </p>
                <input
                  id="key-file-input"
                  type="file"
                  accept=".json"
                  style={{ display: "none" }}
                  onChange={handleFileSelect}
                />
              </label>

              <div className="form-group">
                <label htmlFor="key-json">Or paste the JSON key content:</label>
                <textarea
                  id="key-json"
                  value={keyJson}
                  onChange={(e) => setKeyJson(e.target.value)}
                  placeholder='{"type": "service_account", "project_id": "...", ...}'
                  rows={6}
                />
              </div>

              <button
                type="submit"
                className="btn btn-primary"
                disabled={uploadingKey || !keyJson.trim()}
              >
                {uploadingKey ? (
                  <>
                    <span className="spinner" /> Connecting...
                  </>
                ) : (
                  "Connect Service Account"
                )}
              </button>
            </form>

            <div className="hint" style={{ marginTop: "1rem" }}>
              <strong>How to get a service account key:</strong>
              <ol
                style={{
                  paddingLeft: "1.25rem",
                  marginTop: "0.5rem",
                  lineHeight: "1.8",
                }}
              >
                <li>
                  Go to{" "}
                  <a
                    href="https://console.cloud.google.com/iam-admin/serviceaccounts"
                    target="_blank"
                    rel="noopener"
                    style={{ color: "var(--primary)" }}
                  >
                    Google Cloud Console &gt; Service Accounts
                  </a>
                </li>
                <li>Create a service account (or use an existing one)</li>
                <li>
                  Enable the{" "}
                  <a
                    href="https://console.cloud.google.com/apis/library/indexing.googleapis.com"
                    target="_blank"
                    rel="noopener"
                    style={{ color: "var(--primary)" }}
                  >
                    Indexing API
                  </a>{" "}
                  for your project
                </li>
                <li>Create a JSON key and download it</li>
                <li>
                  Add the service account email as an{" "}
                  <strong>Owner</strong> in{" "}
                  <a
                    href="https://search.google.com/search-console"
                    target="_blank"
                    rel="noopener"
                    style={{ color: "var(--primary)" }}
                  >
                    Google Search Console
                  </a>
                </li>
              </ol>
            </div>
          </div>
        )}
      </div>

      {/* General Settings */}
      <div className="card">
        <h3>General Settings</h3>
        <form onSubmit={handleSaveSettings}>
          <div className="form-group">
            <div className="checkbox-row">
              <input
                type="checkbox"
                id="auto-submit"
                checked={autoSubmit}
                onChange={(e) => setAutoSubmit(e.target.checked)}
              />
              <label htmlFor="auto-submit">
                Auto-submit on deployment success
              </label>
            </div>
            <div className="hint">
              When enabled, automatically notifies Google when a deployment
              succeeds.
            </div>
          </div>

          <div className="form-group">
            <label htmlFor="max-urls">Max URLs per deployment</label>
            <input
              id="max-urls"
              type="number"
              min={1}
              max={200}
              value={maxUrlsPerDeploy}
              onChange={(e) => setMaxUrlsPerDeploy(Number(e.target.value))}
            />
            <div className="hint">
              Maximum number of URLs to submit per auto-deployment (1-200).
              Helps preserve daily quota.
            </div>
          </div>

          <div className="form-group">
            <label htmlFor="daily-quota">Daily quota limit</label>
            <input
              id="daily-quota"
              type="number"
              min={1}
              max={10000}
              value={dailyQuota}
              onChange={(e) => setDailyQuota(Number(e.target.value))}
            />
            <div className="hint">
              Google's default is 200/day. If you've been approved for higher
              quota, update this value.
            </div>
          </div>

          <button
            type="submit"
            className="btn btn-primary"
            disabled={saving}
          >
            {saving ? (
              <>
                <span className="spinner" /> Saving...
              </>
            ) : (
              "Save Settings"
            )}
          </button>
        </form>
      </div>
    </div>
  );
}
