// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect } from "react";
import { getAudit, getRawJson } from "../api";
import type { LighthouseAudit } from "../types";
import { listPath } from "../router";
import { statusBadgeClass, timeAgo, formatMs, severityBadgeClass } from "../utils";
import { Score } from "./Score";
import { MetricCard } from "./MetricCard";

interface Props {
  auditId: string;
}

export function AuditDetail({ auditId }: Props) {
  const [audit, setAudit] = useState<LighthouseAudit | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [downloading, setDownloading] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | undefined;

    async function load() {
      try {
        const a = await getAudit(auditId);
        if (!cancelled) {
          setAudit(a);
          setLoading(false);

          // Poll while running
          if (a.status === "running") {
            timer = setInterval(async () => {
              try {
                const updated = await getAudit(auditId);
                if (!cancelled) {
                  setAudit(updated);
                  if (updated.status !== "running" && timer) {
                    clearInterval(timer);
                  }
                }
              } catch {
                // ignore
              }
            }, 2000);
          }
        }
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : "Failed to load audit");
          setLoading(false);
        }
      }
    }
    load();

    return () => {
      cancelled = true;
      if (timer) clearInterval(timer);
    };
  }, [auditId]);

  async function handleDownloadRaw() {
    setDownloading(true);
    try {
      const json = await getRawJson(auditId);
      const blob = new Blob([json], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `lighthouse-${auditId}.json`;
      a.click();
      URL.revokeObjectURL(url);
    } finally {
      setDownloading(false);
    }
  }

  if (loading) {
    return (
      <div className="empty">
        <span className="spinner" /> Loading audit...
      </div>
    );
  }

  if (error || !audit) {
    return (
      <div>
        <a href={listPath()} className="back-link">Back to audits</a>
        <div className="empty">
          <p>{error || "Audit not found"}</p>
        </div>
      </div>
    );
  }

  return (
    <div>
      <a href={listPath()} className="back-link">Back to audits</a>

      <div className="header">
        <div>
          <h2 style={{ marginBottom: "0.25rem" }}>{audit.url}</h2>
          <div style={{ display: "flex", gap: "0.5rem", alignItems: "center", fontSize: "0.8125rem" }}>
            <span className={statusBadgeClass(audit.status)}>
              {audit.status === "running" && <span className="spinner" style={{ width: "0.65rem", height: "0.65rem", marginRight: "0.25rem" }} />}
              {audit.status}
            </span>
            <span className={`badge badge-${audit.trigger}`}>{audit.trigger}</span>
            <span style={{ color: "var(--text-muted)" }}>{audit.device}</span>
            <span style={{ color: "var(--text-muted)" }}>{timeAgo(audit.created_at)}</span>
            {audit.duration_ms > 0 && (
              <span style={{ color: "var(--text-muted)" }}>{formatMs(audit.duration_ms)}</span>
            )}
          </div>
        </div>
        {audit.raw_json_available && (
          <button className="btn-secondary" onClick={handleDownloadRaw} disabled={downloading}>
            {downloading ? "Downloading..." : "Download Raw JSON"}
          </button>
        )}
      </div>

      {/* Error message */}
      {audit.error_message && (
        <div className="section" style={{ background: "rgba(239,68,68,0.1)", border: "1px solid var(--danger)", borderRadius: "var(--radius)", padding: "0.75rem 1rem", fontSize: "0.8125rem" }}>
          <strong>Error:</strong> {audit.error_message}
        </div>
      )}

      {/* Category scores */}
      {audit.status === "completed" && (
        <div className="scores-row">
          <div className="score-item">
            <Score value={audit.performance_score} large />
            <span className="score-label">Performance</span>
          </div>
          <div className="score-item">
            <Score value={audit.accessibility_score} large />
            <span className="score-label">Accessibility</span>
          </div>
          <div className="score-item">
            <Score value={audit.best_practices_score} large />
            <span className="score-label">Best Practices</span>
          </div>
          <div className="score-item">
            <Score value={audit.seo_score} large />
            <span className="score-label">SEO</span>
          </div>
        </div>
      )}

      {/* Core Web Vitals */}
      {audit.metrics && (
        <div className="section">
          <h3>Core Web Vitals</h3>
          <div className="metrics-grid">
            <MetricCard label="LCP" metricKey="lcp" value={audit.metrics.lcp_ms} unit="s" />
            <MetricCard label="FCP" metricKey="fcp" value={audit.metrics.fcp_ms} unit="s" />
            <MetricCard label="TBT" metricKey="tbt" value={audit.metrics.tbt_ms} unit="ms" />
            <MetricCard label="CLS" metricKey="cls" value={audit.metrics.cls} unit="" />
            <MetricCard label="Speed Index" metricKey="si" value={audit.metrics.speed_index_ms} unit="s" />
            <MetricCard label="TTI" metricKey="tti" value={audit.metrics.tti_ms} unit="s" />
          </div>
        </div>
      )}

      {/* Diagnostics */}
      {audit.diagnostics.length > 0 && (
        <div className="section">
          <h3>Diagnostics & Opportunities ({audit.diagnostics.length})</h3>
          <div className="diagnostic-list">
            {audit.diagnostics.map((d) => (
              <div key={d.id} className="diagnostic">
                <div className="diagnostic-header">
                  <span className={severityBadgeClass(d.severity)}>{d.severity}</span>
                  <span className="title">{d.title}</span>
                  {d.score !== null && (
                    <span style={{ fontSize: "0.75rem", color: "var(--text-muted)", marginLeft: "auto" }}>
                      {Math.round(d.score * 100)}%
                    </span>
                  )}
                </div>
                {d.savings && <div className="savings">{d.savings}</div>}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Metadata */}
      <div className="section">
        <h3>Details</h3>
        <div className="detail-grid">
          <div className="metric-card">
            <div className="label">Audit ID</div>
            <div className="value mono" style={{ fontSize: "0.75rem" }}>{audit.id}</div>
          </div>
          {audit.project_id && (
            <div className="metric-card">
              <div className="label">Project ID</div>
              <div className="value">{audit.project_id}</div>
            </div>
          )}
          {audit.deployment_id && (
            <div className="metric-card">
              <div className="label">Deployment ID</div>
              <div className="value">{audit.deployment_id}</div>
            </div>
          )}
          <div className="metric-card">
            <div className="label">Created</div>
            <div className="value" style={{ fontSize: "0.8125rem" }}>
              {new Date(audit.created_at).toLocaleString()}
            </div>
          </div>
          {audit.completed_at && (
            <div className="metric-card">
              <div className="label">Completed</div>
              <div className="value" style={{ fontSize: "0.8125rem" }}>
                {new Date(audit.completed_at).toLocaleString()}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
