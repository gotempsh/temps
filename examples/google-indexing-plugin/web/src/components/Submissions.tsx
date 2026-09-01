// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect, useCallback } from "react";
import { api } from "../api";
import type { SubmissionResponse, SubmissionResult, QuotaStatus } from "../types";

function timeAgo(iso: string): string {
  const seconds = Math.floor((Date.now() - new Date(iso).getTime()) / 1000);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

export default function Submissions() {
  const [submissions, setSubmissions] = useState<SubmissionResponse[]>([]);
  const [quota, setQuota] = useState<QuotaStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [submitResult, setSubmitResult] = useState<SubmissionResult | null>(null);
  const [urlInput, setUrlInput] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [notificationType, setNotificationType] = useState("URL_UPDATED");

  const load = useCallback(async () => {
    try {
      const [subs, q] = await Promise.all([
        api.listSubmissions(),
        api.getQuota(),
      ]);
      setSubmissions(subs);
      setQuota(q);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load data");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!urlInput.trim()) return;

    setSubmitting(true);
    setSubmitResult(null);
    setError(null);

    try {
      // Split by newlines or commas
      const urls = urlInput
        .split(/[\n,]/)
        .map((u) => u.trim())
        .filter((u) => u.length > 0);

      const result = await api.submitUrls(urls, notificationType);
      setSubmitResult(result);
      setUrlInput("");
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Submission failed");
    } finally {
      setSubmitting(false);
    }
  }

  async function handleDelete(url: string) {
    try {
      await api.deleteSubmission(url);
      setSubmissions((prev) => prev.filter((s) => s.url !== url));
    } catch (e) {
      setError(e instanceof Error ? e.message : "Delete failed");
    }
  }

  if (loading) {
    return (
      <div className="loading-center">
        <div className="spinner" />
      </div>
    );
  }

  const usagePercent = quota ? (quota.usedToday / quota.dailyLimit) * 100 : 0;
  const quotaLevel =
    usagePercent >= 90 ? "high" : usagePercent >= 60 ? "medium" : "low";

  return (
    <div>
      {/* Quota bar */}
      {quota && (
        <div className="stats">
          <div className="stat">
            <div className="stat-value">{quota.remaining}</div>
            <div className="stat-label">Remaining Today</div>
          </div>
          <div className="stat">
            <div className="stat-value">{quota.usedToday}</div>
            <div className="stat-label">Used Today</div>
          </div>
          <div className="stat">
            <div className="stat-value">{quota.dailyLimit}</div>
            <div className="stat-label">Daily Limit</div>
          </div>
          <div className="stat">
            <div className="stat-value">{submissions.length}</div>
            <div className="stat-label">Total URLs</div>
          </div>
        </div>
      )}

      {quota && (
        <div className="quota-bar-container" style={{ marginBottom: "1.5rem" }}>
          <div
            style={{
              display: "flex",
              justifyContent: "space-between",
              fontSize: "0.75rem",
              color: "var(--text-muted)",
              marginBottom: "0.25rem",
            }}
          >
            <span>
              Quota: {quota.usedToday}/{quota.dailyLimit}
            </span>
            <span>{Math.round(usagePercent)}% used</span>
          </div>
          <div className="quota-bar">
            <div
              className={`quota-bar-fill ${quotaLevel}`}
              style={{ width: `${Math.min(usagePercent, 100)}%` }}
            />
          </div>
        </div>
      )}

      {error && <div className="error-banner">{error}</div>}

      {submitResult && (
        <div
          className={
            submitResult.error ? "error-banner" : "success-banner"
          }
        >
          {submitResult.error
            ? submitResult.error
            : `Submitted ${submitResult.submittedCount} URL(s). ${submitResult.failedCount > 0 ? `${submitResult.failedCount} failed.` : ""} ${submitResult.remainingQuota} quota remaining.`}
        </div>
      )}

      {/* Submit form */}
      <div className="card">
        <h3>Submit URLs to Google</h3>
        <form onSubmit={handleSubmit}>
          <div className="form-group">
            <textarea
              value={urlInput}
              onChange={(e) => setUrlInput(e.target.value)}
              placeholder="Enter URLs (one per line or comma-separated)&#10;https://example.com/page1&#10;https://example.com/page2"
              rows={3}
            />
            <div className="hint">
              Paste URLs to notify Google about. Each URL counts toward your
              daily quota.
            </div>
          </div>
          <div
            style={{
              display: "flex",
              gap: "0.5rem",
              alignItems: "center",
            }}
          >
            <select
              value={notificationType}
              onChange={(e) => setNotificationType(e.target.value)}
              style={{
                padding: "0.5rem 0.75rem",
                background: "var(--bg-input)",
                border: "1px solid var(--border)",
                borderRadius: "var(--radius)",
                color: "var(--text)",
                fontSize: "0.875rem",
              }}
            >
              <option value="URL_UPDATED">URL Updated</option>
              <option value="URL_DELETED">URL Deleted</option>
            </select>
            <button
              type="submit"
              className="btn btn-primary"
              disabled={submitting || !urlInput.trim()}
            >
              {submitting ? (
                <>
                  <span className="spinner" /> Submitting...
                </>
              ) : (
                "Submit to Google"
              )}
            </button>
          </div>
        </form>
      </div>

      {/* Submissions table */}
      {submissions.length === 0 ? (
        <div className="empty">
          No URLs submitted yet. Submit URLs above or enable auto-submit in
          Settings.
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>URL</th>
                <th>Type</th>
                <th>Status</th>
                <th>Submitted</th>
                <th>Count</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {submissions.map((s) => (
                <tr key={s.url}>
                  <td className="mono truncate" title={s.url}>
                    {s.url}
                  </td>
                  <td>
                    <span
                      className={`badge ${s.notificationType === "URL_DELETED" ? "badge-warning" : "badge-muted"}`}
                    >
                      {s.notificationType === "URL_DELETED"
                        ? "Deleted"
                        : "Updated"}
                    </span>
                  </td>
                  <td>
                    {s.googleResponseStatus ? (
                      <span
                        className={`badge ${s.googleResponseStatus < 300 ? "badge-success" : "badge-error"}`}
                      >
                        {s.googleResponseStatus}
                      </span>
                    ) : (
                      <span className="badge badge-muted">-</span>
                    )}
                  </td>
                  <td title={s.submittedAt}>{timeAgo(s.submittedAt)}</td>
                  <td>{s.submissionCount}</td>
                  <td>
                    <button
                      type="button"
                      className="btn btn-danger btn-sm"
                      onClick={() => handleDelete(s.url)}
                    >
                      Delete
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
