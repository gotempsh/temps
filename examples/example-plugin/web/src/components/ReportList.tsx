// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect, useCallback, type FormEvent } from "react";
import type { ReportSummary, PluginSettings } from "../types";
import { listReports, startAnalysis, deleteReport, getSettings } from "../api";
import { navigate, reportPath, settingsPath } from "../router";
import { Score } from "./Score";
import { timeAgo, formatMs, statusBadgeClass } from "../utils";

export function ReportList() {
  const [reports, setReports] = useState<ReportSummary[]>([]);
  const [settings, setSettings] = useState<PluginSettings | null>(null);
  const [analyzing, setAnalyzing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [url, setUrl] = useState("");
  const [maxPages, setMaxPages] = useState<string>("200");

  const load = useCallback(async () => {
    try {
      const data = await listReports();
      setReports(data);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load reports");
    }
  }, []);

  const loadSettings = useCallback(async () => {
    try {
      const s = await getSettings();
      setSettings(s);
    } catch {
      // Settings are non-critical for the list view
    }
  }, []);

  useEffect(() => {
    load();
    loadSettings();
    const id = setInterval(load, 5000);
    return () => clearInterval(id);
  }, [load, loadSettings]);

  const handleAnalyze = async (e: FormEvent) => {
    e.preventDefault();
    if (!url.trim() || analyzing) return;

    setAnalyzing(true);
    setError(null);

    try {
      const pages = maxPages ? parseInt(maxPages, 10) : undefined;
      const result = await startAnalysis(url.trim(), pages);
      setUrl("");
      setMaxPages("");
      navigate(reportPath(result.id));
    } catch (e) {
      setError(e instanceof Error ? e.message : "Analysis failed");
    } finally {
      setAnalyzing(false);
    }
  };

  const handleDelete = async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!confirm("Delete this report?")) return;
    try {
      await deleteReport(id);
      await load();
    } catch {
      // ignore
    }
  };

  return (
    <>
      <div className="header">
        <h2>SEO Reports</h2>
        <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
          <a
            href={settingsPath()}
            className="btn-secondary"
            style={{ fontSize: "0.75rem", textDecoration: "none" }}
          >
            Settings
          </a>
        </div>
      </div>

      <form onSubmit={handleAnalyze} className="form-row">
        <input
          type="url"
          placeholder="https://example.com"
          required
          disabled={analyzing}
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          style={{ flex: 1 }}
        />
        <input
          type="number"
          placeholder={settings ? `${settings.default_max_pages} pages` : "Pages"}
          min="1"
          disabled={analyzing}
          value={maxPages}
          onChange={(e) => setMaxPages(e.target.value)}
          style={{ width: "110px" }}
          title="Max pages to crawl (leave empty for default)"
        />
        <button type="submit" className="btn-primary" disabled={analyzing}>
          {analyzing ? (
            <>
              <span className="spinner" /> Analyzing...
            </>
          ) : (
            "Analyze"
          )}
        </button>
      </form>

      {error && (
        <div style={{ color: "var(--danger)", fontSize: "0.8125rem", marginBottom: "0.75rem" }}>
          {error}
        </div>
      )}

      {reports.length === 0 ? (
        <div className="empty">
          <p style={{ fontSize: "1.5rem", marginBottom: "0.25rem" }}>&#x1F50D;</p>
          <p>
            <strong>No SEO reports yet</strong>
          </p>
          <p style={{ fontSize: "0.8125rem" }}>
            Enter a URL above to analyze a site's technical SEO health.
          </p>
        </div>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Score</th>
              <th>URL</th>
              <th>Pages</th>
              <th>Issues</th>
              <th>Status</th>
              <th>When</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {reports.map((r) => (
              <tr key={r.id} onClick={() => navigate(reportPath(r.id))}>
                <td>
                  <Score value={r.score} />
                </td>
                <td>
                  <span className="mono truncate">{r.url}</span>
                </td>
                <td>{r.pages_crawled}</td>
                <td>
                  {r.critical_issues > 0 && (
                    <span className="badge badge-critical">{r.critical_issues} critical</span>
                  )}{" "}
                  {r.warning_issues > 0 && (
                    <span className="badge badge-warning">{r.warning_issues} warn</span>
                  )}{" "}
                  {r.info_issues > 0 && (
                    <span className="badge badge-info">{r.info_issues} info</span>
                  )}
                  {!r.critical_issues && !r.warning_issues && !r.info_issues && (
                    <span style={{ color: "var(--text-muted)" }}>&mdash;</span>
                  )}
                </td>
                <td>
                  <span className={statusBadgeClass(r.status)}>{r.status}</span>
                </td>
                <td style={{ color: "var(--text-muted)", fontSize: "0.75rem" }}>
                  {timeAgo(r.created_at)}
                  {r.duration_ms > 0 && (
                    <span style={{ marginLeft: "0.5rem" }}>{formatMs(r.duration_ms)}</span>
                  )}
                </td>
                <td>
                  <button
                    type="button"
                    className="btn-danger"
                    onClick={(e) => handleDelete(r.id, e)}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}
