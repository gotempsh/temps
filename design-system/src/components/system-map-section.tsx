// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0


import { useEffect, useState } from "react";
import {
  Archive,
  BarChart3,
  Bot,
  Box,
  Cloud,
  Cpu,
  Filter,
  Gauge,
  Globe,
  HardDrive,
  HeartPulse,
  Mail,
  MousePointerClick,
  Users,
  Video,
} from "lucide-react";
// Sandbox: analytics hook stubbed (the live section reports tab views).
const useTrackEvent = () => (_name: string, _props?: Record<string, unknown>) => {}
import { PlatformLogo } from "@/components/platform-logos";

// Shared coordinate space for the SVG connector lines and the HTML node boxes,
// so both stay aligned as the diagram scales with container width.
const VB_W = 780;
const VB_H = 400;

type IconType = React.ComponentType<{ className?: string }>;

type Node = {
  id: string;
  label: string;
  sub?: string;
  icon: IconType;
  x: number;
  y: number;
  w: number;
  h: number;
};

// Left side: who reaches the engine. Center: the engine itself.
const NODES: Node[] = [
  { id: "visitors", label: "Visitors", sub: "public traffic", icon: Globe, x: 20, y: 31, w: 160, h: 68 },
  { id: "team", label: "Your team", sub: "console login", icon: Users, x: 20, y: 151, w: 160, h: 68 },
  { id: "agents", label: "AI agents", sub: "API key", icon: Bot, x: 20, y: 271, w: 160, h: 68 },
  {
    id: "engine",
    label: "Temps Engine",
    sub: "OTel + Sentry · health checks · alerts",
    icon: Cpu,
    x: 250,
    y: 130,
    w: 180,
    h: 110,
  },
];

// Engine's vertical center — also used as the anchor point for the right-side spoke curves.
const ENGINE_CENTER_Y = 185;
const ENGINE_CENTER_X = 340;

function elbow(x1: number, y1: number, x2: number, y2: number) {
  const mx = (x1 + x2) / 2;
  return `M ${x1} ${y1} L ${mx} ${y1} L ${mx} ${y2} L ${x2} ${y2}`;
}

// Right side: what the engine does, fanning out from one point like a semi-circle of spokes.
// SAT_W is sized to content (labels top out around 90 units wide), not the old
// 420 which left a large dead interior. SAT_X keeps the same short curve gap
// off HUB_OUT_X as before the trim — the excess canvas is removed by shrinking
// VB_W (and the container's max-width in lockstep, see below) rather than by
// stretching the curve, so nothing looks disconnected.
const HUB_OUT_X = 430;
const SAT_X = 560;
const SAT_W = 200;

// Top edge of the Applications card. The deploy edge overshoots past this so
// the card masks the overlap; see the note on `e-engine-apps`.
const APPS_TOP = 40;

const EDGES = [
  { id: "e-visitors-engine", d: elbow(180, 65, 250, 160) },
  { id: "e-team-engine", d: elbow(180, 185, 250, 185) },
  { id: "e-agents-engine", d: elbow(180, 305, 250, 210) },
  {
    id: "e-engine-apps",
    // Runs from the engine's top edge up *into* the Applications card. Every
    // other edge lands on a coordinate we control (a node's box, or a
    // cluster's `top`), but this one has to meet the Applications card's
    // *bottom*, and that card has no fixed height — it grows with its content
    // and its text does not scale with the viewBox, so its bottom edge sits at
    // a different y at every viewport width. A hardcoded endpoint left a gap.
    // The cards are opaque (`bg-background`) and paint after the SVG, so overshooting
    // into the card lets it mask the excess and the line always terminates
    // flush with whatever the rendered bottom edge happens to be.
    d: `M ${ENGINE_CENTER_X} 130 L ${ENGINE_CENTER_X} ${APPS_TOP + 8}`,
    label: "deploy",
    labelX: ENGINE_CENTER_X + 55,
    labelYAbs: 105,
  },
  {
    id: "e-engine-monitoring",
    d: `M ${ENGINE_CENTER_X} 240 L ${ENGINE_CENTER_X} 260`,
    label: "monitors",
    labelX: ENGINE_CENTER_X + 62,
    labelYAbs: 250,
  },
  {
    id: "e-engine-services",
    d: elbow(HUB_OUT_X, ENGINE_CENTER_Y, SAT_X, 90),
    label: "manages",
    labelYAbs: ENGINE_CENTER_Y + (90 - ENGINE_CENTER_Y) * 0.62,
  },
  {
    id: "e-engine-analytics",
    d: elbow(HUB_OUT_X, ENGINE_CENTER_Y, SAT_X, 173),
    label: "tracks",
    labelYAbs: ENGINE_CENTER_Y + (173 - ENGINE_CENTER_Y) * 0.62,
  },
  {
    id: "e-engine-integrations",
    d: elbow(HUB_OUT_X, ENGINE_CENTER_Y, SAT_X, 273),
    label: "connects",
    labelYAbs: ENGINE_CENTER_Y + (273 - ENGINE_CENTER_Y) * 0.62,
  },
  {
    id: "e-engine-observability",
    d: elbow(HUB_OUT_X, ENGINE_CENTER_Y, SAT_X, 358),
    label: "observes",
    labelYAbs: ENGINE_CENTER_Y + (358 - ENGINE_CENTER_Y) * 0.62,
  },
] as const;

type ClusterItem = { label: string; sub?: string; logo?: string; icon?: IconType };

const CLUSTERS: {
  id: string;
  title: string;
  top: number;
  x: number;
  w: number;
  icon?: IconType;
  items: ClusterItem[];
}[] = [
  {
    id: "applications",
    title: "Applications",
    top: APPS_TOP,
    x: 240,
    w: 200,
    icon: Box,
    items: [{ label: "Your deployed projects" }],
  },
  {
    id: "services",
    title: "Provisioned services",
    top: 20,
    x: SAT_X,
    w: SAT_W,
    items: [
      { label: "PostgreSQL", logo: "PostgreSQL" },
      { label: "MariaDB", logo: "MariaDB" },
      { label: "MongoDB", logo: "MongoDB" },
      { label: "Redis", logo: "Redis" },
      { label: "RustFS", icon: HardDrive },
    ],
  },
  {
    id: "analytics",
    title: "Analytics",
    top: 133,
    x: SAT_X,
    w: SAT_W,
    items: [
      { label: "Visitor analytics", icon: BarChart3 },
      { label: "Custom events", icon: MousePointerClick },
      { label: "Session replay", icon: Video },
      { label: "Funnel analysis", icon: Filter },
    ],
  },
  {
    id: "integrations",
    title: "Integrations",
    top: 233,
    x: SAT_X,
    w: SAT_W,
    items: [
      { label: "AWS S3", icon: Cloud },
      { label: "Scaleway", logo: "Scaleway" },
      { label: "SMTP", icon: Mail },
      { label: "Sentry-compatible", logo: "Sentry" },
    ],
  },
  {
    id: "observability",
    title: "Observability",
    top: 332,
    x: SAT_X,
    w: SAT_W,
    items: [
      { label: "OpenTelemetry", logo: "OpenTelemetry" },
      { label: "Log history", icon: Archive },
    ],
  },
  {
    id: "monitoring",
    title: "Monitoring",
    top: 260,
    x: 240,
    w: 200,
    items: [
      { label: "Uptime monitoring", icon: HeartPulse },
      { label: "Web Vitals", icon: Gauge },
    ],
  },
];

const CATEGORIES = [
  {
    id: "access",
    label: "Access",
    nodes: ["visitors", "team", "agents", "engine"],
    edges: ["e-visitors-engine", "e-team-engine", "e-agents-engine"],
    clusters: [] as string[],
    caption:
      "Visitors, your team, and AI agents all reach the same engine — through the proxy, the console, or the API directly.",
  },
  {
    id: "applications",
    label: "Applications",
    nodes: ["engine"],
    edges: ["e-engine-apps"],
    clusters: ["applications"],
    caption: "The engine builds, versions, and routes your deployed projects — the same path every push takes.",
  },
  {
    id: "services",
    label: "Services",
    nodes: ["engine"],
    edges: ["e-engine-services"],
    clusters: ["services"],
    caption:
      "It manages PostgreSQL, MariaDB, MongoDB, Redis, or RustFS — Temps' own self-hosted, S3-compatible object storage — provisioned straight from the dashboard.",
  },
  {
    id: "analytics",
    label: "Analytics",
    nodes: ["engine"],
    edges: ["e-engine-analytics"],
    clusters: ["analytics"],
    caption:
      "It tracks visitor analytics, custom events, session replay, and funnels — all stored on your own infrastructure, no PostHog or Plausible required.",
  },
  {
    id: "integrations",
    label: "Integrations",
    nodes: ["engine"],
    edges: ["e-engine-integrations"],
    clusters: ["integrations"],
    caption:
      "It connects to AWS S3 or Scaleway for object storage, sends transactional email over SMTP with click and open tracking, and accepts errors through a Sentry-compatible endpoint.",
  },
  {
    id: "observability",
    label: "Observability",
    nodes: ["engine"],
    edges: ["e-engine-observability"],
    clusters: ["observability"],
    caption:
      "It ingests OpenTelemetry traces, logs, and metrics into Postgres or ClickHouse, keeping log history compressed so retention doesn't get expensive.",
  },
  {
    id: "monitoring",
    label: "Monitoring",
    nodes: ["engine"],
    edges: ["e-engine-monitoring"],
    clusters: ["monitoring"],
    caption:
      "It checks every project for uptime continuously and tracks real-user Web Vitals per page — so you know before a user has to tell you.",
  },
] as const;

function pct(v: number, total: number) {
  return `${(v / total) * 100}%`;
}

function ItemIcon({ item }: { item: ClusterItem }) {
  if (item.logo)
    return (
      <PlatformLogo name={item.logo} className="size-3.5 shrink-0" />
    );
  const Icon = item.icon;
  if (Icon) return <Icon className="size-3.5 shrink-0 text-muted-foreground" />;
  return null;
}

const AUTO_ADVANCE_MS = 2000;

export function SystemMapSection() {
  const trackEvent = useTrackEvent();
  const [activeIndex, setActiveIndex] = useState(0);
  const [paused, setPaused] = useState(false);
  const [autoAdvance, setAutoAdvance] = useState(true);
  const active = CATEGORIES[activeIndex];

  useEffect(() => {
    if (paused || !autoAdvance) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const id = setInterval(() => {
      setActiveIndex((i) => (i + 1) % CATEGORIES.length);
    }, AUTO_ADVANCE_MS);
    return () => clearInterval(id);
    // Restarting on activeIndex change (including manual clicks) keeps the interval
    // between switches consistent instead of firing early right after a manual pick.
  }, [activeIndex, paused, autoAdvance]);

  const activeNodes = new Set<string>(active.nodes);
  const activeEdges = new Set<string>(active.edges);
  const activeClusters = new Set<string>(active.clusters);

  return (
    <section id="system-map" className="op-section w-full px-4 sm:px-8">
      <div className="mx-auto max-w-6xl">
        <p className="op-label">architecture</p>
        <h2 className="op-h1 mt-3 max-w-3xl">One engine at the center of everything.</h2>
        <p className="op-lead mt-4 max-w-2xl">
          Visitors, your team and AI agents reach the same engine. It deploys your apps, manages services, tracks
          analytics, connects integrations and watches everything running, all at once.
        </p>
      </div>

      {/* Desktop / tablet: the diagram gets its own, much wider container — it has more to show than a paragraph does. */}
      <div
        className="hidden md:block max-w-[1248px] mx-auto"
        onMouseEnter={() => setPaused(true)}
        onMouseLeave={() => setPaused(false)}
      >
        <div className="relative w-full" style={{ aspectRatio: `${VB_W} / ${VB_H}` }}>
          <svg
            viewBox={`0 0 ${VB_W} ${VB_H}`}
            className="absolute inset-0 w-full h-full"
            aria-hidden="true"
            fill="none"
          >
            {EDGES.map((edge) => (
              <path
                key={edge.id}
                d={edge.d}
                stroke="currentColor"
                strokeWidth={1.5}
                className={
                  activeEdges.has(edge.id)
                    ? "text-foreground transition-colors duration-300"
                    : "text-border transition-colors duration-300"
                }
              />
            ))}
            {EDGES.filter((edge) => activeEdges.has(edge.id)).map((edge) => (
              <circle key={`${edge.id}-flow`} r={4} className="fill-foreground">
                <animateMotion dur="1.6s" repeatCount="indefinite" path={edge.d} />
              </circle>
            ))}
          </svg>

          {EDGES.filter(
            (e): e is (typeof EDGES)[number] & { label: string; labelYAbs: number; labelX?: number } =>
              "label" in e,
          ).map((edge) => {
            const isActive = activeEdges.has(edge.id);
            const labelX = edge.labelX ?? HUB_OUT_X + (SAT_X - HUB_OUT_X) * 0.62;
            return (
              <span
                key={`${edge.id}-label`}
                className={`absolute -translate-x-1/2 -translate-y-1/2 text-[11px] font-mono px-1.5 py-0.5 bg-background transition-colors duration-300 ${
                  isActive ? "text-foreground" : "text-muted-foreground"
                }`}
                style={{ left: pct(labelX, VB_W), top: pct(edge.labelYAbs, VB_H) }}
              >
                {edge.label}
              </span>
            );
          })}

          {NODES.map((node) => {
            const isActive = activeNodes.has(node.id);
            const Icon = node.icon;
            const isEngine = node.id === "engine";
            return (
              <div
                key={node.id}
                className={`absolute flex flex-col items-center justify-center text-center gap-1 px-3 border bg-background transition-colors duration-300 ${
                  isEngine ? "op-raise" : ""
                } ${isActive ? "border-foreground" : "border-border"}`}
                style={{
                  left: pct(node.x, VB_W),
                  top: pct(node.y, VB_H),
                  width: pct(node.w, VB_W),
                  height: pct(node.h, VB_H),
                }}
              >
                <Icon className={`${isEngine ? "size-6" : "size-4"} ${isActive ? "text-foreground" : "text-muted-foreground"}`} />
                <span
                  className={`${isEngine ? "text-sm" : "text-[12px]"} font-semibold leading-tight ${
                    isActive ? "text-foreground" : "text-foreground/80"
                  }`}
                >
                  {node.label}
                </span>
                {node.sub && (
                  <span className="font-mono text-[11px] text-muted-foreground leading-tight max-w-[90%]">{node.sub}</span>
                )}
              </div>
            );
          })}

          {CLUSTERS.map((cluster) => {
            const isActive = activeClusters.has(cluster.id);
            const TitleIcon = cluster.icon;
            return (
              <div
                key={cluster.id}
                className={`absolute border bg-background px-4 py-3 transition-colors duration-300 ${
                  isActive ? "border-foreground" : "border-border"
                }`}
                style={{
                  left: pct(cluster.x, VB_W),
                  top: pct(cluster.top, VB_H),
                  width: pct(cluster.w, VB_W),
                }}
              >
                <div className="flex items-center gap-1.5 mb-2">
                  {TitleIcon && (
                    <TitleIcon className={`size-3.5 ${isActive ? "text-foreground" : "text-muted-foreground"}`} />
                  )}
                  <p
                    className={`op-label ${
                      isActive ? "text-foreground" : "text-muted-foreground"
                    }`}
                  >
                    {cluster.title}
                  </p>
                </div>
                <ul className="flex flex-col gap-1.5">
                  {cluster.items.map((item) => (
                    <li key={item.label} className="flex items-center gap-2">
                      <ItemIcon item={item} />
                      <span className="text-xs text-foreground leading-tight">
                        {item.label}
                        {item.sub && <span className="text-muted-foreground"> — {item.sub}</span>}
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
            );
          })}
        </div>

        <p className="sr-only">
          Diagram of the Temps architecture. Visitors, your team, and AI agents all reach the Temps engine — through
          the proxy, the console, or the API directly. From there, the engine deploys your applications, manages
          provisioned services (PostgreSQL, MariaDB, MongoDB, Redis, or RustFS, its own self-hosted S3-compatible
          object storage), tracks visitor analytics, custom events, session replay, and funnel analysis, connects to
          external integrations (AWS S3 or Scaleway object storage, SMTP email with click and open tracking,
          Sentry-compatible error ingestion), ingests OpenTelemetry observability data with compressed log history,
          and continuously monitors uptime and Web Vitals for every project.
        </p>
      </div>

      <div className="max-w-5xl mx-auto">
        {/* Mobile: this many labeled boxes and connectors don't fit narrow — fall back to a stacked list. */}
        <div className="md:hidden flex flex-col gap-4">
          {CATEGORIES.map((cat) => (
            <div key={cat.id} className="border bg-background p-4">
              <h3 className="text-sm font-semibold text-foreground mb-1.5">{cat.label}</h3>
              <p className="text-sm text-muted-foreground leading-relaxed">{cat.caption}</p>
            </div>
          ))}
        </div>

        {/* Category toggles + caption: shown on all sizes, but only meaningfully interactive alongside the desktop diagram. */}
        <div className="hidden md:flex flex-wrap items-center justify-center gap-1 mt-10">
          {CATEGORIES.map((cat, i) => (
            <button
              key={cat.id}
              type="button"
              onClick={() => {
                setActiveIndex(i);
                setAutoAdvance(false);
                trackEvent("system_map_category_clicked", { category: cat.id });
              }}
              className={`inline-flex h-8 items-center gap-2 border px-3 text-xs transition-colors ${
                active.id === cat.id
                  ? "border-foreground bg-foreground text-background"
                  : "border-border text-muted-foreground hover:text-foreground"
              }`}
            >
              {cat.label}
            </button>
          ))}
        </div>

        <p className="hidden md:block text-center text-sm text-muted-foreground max-w-[62ch] mx-auto mt-4 leading-relaxed min-h-[3rem]">
          {active.caption}
        </p>
      </div>
    </section>
  );
}
