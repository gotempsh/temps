// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from "react";
import * as api from "../api";
import type { SuggestionsResponse, SuggestionReason } from "../types";

function reasonLabel(reason: SuggestionReason): { text: string; cls: string } {
  switch (reason) {
    case "never_submitted":
      return { text: "Never submitted", cls: "badge-never" };
    case "stale_submission":
      return { text: "Stale", cls: "badge-stale" };
    case "content_modified":
      return { text: "Modified", cls: "badge-modified" };
    case "content_hash_changed":
      return { text: "Content changed", cls: "badge-changed" };
    case "new_page":
      return { text: "New page", cls: "badge-never" };
    default:
      return { text: reason, cls: "" };
  }
}

export function Suggestions() {
  const [siteUrl, setSiteUrl] = useState("");
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<SuggestionsResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [submitSuccess, setSubmitSuccess] = useState<string | null>(null);

  const handleCheck = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!siteUrl.trim()) return;

    setLoading(true);
    setError(null);
    setResult(null);
    setSubmitSuccess(null);

    try {
      const data = await api.getSuggestions(siteUrl.trim());
      setResult(data);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to check");
    } finally {
      setLoading(false);
    }
  };

  const handleSubmitAll = async () => {
    if (!result || result.suggestions.length === 0) return;

    setSubmitting(true);
    setError(null);
    setSubmitSuccess(null);

    try {
      const urls = result.suggestions.map((s) => s.url);
      const res = await api.submitUrls(urls);
      if (res.error) {
        setError(res.error);
      } else {
        setSubmitSuccess(
          `Submitted ${res.submittedCount} URL(s) to IndexNow (status ${res.apiStatus})`,
        );
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Submit failed");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div>
      <div className="header">
        <h2>Page Suggestions</h2>
      </div>

      <p style={{ fontSize: "0.8125rem", color: "var(--text-muted)", marginBottom: "1rem" }}>
        Crawl a site to discover which pages need to be (re)submitted to IndexNow
        based on content changes, staleness, or new pages.
      </p>

      <div className="section">
        <form className="form-row" onSubmit={handleCheck}>
          <input
            type="url"
            placeholder="https://your-site.com"
            value={siteUrl}
            onChange={(e) => setSiteUrl(e.target.value)}
            disabled={loading}
          />
          <button className="btn-primary" type="submit" disabled={loading || !siteUrl.trim()}>
            {loading ? <><span className="spinner" /> Checking...</> : "Check Pages"}
          </button>
        </form>
      </div>

      {error && <div className="error-banner">{error}</div>}
      {submitSuccess && <div className="success-banner">{submitSuccess}</div>}

      {result && (
        <>
          <div className="stats">
            <div className="stat-card">
              <div className="label">Pages Checked</div>
              <div className="value">{result.totalPagesChecked}</div>
            </div>
            <div className="stat-card">
              <div className="label">Need Submission</div>
              <div className="value">{result.pagesNeedingSubmission}</div>
            </div>
            <div className="stat-card">
              <div className="label">Up to Date</div>
              <div className="value">
                {result.totalPagesChecked - result.pagesNeedingSubmission}
              </div>
            </div>
          </div>

          {result.suggestions.length > 0 && (
            <div style={{ marginBottom: "1rem" }}>
              <button
                className="btn-primary"
                onClick={handleSubmitAll}
                disabled={submitting}
              >
                {submitting
                  ? <><span className="spinner" /> Submitting...</>
                  : `Submit ${result.suggestions.length} URL(s) to IndexNow`}
              </button>
            </div>
          )}

          {result.suggestions.length === 0 ? (
            <div className="empty">
              <p>All pages are up to date</p>
            </div>
          ) : (
            <div style={{ overflowX: "auto" }}>
              <table>
                <thead>
                  <tr>
                    <th>URL</th>
                    <th>Reason</th>
                    <th>Last Submitted</th>
                    <th>Content Changed</th>
                  </tr>
                </thead>
                <tbody>
                  {result.suggestions.map((s) => {
                    const reason = reasonLabel(s.reason);
                    return (
                      <tr key={s.url}>
                        <td>
                          <span className="mono truncate" title={s.url}>
                            {s.url}
                          </span>
                        </td>
                        <td>
                          <span className={`badge ${reason.cls}`}>{reason.text}</span>
                        </td>
                        <td style={{ color: "var(--text-muted)", fontSize: "0.75rem" }}>
                          {s.lastSubmittedAt
                            ? new Date(s.lastSubmittedAt).toLocaleDateString()
                            : "Never"}
                        </td>
                        <td>{s.contentChanged ? "Yes" : "No"}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}
    </div>
  );
}
