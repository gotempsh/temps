// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo, useState } from "react";
import { projectPath } from "./project-link";

type HealthState = "healthy" | "failed" | "active" | "paused" | "never" | "unknown";

interface Deployment {
  id: number;
  state: string;
  branch?: string;
  commit_sha?: string;
  commit_message?: string;
  created_at: string;
}

interface ProjectPulse {
  id: number;
  name: string;
  slug: string;
  preset: string;
  sourceType: string;
  health: HealthState;
  latestDeployment: Deployment | null;
  recentDeployments: number;
  recentFailures: number;
  successRate: number | null;
  error?: string;
}

interface Overview {
  generatedAt: string;
  summary: Record<HealthState, number> & {
    total: number;
    deployments24h: number;
    failures24h: number;
  };
  projects: ProjectPulse[];
}

const API_URL = "/api/x/deployment-pulse/overview";
const FILTERS: Array<{ value: "all" | HealthState; label: string }> = [
  { value: "all", label: "All" },
  { value: "failed", label: "Needs attention" },
  { value: "active", label: "In progress" },
  { value: "healthy", label: "Healthy" },
];

const STATUS_LABEL: Record<HealthState, string> = {
  healthy: "Healthy",
  failed: "Failed",
  active: "In progress",
  paused: "Paused",
  never: "Not deployed",
  unknown: "Unavailable",
};

function relativeTime(value?: string): string {
  if (!value) return "Never";
  const seconds = Math.max(0, Math.floor((Date.now() - Date.parse(value)) / 1000));
  if (seconds < 60) return "Just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

function Icon({ name }: { name: "pulse" | "refresh" | "search" | "arrow" }) {
  const paths = {
    pulse: <path d="M3 12h4l2.2-6 4.2 12 2.2-6H21" />,
    refresh: <><path d="M20 11a8 8 0 1 0-2.3 5.7" /><path d="M20 4v7h-7" /></>,
    search: <><circle cx="11" cy="11" r="7" /><path d="m20 20-4-4" /></>,
    arrow: <><path d="M5 12h14" /><path d="m13 6 6 6-6 6" /></>,
  };
  return <svg viewBox="0 0 24 24" aria-hidden="true">{paths[name]}</svg>;
}

export function App() {
  const [overview, setOverview] = useState<Overview | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<"all" | HealthState>("all");

  const load = useCallback(async (background = false) => {
    background ? setRefreshing(true) : setLoading(true);
    try {
      const response = await fetch(API_URL, { headers: { Accept: "application/json" } });
      if (!response.ok) throw new Error(`Temps returned ${response.status}`);
      setOverview(await response.json());
      setError("");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Could not load deployments");
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const timer = window.setInterval(() => void load(true), 30_000);
    return () => window.clearInterval(timer);
  }, [load]);

  const projects = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return (overview?.projects ?? []).filter((project) => {
      const matchesFilter = filter === "all" || project.health === filter;
      const matchesQuery =
        !needle ||
        [project.name, project.slug, project.preset, project.latestDeployment?.branch]
          .filter(Boolean)
          .some((value) => value!.toLowerCase().includes(needle));
      return matchesFilter && matchesQuery;
    });
  }, [filter, overview, query]);

  if (loading) {
    return <main className="shell"><div className="loading-line" /><p className="loading-copy">Reading deployment history…</p></main>;
  }

  if (!overview) {
    return (
      <main className="shell empty-state">
        <div className="brand-mark"><Icon name="pulse" /></div>
        <h1>Deployment data is unavailable</h1>
        <p>{error || "Deployment Pulse could not reach Temps."}</p>
        <button className="primary-button" onClick={() => void load()}>Try again</button>
      </main>
    );
  }

  const attention = overview.summary.failed + overview.summary.unknown;

  return (
    <main className="shell">
      <header className="hero">
        <div>
          <div className="eyebrow"><span className="live-dot" /> Live deployment health</div>
          <h1>{overview.summary.total === 0 ? "No projects to monitor yet." : attention === 0 ? "Everything is shipping cleanly." : `${attention} project${attention === 1 ? "" : "s"} need attention.`}</h1>
          <p>{overview.summary.total === 0 ? "Create or import a project and its deployment health will appear here." : `Latest status across ${overview.summary.total} projects · refreshed ${relativeTime(overview.generatedAt)}`}</p>
        </div>
        <button className="refresh-button" onClick={() => void load(true)} disabled={refreshing}>
          <Icon name="refresh" />
          <span>{refreshing ? "Refreshing" : "Refresh"}</span>
        </button>
      </header>

      <section className="metrics" aria-label="Deployment summary">
        <article className="metric metric-primary">
          <span className="metric-label">Needs attention</span>
          <strong>{attention}</strong>
          <small>{overview.summary.failures24h} failed in the last 24h</small>
        </article>
        <article className="metric">
          <span className="metric-label">Healthy</span>
          <strong>{overview.summary.healthy}</strong>
          <small>latest deployment ready</small>
        </article>
        <article className="metric">
          <span className="metric-label">In progress</span>
          <strong>{overview.summary.active}</strong>
          <small>building or deploying now</small>
        </article>
        <article className="metric">
          <span className="metric-label">Activity</span>
          <strong>{overview.summary.deployments24h}</strong>
          <small>latest deployments in 24h</small>
        </article>
      </section>

      <section className="project-section">
        <div className="section-heading">
          <div>
            <h2>Projects</h2>
            <p>Failures and active deploys are shown first.</p>
          </div>
          <span className="result-count">{projects.length} shown</span>
        </div>

        <div className="controls">
          <label className="search-field">
            <Icon name="search" />
            <span className="sr-only">Search projects</span>
            <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search projects, branches, presets…" />
          </label>
          <div className="filters" aria-label="Filter projects">
            {FILTERS.map((item) => (
              <button key={item.value} className={filter === item.value ? "active" : ""} onClick={() => setFilter(item.value)}>
                {item.label}
                {item.value !== "all" && <span>{overview.summary[item.value]}</span>}
              </button>
            ))}
          </div>
        </div>

        {error && <div className="warning-banner">Refresh failed. Showing the last available snapshot.</div>}

        <div className="project-list">
          {projects.length === 0 ? (
            <div className="no-results">No projects match this view.</div>
          ) : projects.map((project) => {
            const deployment = project.latestDeployment;
            return (
              <article className="project-row" key={project.id}>
                <div className={`status-mark ${project.health}`} aria-hidden="true"><span /></div>
                <div className="project-identity">
                  <strong>{project.name}</strong>
                  <span>{project.slug} · {project.preset}</span>
                </div>
                <div className="deployment-copy">
                  <strong>
                    {deployment?.commit_message ||
                      (project.error ??
                        (deployment ? `Deployment #${deployment.id}` : "No deployment yet"))}
                  </strong>
                  <span>{deployment?.branch || project.sourceType}{deployment?.commit_sha ? ` · ${deployment.commit_sha.slice(0, 7)}` : ""}</span>
                </div>
                <div className="success-rate">
                  <strong>{project.successRate === null ? "—" : `${project.successRate}%`}</strong>
                  <span>last {project.recentDeployments}</span>
                </div>
                <div className="latest-time">
                  <strong>{relativeTime(deployment?.created_at)}</strong>
                  <span>{deployment ? `#${deployment.id}` : "No history"}</span>
                </div>
                <div className={`status-pill ${project.health}`}><span />{STATUS_LABEL[project.health]}</div>
                <a href={projectPath(project.slug)} target="_top" aria-label={`Open ${project.name} in Temps`}><Icon name="arrow" /></a>
              </article>
            );
          })}
        </div>
      </section>
    </main>
  );
}
