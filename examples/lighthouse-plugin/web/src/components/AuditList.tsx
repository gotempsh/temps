// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect } from "react";
import { listAudits, startAudit, deleteAudit } from "../api";
import type { AuditSummary } from "../types";
import { auditPath, historyPath, settingsPath } from "../router";
import { statusBadgeClass, timeAgo, formatMs } from "../utils";
import { Score } from "./Score";

export function AuditList() {
  const [audits, setAudits] = useState<AuditSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [url, setUrl] = useState("");
  const [device, setDevice] = useState("mobile");
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const a = await listAudits();
        if (!cancelled) setAudits(a);
      } finally {
        if (!cancelled) setLoading(false);
      }
    }
    load();

    // Poll for running audits
    const timer = setInterval(async () => {
      try {
        const a = await listAudits();
        if (!cancelled) setAudits(a);
      } catch {
        // ignore
      }
    }, 3000);

    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!url.trim() || submitting) return;
    setSubmitting(true);
    try {
      await startAudit(url.trim(), device);
      setUrl("");
      const a = await listAudits();
      setAudits(a);
    } finally {
      setSubmitting(false);
    }
  }

  async function handleDelete(e: React.MouseEvent, id: string) {
    e.stopPropagation();
    e.preventDefault();
    await deleteAudit(id);
    setAudits((prev) => prev.filter((a) => a.id !== id));
  }

  if (loading) {
    return (
      <div className="empty">
        <span className="spinner" /> Loading...
      </div>
    );
  }

  return (
    <div>
      <div className="header">
        <h2>Lighthouse Audits</h2>
        <div style={{ display: "flex", gap: "0.5rem" }}>
          <a href={historyPath()} className="btn-secondary" style={{ textDecoration: "none" }}>
            History
          </a>
          <a href={settingsPath()} className="btn-secondary" style={{ textDecoration: "none" }}>
            Settings
          </a>
        </div>
      </div>

      {/* Start audit form */}
      <form onSubmit={handleSubmit} className="form-row" style={{ marginBottom: "1.5rem" }}>
        <input
          type="url"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://example.com"
          required
        />
        <select value={device} onChange={(e) => setDevice(e.target.value)}>
          <option value="mobile">Mobile</option>
          <option value="desktop">Desktop</option>
        </select>
        <button type="submit" className="btn-primary" disabled={submitting}>
          {submitting ? (
            <>
              <span className="spinner" /> Running...
            </>
          ) : (
            "Run Audit"
          )}
        </button>
      </form>

      {/* Audits table */}
      {audits.length === 0 ? (
        <div className="empty">
          <p>No audits yet.</p>
          <p style={{ fontSize: "0.8125rem" }}>
            Enter a URL above to run your first Lighthouse audit.
          </p>
        </div>
      ) : (
        <table>
          <thead>
            <tr>
              <th>URL</th>
              <th>Device</th>
              <th>Perf</th>
              <th>A11y</th>
              <th>BP</th>
              <th>SEO</th>
              <th>Status</th>
              <th>Trigger</th>
              <th>When</th>
              <th>Duration</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {audits.map((audit) => (
              <tr
                key={audit.id}
                onClick={() => (window.location.hash = auditPath(audit.id).slice(1))}
              >
                <td>
                  <span className="truncate" title={audit.url}>
                    {audit.url}
                  </span>
                </td>
                <td>
                  <span className="badge" style={{ fontSize: "0.65rem" }}>
                    {audit.device === "desktop" ? "Desktop" : "Mobile"}
                  </span>
                </td>
                <td><Score value={audit.performance_score} /></td>
                <td><Score value={audit.accessibility_score} /></td>
                <td><Score value={audit.best_practices_score} /></td>
                <td><Score value={audit.seo_score} /></td>
                <td>
                  <span className={statusBadgeClass(audit.status)}>
                    {audit.status === "running" && <span className="spinner" style={{ width: "0.65rem", height: "0.65rem", marginRight: "0.25rem" }} />}
                    {audit.status}
                  </span>
                </td>
                <td>
                  <span className={`badge badge-${audit.trigger}`}>
                    {audit.trigger}
                  </span>
                </td>
                <td style={{ whiteSpace: "nowrap", fontSize: "0.75rem", color: "var(--text-muted)" }}>
                  {timeAgo(audit.created_at)}
                </td>
                <td style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>
                  {audit.duration_ms > 0 ? formatMs(audit.duration_ms) : "--"}
                </td>
                <td>
                  <button
                    className="btn-danger"
                    onClick={(e) => handleDelete(e, audit.id)}
                    title="Delete"
                  >
                    x
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
