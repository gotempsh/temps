// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect, useRef, useCallback } from "react";
import type { SeoReport } from "../types";
import { getReport, getReportPrompt } from "../api";
import { listPath, pagePath, navigate } from "../router";
import { Score } from "./Score";
import { StatCard } from "./StatCard";
import { scoreColor, statusBadgeClass, formatMs, getPathname } from "../utils";

interface ReportDetailProps {
  reportId: string;
}

export function ReportDetail({ reportId }: ReportDetailProps) {
  const [report, setReport] = useState<SeoReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copyState, setCopyState] = useState<"idle" | "loading" | "copied">("idle");
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const handleCopyPrompt = useCallback(async () => {
    if (copyState !== "idle") return;
    setCopyState("loading");
    try {
      const prompt = await getReportPrompt(reportId);
      await navigator.clipboard.writeText(prompt);
      setCopyState("copied");
      setTimeout(() => setCopyState("idle"), 2000);
    } catch {
      setCopyState("idle");
    }
  }, [reportId, copyState]);

  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      try {
        const data = await getReport(reportId);
        if (cancelled) return;
        setReport(data);
        setError(null);

        if (data.status !== "running" && pollRef.current) {
          clearInterval(pollRef.current);
          pollRef.current = null;
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : "Failed to load report");
      }
    };

    load();
    pollRef.current = setInterval(load, 2000);

    return () => {
      cancelled = true;
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [reportId]);

  if (error) {
    return (
      <div>
        <a href={listPath()} className="back-link">&larr; All Reports</a>
        <div style={{ color: "var(--danger)" }}>{error}</div>
      </div>
    );
  }

  if (!report) {
    return (
      <div style={{ display: "flex", alignItems: "center", gap: "0.5rem", padding: "2rem", justifyContent: "center" }}>
        <span className="spinner" />
        <span style={{ color: "var(--text-muted)" }}>Loading report...</span>
      </div>
    );
  }

  const { summary: s } = report;
  const isRunning = report.status === "running";

  return (
    <>
      <a href={listPath()} className="back-link">&larr; All Reports</a>

      <div className="header">
        <div style={{ display: "flex", alignItems: "center", gap: "0.75rem", flex: 1 }}>
          <Score value={report.score} large />
          <div>
            <h2 style={{ marginBottom: 0 }}>
              <span className="mono">{report.url}</span>
            </h2>
            <div
              style={{
                fontSize: "0.75rem",
                color: "var(--text-muted)",
                display: "flex",
                gap: "0.75rem",
                marginTop: "0.25rem",
              }}
            >
              <span className={statusBadgeClass(report.status)}>{report.status}</span>
              <span>{s.pages_crawled} pages crawled</span>
              {report.duration_ms > 0 && <span>{formatMs(report.duration_ms)}</span>}
            </div>
          </div>
        </div>
        {!isRunning && s.total_issues > 0 && (
          <button
            type="button"
            className="btn-secondary"
            onClick={handleCopyPrompt}
            disabled={copyState !== "idle"}
            title="Copy a text summary of all issues for pasting into an LLM (ChatGPT, Claude, etc.)"
          >
            {copyState === "loading"
              ? "Loading..."
              : copyState === "copied"
                ? "Copied!"
                : "Copy for LLM"}
          </button>
        )}
      </div>

      {isRunning && (
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: "0.5rem",
            marginBottom: "1rem",
            color: "var(--accent)",
          }}
        >
          <span className="spinner" /> Analysis in progress...
        </div>
      )}

      <div className="stats">
        <StatCard label="Score" value={report.score} color={scoreColor(report.score)} />
        <StatCard label="Pages" value={s.pages_crawled} />
        <StatCard label="Critical" value={s.critical} color="var(--danger)" />
        <StatCard label="Warnings" value={s.warnings} color="var(--warning)" />
        <StatCard label="Info" value={s.info} />
      </div>

      {s.pages_crawled > 0 && (
        <div className="section">
          <h3>Issue Breakdown</h3>
          <div className="stats" style={{ gridTemplateColumns: "repeat(auto-fit, minmax(120px, 1fr))" }}>
            <StatCard label="Missing Titles" value={s.missing_titles} />
            <StatCard label="Missing Descriptions" value={s.missing_descriptions} />
            <StatCard label="Missing H1" value={s.missing_h1} />
            <StatCard label="Images w/o Alt" value={s.images_without_alt} />
            <StatCard label="Missing Canonical" value={s.missing_canonical} />
            <StatCard label="Incomplete OG" value={s.missing_og_tags} />
          </div>
        </div>
      )}

      <div className="section">
        <h3>Pages</h3>
        {report.pages.length === 0 ? (
          <p style={{ color: "var(--text-muted)" }}>
            {isRunning ? "Crawling pages..." : "No pages found."}
          </p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>Score</th>
                <th>URL</th>
                <th>Status</th>
                <th>Title</th>
                <th>Issues</th>
                <th>Time</th>
              </tr>
            </thead>
            <tbody>
              {report.pages.map((p) => {
                const crit = p.issues.filter((i) => i.severity === "critical").length;
                const warn = p.issues.filter((i) => i.severity === "warning").length;
                return (
                  <tr key={p.url} onClick={() => navigate(pagePath(reportId, p.url))}>
                    <td>
                      <Score value={p.score} />
                    </td>
                    <td>
                      <span className="mono truncate">{getPathname(p.url)}</span>
                    </td>
                    <td>
                      <span className="mono">{p.status_code}</span>
                    </td>
                    <td>
                      <span className="truncate">
                        {p.title || <em style={{ color: "var(--text-muted)" }}>missing</em>}
                      </span>
                    </td>
                    <td>
                      {crit > 0 && <span className="badge badge-critical">{crit}</span>}{" "}
                      {warn > 0 && <span className="badge badge-warning">{warn}</span>}
                      {!crit && !warn && "\u2713"}
                    </td>
                    <td className="mono">{formatMs(p.load_time_ms)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </>
  );
}
