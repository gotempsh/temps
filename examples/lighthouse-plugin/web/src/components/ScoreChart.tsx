// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ScoreHistoryPoint } from "../types";

interface ScoreChartProps {
  history: ScoreHistoryPoint[];
}

const COLORS = {
  performance: "#f59e0b",
  accessibility: "#3b82f6",
  "best-practices": "#22c55e",
  seo: "#8b5cf6",
};

const LABELS: Record<string, string> = {
  performance: "Performance",
  accessibility: "Accessibility",
  "best-practices": "Best Practices",
  seo: "SEO",
};

function getScore(point: ScoreHistoryPoint, key: string): number | null {
  switch (key) {
    case "performance": return point.performance_score;
    case "accessibility": return point.accessibility_score;
    case "best-practices": return point.best_practices_score;
    case "seo": return point.seo_score;
    default: return null;
  }
}

export function ScoreChart({ history }: ScoreChartProps) {
  // Reverse so oldest is on the left
  const points = [...history].reverse();

  if (points.length < 2) {
    return (
      <div className="chart-container">
        <h3>Score History</h3>
        <p style={{ color: "var(--text-muted)", fontSize: "0.8125rem", marginTop: "0.5rem" }}>
          Need at least 2 completed audits to show chart.
        </p>
      </div>
    );
  }

  const width = 600;
  const height = 200;
  const padding = { top: 10, right: 10, bottom: 24, left: 32 };
  const chartW = width - padding.left - padding.right;
  const chartH = height - padding.top - padding.bottom;

  const categories = ["performance", "accessibility", "best-practices", "seo"];

  function buildPath(key: string): string {
    const segments: string[] = [];
    points.forEach((p, i) => {
      const score = getScore(p, key);
      if (score === null) return;
      const x = padding.left + (i / (points.length - 1)) * chartW;
      const y = padding.top + chartH - (score / 100) * chartH;
      segments.push(`${segments.length === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`);
    });
    return segments.join(" ");
  }

  // Y-axis labels
  const yTicks = [0, 25, 50, 75, 100];

  return (
    <div className="chart-container">
      <h3>Score History</h3>
      <div className="chart-legend">
        {categories.map((cat) => (
          <span key={cat} className="chart-legend-item">
            <span
              className="chart-legend-dot"
              style={{ background: COLORS[cat as keyof typeof COLORS] }}
            />
            {LABELS[cat]}
          </span>
        ))}
      </div>
      <svg className="chart-svg" viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="xMidYMid meet">
        {/* Grid lines */}
        {yTicks.map((tick) => {
          const y = padding.top + chartH - (tick / 100) * chartH;
          return (
            <g key={tick}>
              <line
                x1={padding.left}
                y1={y}
                x2={width - padding.right}
                y2={y}
                stroke="var(--border)"
                strokeWidth="1"
              />
              <text
                x={padding.left - 4}
                y={y + 3}
                textAnchor="end"
                fill="var(--text-muted)"
                fontSize="9"
              >
                {tick}
              </text>
            </g>
          );
        })}

        {/* Data lines */}
        {categories.map((cat) => (
          <path
            key={cat}
            d={buildPath(cat)}
            fill="none"
            stroke={COLORS[cat as keyof typeof COLORS]}
            strokeWidth="2"
            strokeLinejoin="round"
          />
        ))}

        {/* Data points */}
        {categories.map((cat) =>
          points.map((p, i) => {
            const score = getScore(p, cat);
            if (score === null) return null;
            const x = padding.left + (i / (points.length - 1)) * chartW;
            const y = padding.top + chartH - (score / 100) * chartH;
            return (
              <circle
                key={`${cat}-${p.id}`}
                cx={x}
                cy={y}
                r="3"
                fill={COLORS[cat as keyof typeof COLORS]}
              />
            );
          }),
        )}

        {/* X-axis labels (show first, middle, last) */}
        {[0, Math.floor(points.length / 2), points.length - 1]
          .filter((v, i, arr) => arr.indexOf(v) === i)
          .map((idx) => {
            const x = padding.left + (idx / (points.length - 1)) * chartW;
            const label = new Date(points[idx].created_at).toLocaleDateString(undefined, {
              month: "short",
              day: "numeric",
            });
            return (
              <text
                key={idx}
                x={x}
                y={height - 4}
                textAnchor="middle"
                fill="var(--text-muted)"
                fontSize="9"
              >
                {label}
              </text>
            );
          })}
      </svg>
    </div>
  );
}
