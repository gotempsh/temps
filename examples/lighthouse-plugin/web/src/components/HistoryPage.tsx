// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect } from "react";
import { getScoreHistory } from "../api";
import type { ScoreHistoryPoint } from "../types";
import { listPath } from "../router";
import { ScoreChart } from "./ScoreChart";

export function HistoryPage() {
  const [history, setHistory] = useState<ScoreHistoryPoint[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const h = await getScoreHistory();
        if (!cancelled) setHistory(h);
      } finally {
        if (!cancelled) setLoading(false);
      }
    }
    load();
    return () => { cancelled = true; };
  }, []);

  if (loading) {
    return (
      <div className="empty">
        <span className="spinner" /> Loading...
      </div>
    );
  }

  return (
    <div>
      <button className="back-link" onClick={() => (window.location.hash = listPath().slice(1))}>
        &larr; Back to Audits
      </button>

      <div className="header">
        <h2>Score History</h2>
      </div>

      <ScoreChart history={history} />

      {history.length > 0 && (
        <table>
          <thead>
            <tr>
              <th>Date</th>
              <th>Perf</th>
              <th>A11y</th>
              <th>BP</th>
              <th>SEO</th>
              <th>Trigger</th>
            </tr>
          </thead>
          <tbody>
            {history.map((point) => (
              <tr key={point.id}>
                <td style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>
                  {new Date(point.created_at).toLocaleString()}
                </td>
                <td>{point.performance_score ?? "--"}</td>
                <td>{point.accessibility_score ?? "--"}</td>
                <td>{point.best_practices_score ?? "--"}</td>
                <td>{point.seo_score ?? "--"}</td>
                <td>
                  <span className={`badge badge-${point.trigger}`}>
                    {point.trigger}
                  </span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
