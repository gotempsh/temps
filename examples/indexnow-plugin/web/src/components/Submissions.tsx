// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from "react";
import * as api from "../api";
import type { SubmissionResponse, SubmissionResult } from "../types";

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  return `${days}d ago`;
}

export function Submissions() {
  const [submissions, setSubmissions] = useState<SubmissionResponse[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Submit form
  const [submitUrl, setSubmitUrl] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitResult, setSubmitResult] = useState<SubmissionResult | null>(null);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      const subs = await api.listSubmissions();
      setSubmissions(subs);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!submitUrl.trim()) return;

    setSubmitting(true);
    setSubmitResult(null);
    setError(null);

    try {
      const result = await api.submitUrls(undefined, submitUrl.trim());
      setSubmitResult(result);
      load(); // Refresh list
    } catch (err) {
      setError(err instanceof Error ? err.message : "Submit failed");
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (url: string) => {
    try {
      await api.deleteSubmission(url);
      setSubmissions((prev) => prev.filter((s) => s.url !== url));
    } catch (err) {
      setError(err instanceof Error ? err.message : "Delete failed");
    }
  };

  return (
    <div>
      <div className="header">
        <h2>URL Submissions</h2>
        <button className="btn-secondary" onClick={load} disabled={loading}>
          Refresh
        </button>
      </div>

      {/* Submit form */}
      <div className="section">
        <form className="form-row" onSubmit={handleSubmit}>
          <input
            type="url"
            placeholder="https://your-site.com"
            value={submitUrl}
            onChange={(e) => setSubmitUrl(e.target.value)}
            disabled={submitting}
          />
          <button className="btn-primary" type="submit" disabled={submitting || !submitUrl.trim()}>
            {submitting ? <><span className="spinner" /> Crawling...</> : "Submit to IndexNow"}
          </button>
        </form>
      </div>

      {error && <div className="error-banner">{error}</div>}

      {submitResult && (
        <div className={submitResult.error ? "error-banner" : "success-banner"}>
          {submitResult.error
            ? `Failed: ${submitResult.error}`
            : `Submitted ${submitResult.submittedCount} URL(s) to IndexNow (status ${submitResult.apiStatus})`}
        </div>
      )}

      {/* Stats */}
      {!loading && submissions.length > 0 && (
        <div className="stats">
          <div className="stat-card">
            <div className="label">Total URLs</div>
            <div className="value">{submissions.length}</div>
          </div>
          <div className="stat-card">
            <div className="label">Hosts</div>
            <div className="value">
              {new Set(submissions.map((s) => s.host)).size}
            </div>
          </div>
          <div className="stat-card">
            <div className="label">Total Submissions</div>
            <div className="value">
              {submissions.reduce((sum, s) => sum + s.submissionCount, 0)}
            </div>
          </div>
        </div>
      )}

      {/* Table */}
      {loading ? (
        <div className="empty">
          <span className="spinner" />
          <p>Loading submissions...</p>
        </div>
      ) : submissions.length === 0 ? (
        <div className="empty">
          <p>No submissions yet</p>
          <p style={{ fontSize: "0.75rem" }}>
            Submit a site URL above, or enable auto-submit in Settings to have
            pages submitted automatically on deploy.
          </p>
        </div>
      ) : (
        <div style={{ overflowX: "auto" }}>
          <table>
            <thead>
              <tr>
                <th>URL</th>
                <th>Host</th>
                <th>Last Submitted</th>
                <th>Count</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {submissions.map((sub) => (
                <tr key={sub.url}>
                  <td>
                    <span className="mono truncate" title={sub.url}>
                      {sub.url}
                    </span>
                  </td>
                  <td>{sub.host}</td>
                  <td title={sub.lastSubmittedAt}>
                    {timeAgo(sub.lastSubmittedAt)}
                  </td>
                  <td>{sub.submissionCount}</td>
                  <td>
                    <button
                      className="btn-danger"
                      onClick={() => handleDelete(sub.url)}
                      title="Remove"
                    >
                      &times;
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
