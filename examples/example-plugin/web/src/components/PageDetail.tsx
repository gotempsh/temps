// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect } from "react";
import type { PageAnalysis } from "../types";
import { getReport } from "../api";
import { reportPath } from "../router";
import { Score } from "./Score";
import { Check } from "./Check";
import { formatMs, severityBadgeClass } from "../utils";

interface PageDetailProps {
  reportId: string;
  pageUrl: string;
}

export function PageDetail({ reportId, pageUrl }: PageDetailProps) {
  const [page, setPage] = useState<PageAnalysis | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const report = await getReport(reportId);
        if (cancelled) return;
        const found = report.pages.find((p) => p.url === pageUrl);
        if (found) {
          setPage(found);
        } else {
          setError(`Page not found in report: ${pageUrl}`);
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : "Failed to load page");
      }
    })();
    return () => { cancelled = true; };
  }, [reportId, pageUrl]);

  if (error) {
    return (
      <div>
        <a href={reportPath(reportId)} className="back-link">&larr; Back to Report</a>
        <div style={{ color: "var(--danger)" }}>{error}</div>
      </div>
    );
  }

  if (!page) {
    return (
      <div style={{ display: "flex", alignItems: "center", gap: "0.5rem", padding: "2rem", justifyContent: "center" }}>
        <span className="spinner" />
        <span style={{ color: "var(--text-muted)" }}>Loading page...</span>
      </div>
    );
  }

  const p = page;

  return (
    <>
      <a href={reportPath(reportId)} className="back-link">&larr; Back to Report</a>

      <div className="header" style={{ marginBottom: "1rem" }}>
        <div style={{ display: "flex", alignItems: "center", gap: "0.75rem" }}>
          <Score value={p.score} large />
          <div>
            <h2 style={{ marginBottom: 0 }} className="mono">
              {p.url}
            </h2>
            <div style={{ fontSize: "0.75rem", color: "var(--text-muted)", marginTop: "0.25rem" }}>
              HTTP {p.status_code} &middot; {formatMs(p.load_time_ms)} &middot; {p.word_count} words
              &middot; {p.internal_links} internal links &middot; {p.external_links} external links
            </div>
          </div>
        </div>
      </div>

      <div className="detail-grid">
        <div className="detail-card">
          <div className="label">Title</div>
          <div className="value">
            {p.title || <em style={{ color: "var(--danger)" }}>Missing</em>}
          </div>
          {p.title && (
            <div style={{ fontSize: "0.7rem", color: "var(--text-muted)", marginTop: "0.25rem" }}>
              {p.title.length} characters
            </div>
          )}
        </div>
        <div className="detail-card">
          <div className="label">Meta Description</div>
          <div className="value">
            {p.meta_description || <em style={{ color: "var(--danger)" }}>Missing</em>}
          </div>
          {p.meta_description && (
            <div style={{ fontSize: "0.7rem", color: "var(--text-muted)", marginTop: "0.25rem" }}>
              {p.meta_description.length} characters
            </div>
          )}
        </div>
        <div className="detail-card">
          <div className="label">Canonical URL</div>
          <div className="value mono">
            {p.canonical || <em style={{ color: "var(--warning)" }}>Not set</em>}
          </div>
        </div>
        <div className="detail-card">
          <div className="label">Images</div>
          <div className="value">
            {p.image_count} total, {p.images_without_alt} missing alt
          </div>
        </div>
      </div>

      <div className="section">
        <h3>Technical Checks</h3>
        <div className="checks-grid">
          <Check pass={p.has_viewport} label="Viewport meta tag" />
          <Check pass={p.has_charset} label="Character encoding" />
          <Check pass={p.has_lang} label="HTML lang attribute" />
          <Check pass={p.has_robots_meta} label="Robots meta tag" />
          <Check pass={p.has_og_title} label="Open Graph: title" />
          <Check pass={p.has_og_description} label="Open Graph: description" />
          <Check pass={p.has_og_image} label="Open Graph: image" />
          <Check pass={p.h1_count === 1} label={`Single H1 tag (${p.h1_count} found)`} />
          <Check pass={p.canonical != null} label="Canonical URL" />
          <Check pass={p.images_without_alt === 0} label="All images have alt text" />
        </div>
      </div>

      {p.issues.length > 0 ? (
        <div className="section">
          <h3>Issues ({p.issues.length})</h3>
          <div className="issue-list">
            {p.issues.map((issue, i) => (
              <div className="issue" key={i}>
                <div className="issue-header">
                  <span className={severityBadgeClass(issue.severity)}>{issue.severity}</span>
                  <span className="code">{issue.code}</span>
                </div>
                <div className="message">{issue.message}</div>
                <div className="recommendation">{issue.recommendation}</div>
              </div>
            ))}
          </div>
        </div>
      ) : (
        <div className="section" style={{ textAlign: "center", padding: "1.5rem", color: "var(--success)" }}>
          <p style={{ fontSize: "1.25rem" }}>{"\u2713"}</p>
          <p>
            <strong>No issues found!</strong>
          </p>
        </div>
      )}
    </>
  );
}
