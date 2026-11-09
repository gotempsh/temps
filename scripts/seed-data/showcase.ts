// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
/**
 * Seed a FRESH local Temps instance with a believable "busy production" world
 * for marketing screenshots (temps.sh showcase, README).
 *
 * The world is one fictional online shop running six services. Real paths are
 * used wherever the product has one (projects are created and really deployed
 * through the API, so health checks pass on their own); history the API cannot
 * backdate (analytics, proxy logs, uptime checks) is inserted with SQL.
 *
 * Everything is derived from a fixed PRNG seed, so reruns produce the same
 * shapes and the screenshots stay reproducible. Timestamps are relative to now.
 *
 * Usage:
 *   TEMPS_API=http://127.0.0.1:8180 TEMPS_PASSWORD_FILE=~/.temps-dev/slot-N.admin-password \
 *   DATABASE_URL=postgres://temps:temps@localhost:5432/temps_sN \
 *     bun run showcase.ts [projects|groups|services|domains|email|deployments|traffic|uptime|errors|traces|ai|chat|all|top-up]
 *
 * Only run this against a throwaway instance: it writes thousands of rows.
 */

import postgres from "postgres";
import { encodeExportTraceServiceRequest, SpanKind, StatusCode, type AttrValue, type ResourceSpanGroup, type SeedSpan } from "./otlp.ts";
import { mintApiKey } from "./admin-key.ts";

const API = process.env.TEMPS_API || "http://127.0.0.1:8080";
const EMAIL = process.env.TEMPS_EMAIL || "dev@temps.sh";
const DATABASE_URL = process.env.DATABASE_URL;
const PASSWORD =
  process.env.TEMPS_PASSWORD ||
  (process.env.TEMPS_PASSWORD_FILE
    ? (await Bun.file(process.env.TEMPS_PASSWORD_FILE.replace(/^~/, process.env.HOME || "")).text()).trim()
    : undefined);
const DAYS = 30;
const PROXY_DAYS = 7;
const IMAGE = process.env.SHOWCASE_IMAGE || "nginxinc/nginx-unprivileged:alpine";
const IMAGE_PORT = 8080;

if (!DATABASE_URL || !PASSWORD) {
  console.error("DATABASE_URL and TEMPS_PASSWORD (or TEMPS_PASSWORD_FILE) are required");
  process.exit(1);
}

const sql = postgres(DATABASE_URL, { max: 4, onnotice: () => {} });

// ── deterministic randomness ──────────────────────────────────────────────
let seed = 0x7e3a5;
function rand(): number {
  seed |= 0;
  seed = (seed + 0x6d2b79f5) | 0;
  let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
}
const ri = (a: number, b: number) => Math.floor(rand() * (b - a + 1)) + a;
const pick = <T>(a: readonly T[]): T => a[Math.floor(rand() * a.length)];
function weighted<T>(items: readonly (readonly [T, number])[]): T {
  const total = items.reduce((s, [, w]) => s + w, 0);
  let r = rand() * total;
  for (const [v, w] of items) if ((r -= w) <= 0) return v;
  return items[items.length - 1][0];
}
/** Log-normal-ish latency around a median with a long right tail. */
function latency(median: number, spread = 0.45): number {
  const u = Math.max(1e-9, rand());
  const v = rand();
  const z = Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
  return Math.max(1, Math.round(median * Math.exp(z * spread)));
}
const uuid = () =>
  "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
    const r = Math.floor(rand() * 16);
    return (c === "x" ? r : (r & 0x3) | 0x8).toString(16);
  });
const nanoid = (n: number) =>
  Array.from({ length: n }, () => "abcdefghijklmnopqrstuvwxyz0123456789"[Math.floor(rand() * 36)]).join("");

const NOW = Date.now();
const HOUR = 3600_000;
const DAY = 24 * HOUR;

// ── the world ─────────────────────────────────────────────────────────────
type Kind = "shop" | "api" | "search" | "notify" | "marketing" | "docs" | "admin" | "worker" | "status" | "reco" | "returns";
interface ProjectSpec {
  name: string;
  kind: Kind;
  /** Unique visitors per day at the end of the window (0 = no browser traffic). */
  visitors: number;
  /** Proxied requests per day. */
  requests: number;
  /** How long ago the current deployment went live. */
  deployedAgoH: number;
  /** Median health-check latency, ms. */
  checkMs: number;
  host: string;
  /** Framework preset: drives the logo on the project card. */
  preset: string;
  /** Where the code lives: drives the source logo. null keeps it a Docker image. */
  repo: { provider: "github" | "gitlab"; owner: string } | null;
}

const PROJECTS: ProjectSpec[] = [
  { name: "storefront", kind: "shop", visitors: 1480, requests: 14000, deployedAgoH: 3, checkMs: 38, host: "shop.example.com", preset: "nextjs", repo: { provider: "github", owner: "northside-supply" } },
  { name: "checkout-api", kind: "api", visitors: 0, requests: 26000, deployedAgoH: 27, checkMs: 64, host: "api.example.com", preset: "go", repo: { provider: "github", owner: "northside-supply" } },
  { name: "search-api", kind: "search", visitors: 0, requests: 9400, deployedAgoH: 51, checkMs: 12, host: "search.example.com", preset: "rust", repo: { provider: "github", owner: "northside-supply" } },
  { name: "marketing-site", kind: "marketing", visitors: 910, requests: 6200, deployedAgoH: 74, checkMs: 29, host: "www.example.com", preset: "astro", repo: { provider: "github", owner: "northside-supply" } },
  { name: "docs", kind: "docs", visitors: 640, requests: 4800, deployedAgoH: 122, checkMs: 33, host: "docs.example.com", preset: "docusaurus", repo: { provider: "github", owner: "northside-supply" } },
  { name: "admin-dashboard", kind: "admin", visitors: 96, requests: 2100, deployedAgoH: 213, checkMs: 51, host: "admin.example.com", preset: "vite", repo: { provider: "gitlab", owner: "northside-ops" } },
  { name: "notifications", kind: "notify", visitors: 0, requests: 3300, deployedAgoH: 96, checkMs: 21, host: "notify.internal.example.com", preset: "nodejs", repo: { provider: "github", owner: "northside-supply" } },
  { name: "billing-worker", kind: "worker", visitors: 0, requests: 700, deployedAgoH: 338, checkMs: 44, host: "billing.internal.example.com", preset: "python", repo: null },
  // Standalone services: not part of any project, so the dashboard shows
  // both shapes side by side.
  { name: "status-page", kind: "status", visitors: 210, requests: 1600, deployedAgoH: 160, checkMs: 18, host: "status.example.com", preset: "sveltekit", repo: { provider: "github", owner: "northside-supply" } },
  { name: "recommendations", kind: "reco", visitors: 0, requests: 7800, deployedAgoH: 40, checkMs: 58, host: "reco.internal.example.com", preset: "fastapi", repo: { provider: "github", owner: "northside-data" } },
  { name: "returns-portal", kind: "returns", visitors: 260, requests: 2400, deployedAgoH: 64, checkMs: 47, host: "returns.example.com", preset: "laravel", repo: { provider: "github", owner: "northside-supply" } },
];

/** Checkout payment-provider timeouts two days ago: the incident every screen agrees on. */
const INCIDENT = { start: NOW - 2 * DAY - 5 * HOUR - 17 * 60_000, minutes: 14 };
/** A launch post that sent a traffic spike to the marketing site and shop. */
const LAUNCH_DAY_AGO = 9;

const PAGES: Record<Kind, readonly (readonly [string, string, number])[]> = {
  shop: [
    ["/", "Northside Supply — Everyday outdoor gear", 30],
    ["/collections/new-arrivals", "New arrivals — Northside Supply", 14],
    ["/collections/jackets", "Jackets — Northside Supply", 10],
    ["/collections/backpacks", "Backpacks — Northside Supply", 8],
    ["/products/alpine-shell-jacket", "Alpine Shell Jacket — Northside Supply", 9],
    ["/products/trail-runner-pack-22l", "Trail Runner Pack 22L — Northside Supply", 7],
    ["/products/merino-base-layer", "Merino Base Layer — Northside Supply", 6],
    ["/products/insulated-bottle-750", "Insulated Bottle 750 ml — Northside Supply", 5],
    ["/search", "Search — Northside Supply", 6],
    ["/cart", "Your cart — Northside Supply", 9],
    ["/checkout", "Checkout — Northside Supply", 5],
    ["/checkout/confirmation", "Order confirmed — Northside Supply", 2],
    ["/account/orders", "Your orders — Northside Supply", 3],
  ],
  marketing: [
    ["/", "Northside Supply — Gear that lasts", 34],
    ["/about", "About us — Northside Supply", 8],
    ["/journal", "Journal — Northside Supply", 10],
    ["/journal/packing-for-a-week-in-the-alps", "Packing for a week in the Alps", 9],
    ["/journal/repair-dont-replace", "Repair, don't replace", 7],
    ["/journal/winter-layering-guide", "The winter layering guide", 8],
    ["/stores", "Store locator — Northside Supply", 6],
    ["/sustainability", "Sustainability — Northside Supply", 5],
    ["/careers", "Careers — Northside Supply", 3],
  ],
  docs: [
    ["/", "Partner API docs", 14],
    ["/getting-started", "Getting started — Partner API", 16],
    ["/authentication", "Authentication — Partner API", 11],
    ["/reference/orders", "Orders — API reference", 13],
    ["/reference/inventory", "Inventory — API reference", 10],
    ["/reference/webhooks", "Webhooks — API reference", 9],
    ["/guides/rate-limits", "Rate limits — Partner API", 6],
    ["/guides/idempotency", "Idempotency keys — Partner API", 5],
    ["/changelog", "Changelog — Partner API", 4],
  ],
  admin: [
    ["/orders", "Orders · Admin", 30],
    ["/orders/[id]", "Order details · Admin", 22],
    ["/customers", "Customers · Admin", 12],
    ["/inventory", "Inventory · Admin", 14],
    ["/refunds", "Refunds · Admin", 6],
    ["/reports/sales", "Sales report · Admin", 8],
  ],
  status: [
    ["/", "Northside Supply status", 70],
    ["/incidents/payment-provider-timeouts", "Payment provider timeouts — Northside status", 16],
    ["/history", "Incident history — Northside status", 9],
    ["/subscribe", "Subscribe to updates — Northside status", 5],
  ],
  returns: [
    ["/", "Start a return — Northside Supply", 30],
    ["/returns/new", "Return an item — Northside Supply", 22],
    ["/returns/[id]", "Return status — Northside Supply", 20],
    ["/returns/[id]/label", "Your shipping label — Northside Supply", 9],
    ["/help/returns-policy", "Returns policy — Northside Supply", 12],
  ],
  api: [],
  search: [],
  notify: [],
  worker: [],
  reco: [],
};

const API_ROUTES: Record<Kind, readonly (readonly [string, string, number, number])[]> = {
  // method, path, weight, median latency ms
  api: [
    ["GET", "/v1/cart", 22, 18],
    ["POST", "/v1/cart/items", 12, 31],
    ["GET", "/v1/products", 18, 24],
    ["GET", "/v1/inventory/availability", 10, 15],
    ["POST", "/v1/checkout/sessions", 6, 92],
    ["POST", "/v1/payments", 4, 340],
    ["GET", "/v1/orders", 5, 41],
    ["POST", "/v1/webhooks/payments", 3, 27],
    ["GET", "/healthz", 8, 2],
  ],
  shop: [
    ["GET", "/api/cart", 30, 22],
    ["GET", "/api/recommendations", 18, 64],
    ["POST", "/api/cart/items", 10, 38],
    ["GET", "/api/search", 12, 48],
  ],
  search: [
    ["GET", "/v1/search", 46, 9],
    ["GET", "/v1/suggest", 30, 4],
    ["GET", "/v1/facets", 12, 7],
    ["POST", "/v1/index/products", 2, 85],
    ["GET", "/healthz", 8, 1],
  ],
  notify: [
    ["POST", "/v1/email/order-confirmation", 26, 140],
    ["POST", "/v1/email/shipping-update", 14, 130],
    ["POST", "/v1/push", 18, 60],
    ["POST", "/v1/webhooks/email-events", 22, 11],
    ["GET", "/healthz", 8, 1],
  ],
  marketing: [["POST", "/api/newsletter", 1, 120]],
  docs: [["GET", "/api/search", 1, 35]],
  admin: [
    ["GET", "/api/orders", 30, 55],
    ["GET", "/api/customers", 12, 47],
    ["POST", "/api/refunds", 3, 140],
    ["GET", "/api/reports/sales", 4, 380],
  ],
  status: [
    ["GET", "/api/summary.json", 40, 6],
    ["GET", "/api/incidents", 8, 9],
    ["POST", "/api/subscribe", 1, 80],
  ],
  reco: [
    ["GET", "/v1/recommendations/product", 40, 48],
    ["GET", "/v1/recommendations/home", 24, 62],
    ["POST", "/v1/events/view", 30, 6],
    ["POST", "/v1/models/reload", 1, 2400],
    ["GET", "/healthz", 8, 2],
  ],
  returns: [
    ["GET", "/api/orders/lookup", 14, 90],
    ["POST", "/api/returns", 10, 180],
    ["GET", "/api/returns/{id}", 20, 35],
    ["POST", "/api/returns/{id}/label", 6, 420],
  ],
  worker: [
    ["POST", "/jobs/invoices.generate", 10, 410],
    ["POST", "/jobs/subscriptions.renew", 6, 230],
    ["GET", "/healthz", 8, 2],
  ],
};

const STATIC = ["/_next/static/chunks/main-app.js", "/_next/static/css/app.css", "/favicon.ico", "/og-image.png", "/fonts/inter-var.woff2", "/images/hero.avif", "/robots.txt", "/sitemap.xml"] as const;

const BROWSERS = [["Chrome", ["128.0", "129.0", "130.0"], 58], ["Safari", ["17.6", "18.0", "18.1"], 24], ["Firefox", ["130.0", "131.0"], 7], ["Edge", ["129.0", "130.0"], 8], ["Samsung Internet", ["26.0"], 3]] as const;
const OSES = [["macOS", ["14.6", "15.0", "15.1"], 30], ["Windows", ["10", "11"], 28], ["iOS", ["17.6", "18.0", "18.1"], 22], ["Android", ["14", "15"], 16], ["Linux", ["6.8"], 4]] as const;
const LANGS = [["en-US", 46], ["en-GB", 11], ["de-DE", 10], ["fr-FR", 6], ["es-ES", 6], ["pt-BR", 5], ["nl-NL", 4], ["ja-JP", 4], ["it-IT", 3], ["sv-SE", 2], ["pl-PL", 3]] as const;

const SOURCES = [
  // referrer, channel, weight
  [null, "direct", 30],
  ["https://www.google.com/", "organic_search", 34],
  ["https://duckduckgo.com/", "organic_search", 4],
  ["https://www.bing.com/", "organic_search", 3],
  ["https://www.instagram.com/", "social", 7],
  ["https://www.reddit.com/", "social", 4],
  ["https://news.ycombinator.com/", "referral", 2],
  ["https://www.youtube.com/", "social", 3],
  ["https://l.facebook.com/", "social", 3],
  ["https://mail.google.com/", "email", 6],
  ["https://www.google.com/aclk", "paid_search", 4],
] as const;

const CITIES = [
  // city, region, country, cc, lat, lon, tz, eu, weight
  ["New York", "New York", "United States", "US", 40.71, -74.01, "America/New_York", false, 9],
  ["San Francisco", "California", "United States", "US", 37.77, -122.42, "America/Los_Angeles", false, 6],
  ["Seattle", "Washington", "United States", "US", 47.61, -122.33, "America/Los_Angeles", false, 4],
  ["Chicago", "Illinois", "United States", "US", 41.88, -87.63, "America/Chicago", false, 4],
  ["Austin", "Texas", "United States", "US", 30.27, -97.74, "America/Chicago", false, 3],
  ["Denver", "Colorado", "United States", "US", 39.74, -104.99, "America/Denver", false, 3],
  ["Boston", "Massachusetts", "United States", "US", 42.36, -71.06, "America/New_York", false, 3],
  ["Toronto", "Ontario", "Canada", "CA", 43.65, -79.38, "America/Toronto", false, 4],
  ["Vancouver", "British Columbia", "Canada", "CA", 49.28, -123.12, "America/Vancouver", false, 3],
  ["London", "England", "United Kingdom", "GB", 51.51, -0.13, "Europe/London", false, 8],
  ["Manchester", "England", "United Kingdom", "GB", 53.48, -2.24, "Europe/London", false, 2],
  ["Berlin", "Berlin", "Germany", "DE", 52.52, 13.4, "Europe/Berlin", true, 5],
  ["Munich", "Bavaria", "Germany", "DE", 48.14, 11.58, "Europe/Berlin", true, 4],
  ["Hamburg", "Hamburg", "Germany", "DE", 53.55, 9.99, "Europe/Berlin", true, 2],
  ["Paris", "Île-de-France", "France", "FR", 48.86, 2.35, "Europe/Paris", true, 4],
  ["Lyon", "Auvergne-Rhône-Alpes", "France", "FR", 45.76, 4.84, "Europe/Paris", true, 2],
  ["Amsterdam", "North Holland", "Netherlands", "NL", 52.37, 4.9, "Europe/Amsterdam", true, 3],
  ["Madrid", "Madrid", "Spain", "ES", 40.42, -3.7, "Europe/Madrid", true, 3],
  ["Barcelona", "Catalonia", "Spain", "ES", 41.39, 2.17, "Europe/Madrid", true, 3],
  ["Milan", "Lombardy", "Italy", "IT", 45.46, 9.19, "Europe/Rome", true, 2],
  ["Stockholm", "Stockholm", "Sweden", "SE", 59.33, 18.07, "Europe/Stockholm", true, 2],
  ["Zurich", "Zurich", "Switzerland", "CH", 47.38, 8.54, "Europe/Zurich", false, 2],
  ["Warsaw", "Masovia", "Poland", "PL", 52.23, 21.01, "Europe/Warsaw", true, 2],
  ["Dublin", "Leinster", "Ireland", "IE", 53.35, -6.26, "Europe/Dublin", true, 2],
  ["São Paulo", "São Paulo", "Brazil", "BR", -23.55, -46.63, "America/Sao_Paulo", false, 3],
  ["Mexico City", "CDMX", "Mexico", "MX", 19.43, -99.13, "America/Mexico_City", false, 2],
  ["Tokyo", "Tokyo", "Japan", "JP", 35.68, 139.69, "Asia/Tokyo", false, 3],
  ["Sydney", "New South Wales", "Australia", "AU", -33.87, 151.21, "Australia/Sydney", false, 3],
  ["Melbourne", "Victoria", "Australia", "AU", -37.81, 144.96, "Australia/Melbourne", false, 2],
  ["Singapore", "Singapore", "Singapore", "SG", 1.35, 103.82, "Asia/Singapore", false, 2],
  ["Bengaluru", "Karnataka", "India", "IN", 12.97, 77.59, "Asia/Kolkata", false, 3],
  ["Seoul", "Seoul", "South Korea", "KR", 37.57, 126.98, "Asia/Seoul", false, 1],
  ["Cape Town", "Western Cape", "South Africa", "ZA", -33.92, 18.42, "Africa/Johannesburg", false, 1],
] as const;

// ── API helper ────────────────────────────────────────────────────────────
let cookie = "";
async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  // JSON handlers reject an empty body, even for POSTs that take no fields.
  const body = init.body ?? (init.method && init.method !== "GET" && init.method !== "DELETE" ? "{}" : undefined);
  const res = await fetch(`${API}/api${path}`, {
    ...init,
    body,
    headers: { "Content-Type": "application/json", ...(cookie ? { Cookie: cookie } : {}), ...(init.headers || {}) },
  });
  const setCookie = res.headers.getSetCookie?.() ?? [];
  if (setCookie.length) cookie = setCookie.map((c) => c.split(";")[0]).join("; ");
  const text = await res.text();
  if (!res.ok) throw new Error(`${init.method || "GET"} ${path} -> ${res.status}: ${text.slice(0, 400)}`);
  return (text ? JSON.parse(text) : undefined) as T;
}
async function login() {
  await api("/auth/login", { method: "POST", body: JSON.stringify({ email: EMAIL, password: PASSWORD }) });
}

// ── stage: projects ───────────────────────────────────────────────────────
interface Seeded { spec: ProjectSpec; projectId: number; envId: number; deploymentId: number }

async function seedProjects(): Promise<Seeded[]> {
  console.log("\n▸ projects");
  await login();
  const existing = await api<{ projects: { id: number; slug: string }[] }>("/projects?per_page=100");
  const out: Seeded[] = [];
  for (const spec of PROJECTS) {
    let projectId = existing.projects.find((p) => p.slug === spec.name)?.id;
    if (!projectId) {
      const p = await api<{ id: number }>("/projects", {
        method: "POST",
        body: JSON.stringify({
          name: spec.name,
          source_type: "docker_image",
          preset: "dockerfile",
          exposed_port: IMAGE_PORT,
          is_web_app: spec.kind !== "worker",
        }),
      });
      projectId = p.id;
      console.log(`  created ${spec.name} (#${projectId})`);
    }
    const envs = await api<{ id: number; name: string; current_deployment_id: number | null }[]>(`/projects/${projectId}/environments`);
    const env = envs.find((e) => e.name === "production") ?? envs[0];
    let deploymentId = env.current_deployment_id ?? 0;
    if (!deploymentId) {
      const d = await api<{ id: number }>(`/projects/${projectId}/environments/${env.id}/deploy/image`, {
        method: "POST",
        body: JSON.stringify({ image_ref: IMAGE, health_check_path: "/" }),
      });
      deploymentId = d.id;
    }
    out.push({ spec, projectId, envId: env.id, deploymentId });
  }
  // Wait for every deployment to go live (they run in parallel).
  for (const s of out) {
    for (let i = 0; ; i++) {
      const d = await api<{ status: string; cancelled_reason?: string }>(`/projects/${s.projectId}/deployments/${s.deploymentId}`);
      if (d.status === "completed") break;
      if (["failed", "cancelled"].includes(d.status) || i > 100)
        throw new Error(`deployment ${s.deploymentId} of ${s.spec.name}: ${d.status} ${d.cancelled_reason ?? ""}`);
      await Bun.sleep(3000);
    }
    console.log(`  ${s.spec.name}: deployment #${s.deploymentId} live`);
  }
  // "Deployed just now" on every card reads fake: spread the go-live times out.
  for (const s of out) {
    const at = new Date(NOW - s.spec.deployedAgoH * HOUR);
    const created = new Date(at.getTime() - ri(70, 160) * 1000);
    await sql`UPDATE deployments SET created_at = ${created}, started_at = ${created}, deploying_at = ${created},
                ready_at = ${at}, finished_at = ${at}, updated_at = ${at}
              WHERE id = ${s.deploymentId} AND (finished_at IS NULL OR finished_at > ${at})`;
    await sql`UPDATE projects SET last_deployment = ${at}, created_at = LEAST(created_at, ${new Date(NOW - (DAYS + 12) * DAY)})
              WHERE id = ${s.projectId}`;
    await sql`UPDATE environments SET created_at = LEAST(created_at, ${new Date(NOW - (DAYS + 12) * DAY)}) WHERE id = ${s.envId}`;
  }
  // Every card showing a Docker whale reads as a demo. Give each service the
  // framework and repository it would really have; the deployed container is
  // the same placeholder either way.
  for (const s of out) {
    const r = s.spec.repo;
    const host = r?.provider === "gitlab" ? "gitlab.com" : "github.com";
    await sql`UPDATE projects SET preset = ${s.spec.preset}, preset_config = ${sql.json({ preset: s.spec.preset })},
                source_type = ${r ? "git" : "docker_image"}, main_branch = ${r ? "main" : ""},
                repo_owner = ${r?.owner ?? ""}, repo_name = ${r ? s.spec.name : ""},
                git_url = ${r ? `https://${host}/${r.owner}/${s.spec.name}.git` : null}, is_public_repo = ${!!r}
              WHERE id = ${s.projectId}`;
  }
  // Drop failed attempts so deployment lists only show the story.
  await sql`DELETE FROM deployments WHERE state = 'failed' AND project_id IN ${sql(out.map((s) => s.projectId))}`.catch(() => {});
  return out;
}

/** The projects the services ship as, so the dashboard shows them grouped. */
const GROUPS = [
  { name: "Northside Shop", description: "Storefront, checkout and search", services: ["storefront", "checkout-api", "search-api", "notifications"] },
  { name: "Content", description: "Marketing site and docs", services: ["marketing-site", "docs"] },
  { name: "Back office", description: "Ops dashboard and billing", services: ["admin-dashboard", "billing-worker"] },
];

async function seedGroups(seeded: Seeded[]) {
  console.log("\n▸ project groups");
  await login();
  const listed = await api<{ groups: { name: string }[] }>("/project-groups?page_size=100").catch(() => null);
  const existing = listed?.groups ?? [];
  for (const g of GROUPS) {
    if (existing.some((e) => e.name === g.name)) { console.log(`  ${g.name}: exists`); continue; }
    const ids = g.services.map((n) => seeded.find((s) => s.spec.name === n)?.projectId).filter((id): id is number => !!id);
    await api("/project-groups", { method: "POST", body: JSON.stringify({ name: g.name, description: g.description, project_ids: ids }) });
    console.log(`  ${g.name}: ${ids.length} services`);
  }
}

async function loadSeeded(): Promise<Seeded[]> {
  const rows = await sql`
    SELECT p.id AS project_id, p.slug, e.id AS env_id, e.current_deployment_id
    FROM projects p JOIN environments e ON e.project_id = p.id AND e.name = 'production'
    WHERE p.deleted_at IS NULL`;
  return PROJECTS.flatMap((spec) => {
    const r = rows.find((x) => x.slug === spec.name);
    return r && r.current_deployment_id
      ? [{ spec, projectId: r.project_id, envId: r.env_id, deploymentId: r.current_deployment_id }]
      : [];
  });
}

// ── stage: services ───────────────────────────────────────────────────────
/**
 * The databases the services use, created for real (each is a running
 * container), linked to the projects that use them, and dated to when the
 * projects moved over, not to the minute this script ran.
 */
const SERVICES = [
  { name: "orders-db", type: "postgres", parameters: { database: "orders", username: "checkout" }, projects: ["checkout-api", "admin-dashboard", "billing-worker"], agoD: 41 },
  { name: "sessions-cache", type: "redis", parameters: {}, projects: ["storefront", "admin-dashboard"], agoD: 40 },
  { name: "catalog", type: "mongodb", parameters: { database: "catalog", username: "search" }, projects: ["search-api"], agoD: 38 },
  { name: "media", type: "rustfs", parameters: {}, projects: ["storefront"], agoD: 37 },
  { name: "returns-db", type: "mariadb", parameters: {}, projects: ["returns-portal"], agoD: 19 },
] as const;

async function seedServices(seeded: Seeded[]) {
  console.log("\n▸ services");
  await login();
  const existing = await api<{ id: number; name: string; status: string }[]>("/external-services");
  for (const svc of SERVICES) {
    const ids = svc.projects.map((n) => seeded.find((s) => s.spec.name === n)?.projectId).filter((id): id is number => !!id);
    let found = existing.find((e) => e.name === svc.name);
    if (!found) {
      found = await api<{ id: number; name: string; status: string }>("/external-services", {
        method: "POST",
        body: JSON.stringify({ name: svc.name, service_type: svc.type, parameters: svc.parameters, project_id: ids[0] }),
      });
    } else if (found.status !== "running") {
      await api(`/external-services/${found.id}/start`, { method: "POST" });
    }
    const linked = await api<{ project: { id: number } }[]>(`/external-services/${found.id}/projects`);
    for (const id of ids)
      if (!linked.some((l) => l.project.id === id))
        await api(`/external-services/${found.id}/projects`, { method: "POST", body: JSON.stringify({ project_id: id }) });
    await sql`UPDATE external_services SET created_at = ${new Date(NOW - svc.agoD * DAY)} WHERE id = ${found.id}`;
    console.log(`  ${svc.name} (${svc.type}): linked to ${svc.projects.join(", ")}`);
  }
}

// ── stage: domains ────────────────────────────────────────────────────────
/**
 * The shop's public hostnames with their certificates on a rolling renewal
 * schedule (Let's Encrypt renews 30 days before expiry, so none is older than
 * ~60 days), plus one wildcard over DNS-01. Names are the projects'
 * `example.com` hosts; certificates themselves are not stored (nothing can
 * issue for those names), so the proxy has nothing to load.
 */
async function seedDomains() {
  console.log("\n▸ domains");
  const hosts = PROJECTS.map((p) => p.host).filter((h) => !h.includes(".internal."));
  const rows = [
    { domain: "*.example.com", status: "active", method: "dns-01", wildcard: true, renewedAgoD: 18 },
    ...hosts.map((domain, i) => ({ domain, status: "active", method: "http-01", wildcard: false, renewedAgoD: [9, 27, 41, 3, 55, 22, 33][i % 7]! })),
  ];
  await sql`DELETE FROM domains WHERE domain LIKE '%example.com'`;
  for (const r of rows) {
    const renewed = r.renewedAgoD >= 0 ? new Date(NOW - r.renewedAgoD * DAY - ri(1, 20) * HOUR) : null;
    await sql`INSERT INTO domains (domain, status, verification_method, is_wildcard, last_renewed, expiration_time, created_at, updated_at)
              VALUES (${r.domain}, ${r.status}, ${r.method}, ${r.wildcard}, ${renewed},
                      ${renewed ? new Date(renewed.getTime() + 90 * DAY) : null},
                      ${new Date(NOW - (r.renewedAgoD >= 0 ? 40 : 0.1) * DAY)}, now())`;
  }
  console.log(`  ${rows.length} domains`);
}

// ── stage: email ──────────────────────────────────────────────────────────
/**
 * Transactional email the shop sends through Amazon SES: a verified sending
 * domain and a month of order confirmations, shipping updates, password
 * resets and return labels, with the delivery, open, click and bounce events
 * a provider reports. Credentials are AWS's documentation example pair,
 * encrypted the way the server encrypts real ones.
 */
const FIRST = ["alex", "jordan", "maria", "chen", "fatima", "liam", "sofia", "noah", "aisha", "mateo", "emma", "yuki", "omar", "lena", "ravi", "grace"];
const LAST = ["morgan", "silva", "novak", "kim", "haddad", "brennan", "rossi", "okoro", "lindqvist", "tanaka", "duarte", "fischer"];
const MAILS = [
  { tag: "order-confirmation", project: "checkout-api", perDay: 62, from: "orders", subject: (n: string) => `Your order ${n} is confirmed`, open: 0.71, click: 0.24 },
  { tag: "shipping-update", project: "notifications", perDay: 51, from: "orders", subject: (n: string) => `Order ${n} is on its way`, open: 0.64, click: 0.38 },
  { tag: "password-reset", project: "storefront", perDay: 14, from: "account", subject: () => "Reset your Northside password", open: 0.83, click: 0.69 },
  { tag: "return-label", project: "returns-portal", perDay: 9, from: "returns", subject: (n: string) => `Your return label for ${n}`, open: 0.77, click: 0.52 },
];

async function seedEmail(seeded: Seeded[]) {
  console.log("\n▸ email");
  const projectId = (name: string) => seeded.find((s) => s.spec.name === name)?.projectId ?? null;
  await sql`DELETE FROM email_events WHERE email_id IN (SELECT id FROM emails WHERE from_address LIKE '%@example.com')`;
  await sql`DELETE FROM emails WHERE from_address LIKE '%@example.com'`;
  await sql`DELETE FROM email_domains WHERE domain = 'example.com'`;
  await sql`DELETE FROM email_providers WHERE name = 'Amazon SES'`;
  const credentials = await encryptLikeServer(JSON.stringify({ access_key_id: "AKIAIOSFODNN7EXAMPLE", secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" }));
  const since = new Date(NOW - 34 * DAY);
  const [provider] = await sql`INSERT INTO email_providers (name, provider_type, region, credentials, is_active, created_at, updated_at)
    VALUES ('Amazon SES', 'ses', 'eu-west-1', ${credentials}, true, ${since}, ${since}) RETURNING id`;
  const [domain] = await sql`INSERT INTO email_domains (provider_id, domain, status, spf_record_name, spf_record_value, dkim_selector,
      dkim_record_name, dkim_record_value, mx_record_name, mx_record_value, mx_record_priority, last_verified_at, created_at, updated_at)
    VALUES (${provider!.id}, 'example.com', 'verified', 'mail.example.com', 'v=spf1 include:amazonses.com ~all', 'temps1',
      'temps1._domainkey.example.com', ${`v=DKIM1; k=rsa; p=MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQ${nanoid(40)}AQAB`},
      'mail.example.com', 'feedback-smtp.eu-west-1.amazonses.com', 10, ${new Date(NOW - 2 * HOUR)}, ${since}, now()) RETURNING id`;
  for (const m of MAILS) {
    const id = projectId(m.project);
    if (id) await sql`INSERT INTO email_domain_projects (domain_id, project_id) VALUES (${domain!.id}, ${id}) ON CONFLICT DO NOTHING`;
  }

  const emails: Record<string, unknown>[] = [];
  const events: Record<string, unknown>[] = [];
  let order = 104200;
  for (let d = 30; d >= 0; d--) {
    for (const m of MAILS) {
      const count = Math.round(m.perDay * dayFactor("shop", d));
      for (let k = 0; k < count; k++) {
        const hour = weighted(HOURLY.shop.map((w, h) => [h, w] as const));
        const at = NOW - d * DAY - (new Date(NOW).getUTCHours() - hour) * HOUR + ri(0, 3599) * 1000;
        if (at > NOW - 30_000) continue;
        const to = `${pick(FIRST)}.${pick(LAST)}${rand() < 0.3 ? ri(1, 99) : ""}@example.net`;
        // Shipping updates and return labels refer to orders placed earlier.
        const placed = m.tag === "order-confirmation" ? order++ : m.tag === "shipping-update" ? order - ri(60, 180) : order - ri(400, 1600);
        const num = `#NS-${placed}`;
        const subject = m.subject(num);
        const failed = rand() < 0.004;
        const id = uuid();
        const sent = new Date(at + ri(200, 1400));
        const bounced = !failed && rand() < 0.016;
        const complained = !failed && !bounced && rand() < 0.0008;
        const opened = !failed && !bounced && rand() < m.open && at < NOW - 2 * 60_000;
        const clicked = opened && rand() < m.click / m.open;
        const openAt = opened ? new Date(Math.min(NOW - 1000, at + latency(25 * 60_000, 1.1))) : null;
        const clickAt = clicked && openAt ? new Date(Math.min(NOW - 500, openAt.getTime() + ri(5, 240) * 1000)) : null;
        const opens = opened ? 1 + (rand() < 0.35 ? ri(1, 3) : 0) : 0;
        emails.push({
          id, domain_id: domain!.id, project_id: projectId(m.project), provider_id: provider!.id,
          from_address: `${m.from}@example.com`, from_name: "Northside Supply", to_addresses: sql.json([to]),
          subject, html_body: `<p>Hi,</p><p>${subject}.</p><p><a href="https://shop.example.com/orders/${num.slice(1)}">View your order</a></p><p>Northside Supply</p>`,
          text_body: `${subject}.\nhttps://shop.example.com/orders/${num.slice(1)}`, tags: sql.json([m.tag]),
          status: failed ? "failed" : "sent", provider_message_id: failed ? null : `0102019${nanoid(10)}-${uuid()}-000000`,
          error_message: failed ? "SES rejected the message: Throttling: Maximum sending rate exceeded" : null,
          sent_at: failed ? null : sent, created_at: new Date(at), track_opens: true, track_clicks: true,
          open_count: opens, click_count: clicked ? 1 : 0, first_opened_at: openAt, first_clicked_at: clickAt, attempt_count: failed ? 3 : 1,
        });
        if (failed) continue;
        const ev = (event_type: string, when: Date, extra: Record<string, unknown> = {}) =>
          events.push({ email_id: id, event_type, created_at: when, recipient: to, ip_address: null, user_agent: null, link_url: null, ...extra });
        ev(bounced ? "bounced" : "delivered", new Date(sent.getTime() + ri(1500, 9000)));
        if (complained) ev("complained", new Date(sent.getTime() + ri(2, 30) * 60_000));
        if (openAt) ev("opened", openAt, { user_agent: pick(["Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X)", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)", "GoogleImageProxy"]) });
        if (clickAt) ev("clicked", clickAt, { link_url: `https://shop.example.com/orders/${num.slice(1)}`, link_index: 0 });
      }
    }
  }
  await insertBatched("emails", emails, Object.keys(emails[0]!));
  await insertBatched("email_events", events, Object.keys(events[0]!));
  console.log(`  ${emails.length} emails, ${events.length} events`);
}

// ── stage: deployments ────────────────────────────────────────────────────
/**
 * A deployment list with one entry reads as a fresh install. A team shipping
 * every day leaves a history: several production deploys a day from `main`,
 * preview deploys from feature branches, the odd failed build and a push
 * cancelled by the next one. History is inserted between the first real
 * deployment and a fresh real deploy, so numbers rise with time. Each run adds
 * one real deploy per service.
 */
const AUTHORS = ["Sam Rivera", "Priya Natarajan", "Jonas Weber", "Amara Okafor", "Lucas Moreau", "Mei Lin"];
const COMMITS: Record<Kind, string[]> = {
  shop: [
    "feat(cart): keep the cart across devices for signed-in customers",
    "fix(checkout): show the shipping total before the payment step",
    "perf(images): serve product photos as AVIF with a WebP fallback",
    "feat(search): suggest categories while typing",
    "fix(pdp): stop the size picker jumping on slow connections",
    "chore(deps): bump next to 15.5",
    "feat(account): order history with tracking links",
    "fix(cart): badge count after removing the last item",
    "feat(home): autumn collection banner",
    "perf(pdp): prefetch the recommendations rail",
    "fix(i18n): EUR prices rounding to whole cents",
    "feat(checkout): save addresses for next time",
  ],
  api: [
    "fix(payments): retry card authorisation once on a gateway timeout",
    "feat(orders): idempotency keys on POST /orders",
    "perf(db): index orders by customer and created_at",
    "fix(tax): VAT for orders shipped to Northern Ireland",
    "feat(payments): Apple Pay through the card gateway",
    "chore(deps): bump pgx to v5.7",
    "fix(inventory): release reserved stock after 15 minutes",
    "feat(webhooks): sign outgoing order events",
  ],
  search: ["perf(index): cut cold-start reindex from 40s to 9s", "feat(ranking): boost in-stock items", "fix(query): typo tolerance for two-letter words", "chore(deps): bump tantivy"],
  marketing: ["content: autumn collection landing page", "feat(blog): related posts", "fix(seo): canonical URLs on paginated pages", "content: care guide for merino knitwear", "perf: lazy-load the hero video"],
  docs: ["docs(api): document order webhooks", "docs: returns policy for EU customers", "fix: broken links in the size guide", "docs(api): pagination examples"],
  admin: ["feat(orders): bulk refund", "fix(reports): timezone of the daily sales chart", "feat(stock): low-stock alerts per warehouse"],
  notify: ["feat(email): shipping confirmation with tracking map", "fix(sms): strip spaces from UK numbers", "chore(templates): new footer"],
  worker: [],
  status: ["feat: incident history page", "fix: dark mode contrast on the uptime bars"],
  reco: ["feat(model): retrain on the last 90 days", "perf: cache embeddings per category", "fix: exclude out-of-stock items"],
  returns: ["feat(labels): print-at-home return labels", "fix(refunds): partial refunds for bundles", "feat: exchange for a different size"],
};
/** Production deploys over the last two weeks, per service. */
const DEPLOY_COUNT: Record<Kind, number> = { shop: 34, api: 22, search: 10, marketing: 16, docs: 12, admin: 8, notify: 9, worker: 6, status: 5, reco: 10, returns: 9 };
const BUILD_SECS: Record<Kind, [number, number]> = { shop: [95, 160], api: [48, 80], search: [180, 260], marketing: [60, 110], docs: [70, 120], admin: [55, 90], notify: [40, 70], worker: [30, 50], status: [50, 80], reco: [90, 150], returns: [80, 130] };
/** Feature branches with a preview environment, per service. */
const PREVIEWS: Partial<Record<Kind, { branch: string; messages: string[] }[]>> = {
  shop: [
    { branch: "feat/one-page-checkout", messages: ["feat(checkout): single-page layout behind a flag", "feat(checkout): inline address validation", "fix(checkout): keep the promo code when editing the cart"] },
    { branch: "fix/cart-badge-count", messages: ["fix(cart): badge count after removing the last item"] },
  ],
  api: [{ branch: "feat/apple-pay", messages: ["feat(payments): Apple Pay merchant validation", "test(payments): Apple Pay sandbox flow"] }],
  marketing: [{ branch: "content/holiday-gift-guide", messages: ["content: holiday gift guide draft", "content: gift guide photos"] }],
};
/** Stored the way the workflow executor wraps a failed job's message. */
const wrappedFailure = (job: string, inner: string) => `Job execution failed: Required job '${job}' failed: Some("Job execution failed: ${inner}")`;
const FAILURES = [
  wrappedFailure("build_image", 'Failed to build image: Build failed: failed to solve: process \\"/bin/sh -c bun run build\\" did not complete successfully: exit code: 1'),
  wrappedFailure("mark_deployment_complete", "Health check failed: GET / returned 503 for 120s, container is unhealthy"),
];

/**
 * The API reads author and date from the stored GitHub commit, not from the
 * `commit_author` column, so history carries a commit object of that shape.
 */
function commitJson(commitSha: string, message: string, author: string, at: Date, owner: string, repo: string) {
  const login = author.toLowerCase().replace(/[^a-z]+/g, "-");
  const u = `https://api.github.com/users/${login}`;
  const user = {
    login, id: 1000 + AUTHORS.indexOf(author), node_id: `U_${login}`, avatar_url: `https://avatars.example.com/${login}`, gravatar_id: "", url: u,
    html_url: `https://github.com/${login}`, followers_url: `${u}/followers`, following_url: `${u}/following`,
    gists_url: `${u}/gists`, starred_url: `${u}/starred`, subscriptions_url: `${u}/subscriptions`,
    organizations_url: `${u}/orgs`, repos_url: `${u}/repos`, events_url: `${u}/events`,
    received_events_url: `${u}/received_events`, type: "User", site_admin: false,
  };
  const who = { name: author, email: `${login}@northside.example`, date: at.toISOString() };
  const base = `https://api.github.com/repos/${owner}/${repo}`;
  return {
    url: `${base}/commits/${commitSha}`, sha: commitSha, node_id: `C_${commitSha.slice(0, 12)}`,
    html_url: `https://github.com/${owner}/${repo}/commit/${commitSha}`, comments_url: `${base}/commits/${commitSha}/comments`,
    commit: { url: `${base}/git/commits/${commitSha}`, author: who, committer: who, message, comment_count: 0, tree: { sha: commitSha, url: `${base}/git/trees/${commitSha}` } },
    author: user, committer: user, parents: [],
  };
}

const sha = () => Array.from({ length: 40 }, () => "0123456789abcdef"[Math.floor(rand() * 16)]).join("");

/** A weekday working-hours instant on or before `at`, so pushes cluster like a team's. */
function workHour(at: number): number {
  const d = new Date(at);
  const h = d.getUTCHours();
  if (h < 7) d.setUTCHours(ri(16, 18), ri(0, 59));
  if (h < 7) d.setUTCDate(d.getUTCDate() - 1);
  else if (h > 19) d.setUTCHours(ri(15, 18), ri(0, 59));
  return Math.min(d.getTime(), at);
}

async function seedDeployments(seeded: Seeded[]) {
  console.log("\n▸ deployments");
  await login();
  const span = 14 * DAY;
  const tag = (n: number) => `v1.${14 + Math.floor(n / 10)}.${n % 10}`;

  interface Plan { s: Seeded; git: boolean; messages: string[]; build: [number, number]; liveAt: number; rows: Record<string, unknown>[] }
  const row = (p: Plan, envId: number, at: number, state: string, opts: { branch?: string; message?: string; reason?: string; n: number }) => {
    const { s, git } = p;
    const dur = ri(p.build[0], p.build[1]) * 1000;
    const created = new Date(at - dur);
    const done = state === "cancelled" ? new Date(at - dur + ri(15, 40) * 1000) : new Date(at);
    const commitSha = sha();
    const message = opts.message ?? pick(p.messages);
    const author = pick(AUTHORS);
    p.rows.push({
      project_id: s.projectId, environment_id: envId, created_at: created, updated_at: done,
      slug: `${s.spec.name}-h${opts.n}`, state, metadata: sql.json(sourceMeta(p, opts.n)),
      started_at: created, deploying_at: state === "completed" ? new Date(at - 20_000) : null,
      ready_at: state === "completed" ? done : null, finished_at: done,
      branch_ref: git ? (opts.branch ?? "main") : null, commit_sha: git ? commitSha : null,
      commit_message: git ? message : null, commit_author: author,
      commit_json: git ? sql.json(commitJson(commitSha, message, author, new Date(created.getTime() - ri(1, 6) * 60_000), s.spec.repo!.owner, s.spec.name)) : null,
      cancelled_reason: opts.reason ?? null, api_enabled: false, metrics_enabled: false, metrics_path: "/metrics",
      docker_socket_mounted: false,
    });
  };
  const sourceMeta = (p: Plan, n: number) =>
    p.git
      ? { isRollback: false, healthCheckPath: "/", deploymentSourceType: "git" }
      : { isRollback: false, healthCheckPath: "/", deploymentSourceType: "docker_image", externalImageRef: `ghcr.io/northside-supply/${p.s.spec.name}:${tag(n)}`, imageUploadedLocally: false };
  /** Give a real deployment the commit (or image tag) it would have had. */
  const dress = async (p: Plan, id: number, n: number, message: string, at: Date) => {
    const commitSha = sha();
    const author = pick(AUTHORS);
    await sql`UPDATE deployments SET branch_ref = ${p.git ? "main" : null}, commit_sha = ${p.git ? commitSha : null},
                commit_message = ${p.git ? message : null}, commit_author = ${author},
                commit_json = ${p.git ? sql.json(commitJson(commitSha, message, author, at, p.s.spec.repo!.owner, p.s.spec.name)) : null},
                metadata = ${sql.json(sourceMeta(p, n))}
              WHERE id = ${id}`;
  };
  /** Insert, oldest first, the planned rows that start before `until`: ids then rise with time. */
  const flush = async (p: Plan, until = Infinity) => {
    const due = p.rows.filter((r) => (r.created_at as Date).getTime() < until).sort((a, b) => (a.created_at as Date).getTime() - (b.created_at as Date).getTime());
    p.rows = p.rows.filter((r) => !due.includes(r));
    if (due.length) await insertBatched("deployments", due, Object.keys(due[0]!));
    return due.length;
  };

  const plans: Plan[] = [];
  for (const s of seeded) {
    const { kind } = s.spec;
    const p: Plan = { s, git: !!s.spec.repo, messages: COMMITS[kind], build: BUILD_SECS[kind], liveAt: NOW - s.spec.deployedAgoH * HOUR, rows: [] };
    plans.push(p);
    // Seeded history is replaced; real deployments (with containers and the
    // traffic that references them) are never deleted.
    await sql`DELETE FROM deployments WHERE project_id = ${s.projectId} AND slug LIKE ${`${s.spec.name}-h%`}`;
    await sql`DELETE FROM environments WHERE project_id = ${s.projectId} AND is_preview`;
    if (p.git) await sql`UPDATE environments SET branch = 'main' WHERE id = ${s.envId}`;

    // The first real deployment opens the history, two weeks back.
    const [first] = await sql`SELECT id FROM deployments WHERE project_id = ${s.projectId} ORDER BY id LIMIT 1`;
    if (!first) throw new Error(`${s.spec.name} has no deployment: run the projects stage first`);
    const firstAt = new Date(p.liveAt - span - ri(2, 20) * HOUR);
    const firstStart = new Date(firstAt.getTime() - ri(p.build[0], p.build[1]) * 1000);
    await sql`UPDATE deployments SET created_at = ${firstStart}, started_at = ${firstStart}, deploying_at = ${firstStart},
                ready_at = ${firstAt}, finished_at = ${firstAt}, updated_at = ${firstAt} WHERE id = ${first.id}`;
    await dress(p, first.id, 0, p.git ? "chore: move hosting to Temps" : "", firstStart);

    // Production history between it and the release that is live now.
    const total = DEPLOY_COUNT[kind];
    const times: number[] = [];
    let at = p.liveAt;
    for (let n = total - 1; n >= 1; n--) times.unshift((at = workHour(at - (span / total) * (0.5 + rand()))));
    times.forEach((t, k) => {
      const n = k + 1;
      const failed = p.git && (n === total - 5 || n === total - 17);
      const cancelled = p.git && (n === total - 9 || n === total - 23);
      row(p, s.envId, Math.max(t, firstAt.getTime() + HOUR * n), failed ? "failed" : cancelled ? "cancelled" : "completed", {
        n, message: p.messages[n % p.messages.length],
        reason: failed ? FAILURES[n === total - 5 ? 0 : 1] : cancelled ? "Cancelled: superseded by a newer push to main" : undefined,
      });
    });

    // Work in progress: preview deploys of open branches over the last days
    // and, on the busiest service, a failed build of the next change after
    // the live release (production keeps serving it).
    let n = 900;
    if (kind === "shop") row(p, s.envId, p.liveAt + 70 * 60_000, "failed", { n: n++, message: p.messages[11], reason: FAILURES[0] });
    for (const [b, { branch, messages }] of (PREVIEWS[kind] ?? []).entries()) {
      const slug = branch.replace(/[^a-z0-9]+/g, "-");
      const [env] = await sql`INSERT INTO environments (name, slug, subdomain, host, upstreams, created_at, updated_at, project_id, branch, is_preview)
        VALUES (${branch}, ${slug}, ${`${s.spec.name}-${slug}`}, '', ${sql.json([])}, ${new Date(NOW - 4 * DAY)}, now(), ${s.projectId}, ${branch}, true)
        RETURNING id`;
      let t = NOW - (b === 0 ? ri(30, 60) : ri(80, 110)) * 60_000;
      const times: number[] = [];
      for (const _ of messages) times.unshift((t = workHour(t - (times.length ? ri(5, 22) * HOUR : 0))));
      times.forEach((tt, k) => row(p, env!.id, tt, "completed", { n: n++, branch, message: messages[k] }));
    }
    await flush(p, p.liveAt - p.build[1] * 1000);
  }

  // The release that is live now is a real deploy, made after the history so
  // its number is the highest, like on any instance that has shipped for a while.
  const live = await Promise.all(plans.map((p) =>
    api<{ id: number }>(`/projects/${p.s.projectId}/environments/${p.s.envId}/deploy/image`, {
      method: "POST", body: JSON.stringify({ image_ref: IMAGE, health_check_path: "/" }),
    })));
  for (const [k, p] of plans.entries()) {
    const id = live[k].id;
    for (let i = 0; ; i++) {
      const d = await api<{ status: string; cancelled_reason?: string }>(`/projects/${p.s.projectId}/deployments/${id}`);
      if (d.status === "completed") break;
      if (["failed", "cancelled"].includes(d.status) || i > 100) throw new Error(`deployment ${id} of ${p.s.spec.name}: ${d.status} ${d.cancelled_reason ?? ""}`);
      await Bun.sleep(3000);
    }
    const at = new Date(p.liveAt);
    const start = new Date(p.liveAt - ri(p.build[0], p.build[1]) * 1000);
    await sql`UPDATE deployments SET created_at = ${start}, started_at = ${start}, deploying_at = ${start},
                ready_at = ${at}, finished_at = ${at}, updated_at = ${at} WHERE id = ${id}`;
    await dress(p, id, DEPLOY_COUNT[p.s.spec.kind], p.messages[0] ?? "", start);
    await sql`UPDATE projects SET last_deployment = ${at} WHERE id = ${p.s.projectId}`;
    p.s.deploymentId = id;
    // Earlier runs' live releases would sit in the list twice: fold their
    // traffic into the first deployment, then remove them.
    const [first] = await sql`SELECT min(id) AS id FROM deployments WHERE project_id = ${p.s.projectId}`;
    const stale = (await sql`SELECT id FROM deployments WHERE project_id = ${p.s.projectId} AND id NOT IN (${first!.id}, ${id})
                               AND slug NOT LIKE ${`${p.s.spec.name}-h%`}`).map((r) => r.id as number);
    if (stale.length) {
      for (const table of ["events", "request_logs", "session_replay_sessions", "performance_metrics", "proxy_logs", "error_groups", "error_events"])
        await sql`UPDATE ${sql(table)} SET deployment_id = ${first!.id} WHERE deployment_id IN ${sql(stale)}`;
      await sql`DELETE FROM deployments WHERE id IN ${sql(stale)}`;
    }
  }

  for (const p of plans) {
    const later = await flush(p);
    const total = (await sql`SELECT count(*)::int AS c FROM deployments WHERE project_id = ${p.s.projectId}`)[0]!.c;
    console.log(`  ${p.s.spec.name}: ${total} deployments (${later} after the live #${p.s.deploymentId})`);
  }
}

// ── stage: traffic (geo, analytics, proxy logs) ───────────────────────────
async function seedGeo(): Promise<{ id: number; w: number; tz: string }[]> {
  const out: { id: number; w: number; tz: string }[] = [];
  let n = 0;
  for (const [city, region, country, cc, lat, lon, tz, eu, w] of CITIES) {
    // Documentation ranges (RFC 5737): look like addresses, identify nobody.
    n++;
    const ip = `${["192.0.2", "198.51.100", "203.0.113"][n % 3]}.${10 + n}`;
    const [row] = await sql`
      INSERT INTO ip_geolocations (ip_address, latitude, longitude, region, city, country, country_code, timezone, is_eu, created_at, updated_at)
      VALUES (${ip}, ${lat}, ${lon}, ${region}, ${city}, ${country}, ${cc}, ${tz}, ${eu}, now(), now())
      ON CONFLICT (ip_address) DO UPDATE SET city = EXCLUDED.city
      RETURNING id`;
    out.push({ id: row.id, w, tz });
  }
  return out;
}

/**
 * Share of daily traffic per UTC hour, per kind of service. Distinct shapes on
 * purpose: identical curves on every project card read as generated data.
 */
const HOURLY: Record<Kind, number[]> = {
  // consumers: EU lunch + a long US evening peak
  shop: [3.4, 2.9, 2.3, 1.9, 1.8, 2.0, 2.6, 3.3, 4.0, 4.4, 4.6, 4.9, 5.4, 5.1, 4.8, 4.9, 5.3, 5.8, 6.1, 6.4, 6.2, 5.6, 4.7, 3.9],
  // follows the shop, plus a nightly batch sync at 02:00 UTC
  api: [3.6, 3.1, 6.8, 2.2, 1.9, 2.1, 2.6, 3.2, 3.9, 4.3, 4.6, 4.8, 5.2, 5.0, 4.7, 4.9, 5.2, 5.6, 5.9, 6.2, 6.0, 5.4, 4.6, 3.9],
  // shop search-as-you-type, so it tracks the shop curve
  search: [3.3, 2.8, 2.2, 1.8, 1.7, 1.9, 2.5, 3.2, 4.0, 4.5, 4.7, 5.0, 5.5, 5.2, 4.9, 5.0, 5.4, 5.9, 6.2, 6.5, 6.3, 5.7, 4.8, 4.0],
  // order emails trail checkouts; shipping updates go out with the morning pick
  notify: [2.6, 2.2, 1.8, 1.5, 1.5, 2.0, 4.8, 6.9, 5.2, 4.6, 4.5, 4.7, 5.0, 4.9, 4.6, 4.7, 5.0, 5.4, 5.7, 5.9, 5.7, 5.1, 4.3, 3.5],
  // editorial: European morning readers, a smaller US bump
  marketing: [2.1, 1.7, 1.5, 1.4, 1.6, 2.4, 3.9, 5.6, 6.8, 7.1, 6.6, 5.9, 5.3, 5.0, 5.2, 5.4, 5.0, 4.6, 4.1, 3.7, 3.3, 3.0, 2.6, 2.3],
  // developers: two working days (EU, US) with a lunch dip
  docs: [1.2, 0.9, 0.8, 0.8, 0.9, 1.3, 2.5, 4.4, 6.2, 6.9, 6.6, 5.1, 5.6, 6.8, 7.4, 7.6, 7.1, 6.0, 4.3, 3.0, 2.3, 1.8, 1.5, 1.3],
  // one ops team in one timezone, 08:00–18:00
  admin: [0.3, 0.2, 0.2, 0.2, 0.3, 0.6, 2.2, 7.8, 10.6, 10.9, 10.1, 7.4, 8.8, 10.2, 9.9, 8.6, 6.1, 2.4, 0.9, 0.6, 0.5, 0.4, 0.4, 0.3],
  // people checking in on the shop: follows it, flatter
  status: [3.6, 3.2, 2.9, 2.7, 2.7, 2.9, 3.4, 4.0, 4.5, 4.8, 4.9, 5.0, 5.2, 5.0, 4.8, 4.9, 5.1, 5.4, 5.6, 5.7, 5.5, 5.0, 4.4, 3.9],
  // called by the storefront on every product and home page view
  reco: [3.4, 2.9, 2.3, 1.9, 1.8, 2.0, 2.6, 3.3, 4.0, 4.4, 4.6, 4.9, 5.4, 5.1, 4.8, 4.9, 5.3, 5.8, 6.1, 6.4, 6.2, 5.6, 4.7, 3.9],
  // customers after work and at the weekend-ish evening peak
  returns: [1.8, 1.3, 1.0, 0.9, 0.9, 1.2, 2.2, 3.6, 4.6, 5.0, 5.1, 5.2, 5.6, 5.3, 5.1, 5.3, 5.8, 6.4, 7.0, 7.3, 6.9, 5.8, 4.2, 2.8],
  // cron-shaped: invoice runs at 00:00 and 06:00, renewals at 12:00
  worker: [14.0, 2.0, 1.6, 1.5, 1.5, 1.6, 11.0, 2.2, 2.0, 2.0, 1.9, 2.0, 9.5, 2.1, 2.0, 2.0, 1.9, 2.0, 4.2, 2.0, 1.9, 1.9, 1.8, 1.8],
};
const hourShare = (kind: Kind, h: number) => HOURLY[kind][h] / HOURLY[kind].reduce((a, b) => a + b, 0);

function dayFactor(kind: Kind, daysAgo: number): number {
  const d = new Date(NOW - daysAgo * DAY).getUTCDay();
  const weekend = d === 0 || d === 6;
  const week = kind === "docs" || kind === "admin" ? (weekend ? 0.45 : 1.08) : weekend ? 1.12 : 0.97;
  const growth = 0.84 + 0.16 * (1 - daysAgo / DAYS); // ~+19% over the window
  const launch =
    (kind === "marketing" || kind === "shop") && daysAgo === LAUNCH_DAY_AGO ? 2.4 : daysAgo === LAUNCH_DAY_AGO - 1 ? 1.35 : 1;
  return week * growth * launch * (0.93 + rand() * 0.14);
}

async function insertBatched(table: string, rows: Record<string, unknown>[], cols: string[]) {
  const per = Math.floor(60000 / cols.length);
  for (let i = 0; i < rows.length; i += per) {
    await sql`INSERT INTO ${sql(table)} ${sql(rows.slice(i, i + per) as any, ...cols)}`;
  }
}

/**
 * `top-up` mode: history is relative to the run, so a world seeded yesterday
 * ends yesterday and every chart drops to nothing at its right edge. Topping
 * up extends each already-seeded series from its newest row to now, drawing
 * from the same generator, instead of skipping it.
 */
async function seedAnalytics(seeded: Seeded[], geos: { id: number; w: number }[], topUp = false) {
  const geoPick = () => weighted(geos.map((g) => [g.id, g.w] as const));
  for (const s of seeded) {
    if (!s.spec.visitors) continue;
    // Visitor and session keys carry the service id: the generator restarts
    // on every run and skips seeded services, so a service added later would
    // otherwise draw the keys an earlier one already used.
    const pages = PAGES[s.spec.kind];
    const already = await sql`SELECT 1 FROM events WHERE project_id = ${s.projectId} AND timestamp < now() - interval '1 day' LIMIT 1`;
    if (already.length && !topUp) {
      console.log(`  ${s.spec.name}: analytics already seeded, skipping`);
      continue;
    }
    if (!already.length && topUp) {
      console.log(`  ${s.spec.name}: no analytics to top up, run the traffic stage first`);
      continue;
    }
    const [{ last }] = topUp
      ? await sql`SELECT max(timestamp) AS last FROM events WHERE project_id = ${s.projectId}`
      : [{ last: null }];
    const fromAt = last ? new Date(last).getTime() : -Infinity;
    // The generator replays from its fixed seed, so a top-up draws the keys
    // the first run already used; a per-run salt keeps them unique.
    const salt = topUp ? `${Date.now().toString(36)}-` : "";

    interface V { key: string; first: number; last: number; geo: number; browser: (typeof BROWSERS)[number]; os: (typeof OSES)[number]; device: string; lang: string; src: (typeof SOURCES)[number] }
    const visitors: V[] = [];
    const sessions: { v: number; at: number; src: (typeof SOURCES)[number]; sid: string }[] = [];
    const returningShare = s.spec.kind === "admin" ? 0.85 : s.spec.kind === "docs" ? 0.42 : 0.31;

    for (let daysAgo = DAYS; daysAgo >= 0; daysAgo--) {
      const daily = s.spec.visitors * dayFactor(s.spec.kind, daysAgo);
      for (let h = 0; h < 24; h++) {
        const hourStart = NOW - daysAgo * DAY - (NOW % DAY) + h * HOUR;
        if (hourStart > NOW) break;
        const count = Math.round(daily * hourShare(s.spec.kind, h) * (0.8 + rand() * 0.45));
        for (let i = 0; i < count; i++) {
          const at = hourStart + Math.floor(rand() * HOUR);
          if (at > NOW || at <= fromAt) continue;
          let vi: number;
          if (visitors.length > 50 && rand() < returningShare) {
            vi = visitors.length - 1 - Math.floor(Math.pow(rand(), 2) * visitors.length); // recent visitors return more
          } else {
            const os = weighted(OSES.map((o) => [o, o[2]] as const));
            const mobile = os[0] === "iOS" || os[0] === "Android";
            const browser = os[0] === "iOS" ? BROWSERS[1] : weighted(BROWSERS.map((b) => [b, b[2]] as const));
            visitors.push({
              key: `sv-${s.projectId}-${salt}${nanoid(14)}`, first: at, last: at, geo: geoPick(), browser, os,
              device: mobile ? (rand() < 0.12 ? "tablet" : "mobile") : "desktop",
              lang: weighted(LANGS.map((l) => [l[0], l[1]] as const)),
              src: weighted(SOURCES.map((x) => [x, x[2]] as const)),
            });
            vi = visitors.length - 1;
          }
          const v = visitors[vi];
          v.last = Math.max(v.last, at);
          const src = daysAgo === LAUNCH_DAY_AGO && rand() < 0.55 ? SOURCES[6] : sessions.length && rand() < 0.4 ? SOURCES[0] : v.src;
          sessions.push({ v: vi, at, src, sid: `ss-${s.projectId}-${salt}${nanoid(18)}` });
        }
      }
    }

    // visitors
    if (!visitors.length) {
      console.log(`  ${s.spec.name}: analytics already up to date`);
      continue;
    }
    const vRows = visitors.map((v) => ({
      visitor_id: v.key, first_seen: new Date(v.first), last_seen: new Date(v.last), user_agent: null,
      ip_address_id: v.geo, is_crawler: false, project_id: s.projectId, environment_id: s.envId, has_activity: true,
      first_referrer: v.src[0], first_referrer_hostname: v.src[0] ? new URL(v.src[0]).hostname : null, first_channel: v.src[1],
    }));
    const vIds: number[] = [];
    const vCols = Object.keys(vRows[0]);
    const per = Math.floor(60000 / vCols.length);
    for (let i = 0; i < vRows.length; i += per) {
      const res = await sql`INSERT INTO visitor ${sql(vRows.slice(i, i + per) as any, ...vCols)} RETURNING id`;
      vIds.push(...res.map((r: any) => r.id));
    }

    // sessions
    await insertBatched(
      "request_sessions",
      sessions.map((x) => ({
        session_id: x.sid, started_at: new Date(x.at), last_accessed_at: new Date(Math.min(x.at + ri(20, 900) * 1000, NOW)),
        ip_address: null, user_agent: null, referrer: x.src[0], data: "{}", visitor_id: vIds[x.v],
        channel: x.src[1], referrer_hostname: x.src[0] ? new URL(x.src[0]).hostname : null,
      })),
      ["session_id", "started_at", "last_accessed_at", "ip_address", "user_agent", "referrer", "data", "visitor_id", "channel", "referrer_hostname"],
    );

    // events
    const ev: Record<string, unknown>[] = [];
    const cols = ["project_id", "environment_id", "deployment_id", "timestamp", "session_id", "visitor_id", "hostname", "pathname", "page_path", "href", "page_title", "referrer", "referrer_hostname", "is_entry", "is_exit", "is_bounce", "time_on_page", "session_page_number", "scroll_depth", "browser", "browser_version", "operating_system", "operating_system_version", "device_type", "screen_width", "screen_height", "ip_geolocation_id", "channel", "event_type", "is_crawler", "language", "lcp", "fcp", "ttfb", "cls", "inp"];
    let total = 0;
    const flush = async () => {
      if (ev.length) { await insertBatched("events", ev.splice(0), cols); }
    };
    for (const x of sessions) {
      const v = visitors[x.v];
      const depth = weighted([[1, 38], [2, 20], [3, 15], [4, 10], [5, 7], [6, 4], [8, 4], [11, 2]] as const);
      let t = x.at;
      const mobile = v.device !== "desktop";
      const [sw, sh] = mobile ? pick([[390, 844], [393, 852], [412, 915]] as const) : v.device === "tablet" ? [820, 1180] : pick([[1440, 900], [1920, 1080], [1536, 864], [2560, 1440]] as const);
      for (let p = 0; p < depth; p++) {
        // A session still going at seed time ends at now, like a visitor who
        // is still on the site: no page views from the future.
        if (t > NOW) break;
        const [path, title] = p === 0 && x.src[1] === "organic_search" && rand() < 0.4 ? pick(pages.slice(1)) : weighted(pages.map((pg) => [pg, pg[2]] as const));
        const dwell = ri(6, depth === 1 ? 70 : 150);
        const href = `https://${s.spec.host}${path}`;
        const base = {
          project_id: s.projectId, environment_id: s.envId, deployment_id: s.deploymentId, session_id: x.sid, visitor_id: vIds[x.v],
          hostname: s.spec.host, pathname: path, page_path: path, href, page_title: title,
          session_page_number: p + 1, browser: v.browser[0], browser_version: v.browser[1][0], operating_system: v.os[0],
          operating_system_version: v.os[1][v.os[1].length - 1], device_type: v.device, screen_width: sw, screen_height: sh,
          ip_geolocation_id: v.geo, channel: x.src[1], is_crawler: false, language: v.lang,
        };
        ev.push({
          ...base, timestamp: new Date(t), referrer: p === 0 ? x.src[0] : null,
          referrer_hostname: p === 0 && x.src[0] ? new URL(x.src[0]).hostname : null,
          is_entry: p === 0, is_exit: p === depth - 1, is_bounce: depth === 1, time_on_page: dwell, scroll_depth: ri(25, 100),
          event_type: "page_view",
          lcp: latency(mobile ? 1850 : 1150, 0.35), fcp: latency(mobile ? 1050 : 620, 0.3), ttfb: latency(mobile ? 240 : 140, 0.4),
          cls: Math.round(rand() * rand() * 0.18 * 1000) / 1000, inp: latency(mobile ? 140 : 85, 0.4),
        });
        if (t + dwell * 1000 <= NOW) ev.push({
          ...base, timestamp: new Date(t + dwell * 1000), referrer: null, referrer_hostname: null,
          is_entry: false, is_exit: false, is_bounce: false, time_on_page: dwell, scroll_depth: ri(40, 100),
          event_type: "page_leave", lcp: null, fcp: null, ttfb: null, cls: null, inp: null,
        });
        t += (dwell + ri(1, 8)) * 1000;
        total += 2;
        if (ev.length >= 20000) await flush();
      }
    }
    await flush();
    console.log(`  ${s.spec.name}: ${visitors.length} visitors, ${sessions.length} sessions, ${total} events`);
  }
}

async function seedProxyLogs(seeded: Seeded[], geos: { id: number; w: number }[], topUp = false) {
  const geoPick = () => weighted(geos.map((g) => [g.id, g.w] as const));
  const cols = ["timestamp", "method", "path", "host", "status_code", "response_time_ms", "request_source", "is_system_request", "routing_status", "project_id", "environment_id", "deployment_id", "upstream_host", "client_ip", "user_agent", "referrer", "request_id", "ip_geolocation_id", "browser", "operating_system", "device_type", "is_bot", "bot_name", "request_size_bytes", "response_size_bytes", "cache_status", "created_date", "error_message"];
  for (const s of seeded) {
    // Live traffic (health checks, the proxy) writes rows from the first minute,
    // so only history older than a day means this stage already ran.
    const already = await sql`SELECT 1 FROM proxy_logs WHERE project_id = ${s.projectId} AND timestamp < now() - interval '1 day' LIMIT 1`;
    if (already.length && !topUp) { console.log(`  ${s.spec.name}: proxy logs already seeded, skipping`); continue; }
    if (!already.length && topUp) { console.log(`  ${s.spec.name}: no proxy logs to top up, run the traffic stage first`); continue; }
    // Seeded traffic is the only `user`/`bot` traffic: health checks and the
    // monitor keep writing their own rows, which must not move the cursor.
    const [{ last }] = topUp
      ? await sql`SELECT max(timestamp) AS last FROM proxy_logs WHERE project_id = ${s.projectId} AND request_source IN ('user', 'bot')`
      : [{ last: null }];
    const fromAt = last ? new Date(last).getTime() : -Infinity;
    const rows: Record<string, unknown>[] = [];
    const pages = PAGES[s.spec.kind];
    const routes = API_ROUTES[s.spec.kind];
    let n = 0;
    for (let daysAgo = PROXY_DAYS; daysAgo >= 0; daysAgo--) {
      const daily = s.spec.requests * dayFactor(s.spec.kind, daysAgo);
      for (let h = 0; h < 24; h++) {
        const hourStart = NOW - daysAgo * DAY - (NOW % DAY) + h * HOUR;
        if (hourStart > NOW) break;
        const count = Math.round(daily * hourShare(s.spec.kind, h) * (0.85 + rand() * 0.3));
        for (let i = 0; i < count; i++) {
          const ts = hourStart + Math.floor(rand() * HOUR);
          if (ts > NOW || ts <= fromAt) continue;
          const isBot = s.spec.kind !== "worker" && s.spec.kind !== "api" && rand() < 0.06;
          const roll = rand();
          let method = "GET", path: string, med: number, size: number, cache: string | null = null;
          if (routes.length && (pages.length === 0 || roll < 0.35)) {
            const r = weighted(routes.map((x) => [x, x[2]] as const));
            [method, path, , med] = r; size = ri(300, 9000);
          } else if (pages.length && roll < 0.75) {
            path = weighted(pages.map((p) => [p[0], p[2]] as const)).replace("[id]", String(ri(48000, 52000))); med = 46; size = ri(18000, 90000);
          } else {
            path = pick(STATIC); med = 4; size = ri(2000, 240000); cache = rand() < 0.86 ? "HIT" : "MISS";
          }
          const inIncident = ts >= INCIDENT.start && ts < INCIDENT.start + INCIDENT.minutes * 60_000;
          // Storefront clients retry a stalled charge, so payment calls pile up during the incident.
          if (s.spec.kind === "api" && inIncident && rand() < 0.45) { method = "POST"; path = "/v1/payments"; med = 340; }
          let status: number = weighted([[200, 930], [304, 30], [301, 8], [404, 18], [401, 6], [429, 2], [500, 3]] as const);
          if (method === "POST" && status === 200) status = path.includes("items") ? 201 : 200;
          let rt = latency(med);
          let err: string | null = null;
          if (s.spec.kind === "api" && inIncident && (path === "/v1/payments" || path === "/v1/checkout/sessions") && rand() < 0.96) {
            status = rand() < 0.62 ? 504 : 502; rt = status === 504 ? ri(9800, 10200) : ri(2100, 4800);
            err = status === 504 ? "upstream timed out after 10000ms" : "upstream reset the connection";
          }
          const browser = isBot ? null : pick(["Chrome", "Chrome", "Safari", "Firefox", "Edge"]);
          rows.push({
            timestamp: new Date(ts), method, path, host: s.spec.host, status_code: status, response_time_ms: rt,
            request_source: isBot ? "bot" : "user", is_system_request: false, routing_status: status >= 500 ? "error" : "success",
            project_id: s.projectId, environment_id: s.envId, deployment_id: s.deploymentId,
            upstream_host: `${s.spec.name}-production:${IMAGE_PORT}`,
            client_ip: `${pick(["192.0.2", "198.51.100", "203.0.113"])}.${ri(1, 254)}`,
            user_agent: isBot ? "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)" : `Mozilla/5.0 ${browser}`,
            referrer: null, request_id: uuid(), ip_geolocation_id: geoPick(), browser,
            operating_system: isBot ? null : pick(["macOS", "Windows", "iOS", "Android"]),
            device_type: isBot ? null : pick(["desktop", "desktop", "mobile"]),
            is_bot: isBot, bot_name: isBot ? "Googlebot" : null,
            request_size_bytes: method === "GET" ? ri(200, 900) : ri(400, 4000), response_size_bytes: size, cache_status: cache,
            created_date: new Date(ts).toISOString().slice(0, 10), error_message: err,
          });
          n++;
          if (rows.length >= 20000) await insertBatched("proxy_logs", rows.splice(0), cols);
        }
      }
    }
    await insertBatched("proxy_logs", rows.splice(0), cols);
    console.log(`  ${s.spec.name}: ${n} proxy log rows`);
  }
}

async function refreshAggregates() {
  for (const [view, end] of [["proxy_logs_stats_1m", "1 minute"], ["events_hourly", "1 hour"]] as const) {
    await sql
      .unsafe(`CALL refresh_continuous_aggregate('${view}', NULL, now() - interval '${end}')`)
      .then(() => console.log(`  refreshed ${view}`))
      .catch((e) => console.log(`  ${view}: not refreshed (${String(e.message).slice(0, 80)})`));
  }
}

// ── stage: uptime ─────────────────────────────────────────────────────────
async function seedUptime(seeded: Seeded[]) {
  console.log("\n▸ uptime");
  for (const s of seeded) {
    const monitors = await sql`SELECT id, check_interval_seconds FROM status_monitors WHERE project_id = ${s.projectId} AND environment_id = ${s.envId}`;
    for (const m of monitors) {
      const [{ first }] = await sql`SELECT min(checked_at) AS first FROM status_checks WHERE monitor_id = ${m.id}`;
      const end = first ? new Date(first).getTime() : NOW;
      const start = NOW - DAYS * DAY;
      if (end - start < DAY) { console.log(`  ${s.spec.name}: uptime already seeded, skipping`); continue; }
      const step = (m.check_interval_seconds || 60) * 1000;
      const rows: Record<string, unknown>[] = [];
      for (let t = start; t < end; t += step) {
        let status = "operational", ms = latency(s.spec.checkMs, 0.3), err: string | null = null;
        if (s.spec.kind === "api" && t >= INCIDENT.start && t < INCIDENT.start + INCIDENT.minutes * 60_000) {
          status = "major_outage"; ms = 10000; err = "GET / timed out after 10s";
        } else if (s.spec.kind === "marketing" && t >= NOW - 11 * DAY - 3 * HOUR && t < NOW - 11 * DAY - 3 * HOUR + 26 * 60_000) {
          status = "degraded"; ms = latency(1900, 0.2);
        }
        rows.push({ monitor_id: m.id, status, response_time_ms: ms, checked_at: new Date(t + ri(0, 900)), error_message: err, created_at: new Date(t) });
      }
      await insertBatched("status_checks", rows, ["monitor_id", "status", "response_time_ms", "checked_at", "error_message", "created_at"]);
      await sql`UPDATE status_monitors SET created_at = ${new Date(start)} WHERE id = ${m.id}`;
      console.log(`  ${s.spec.name}: ${rows.length} checks`);
    }
  }
}

// ── stage: errors ─────────────────────────────────────────────────────────
type Frame = { filename: string; function: string; lineno: number; in_app?: boolean; colno?: number };
interface ErrorSpec {
  project: string;
  platform: "javascript" | "go" | "python" | "php";
  type: string;
  value: string;
  /** Oldest call first, crashing frame last (Sentry order). */
  frames: Frame[];
  release: string;
  url?: string;
  /** Event timestamps, computed up front so the shape is deliberate. */
  times: () => number[];
  status?: "resolved" | "ignored";
  browsers?: readonly string[];
  /** Still happening: `top-up` sends it a few events at run time, so the
   *  list has errors seen minutes ago and not only at seed time. */
  ongoing?: boolean;
}

const spread = (n: number, fromAgo: number, toAgo: number) =>
  Array.from({ length: n }, () => NOW - toAgo - rand() * (fromAgo - toAgo));
const incident = (n: number) =>
  Array.from({ length: n }, () => INCIDENT.start + Math.pow(rand(), 0.7) * INCIDENT.minutes * 60_000);
const weekdays = (perDay: number, days: number) =>
  Array.from({ length: days }, (_, d) => d).flatMap((d) => {
    const day = new Date(NOW - d * DAY).getUTCDay();
    if (day === 0 || day === 6) return [];
    return Array.from({ length: ri(Math.max(0, perDay - 2), perDay + 2) }, () => NOW - d * DAY - ri(1, 9) * HOUR - rand() * HOUR);
  }).filter((t) => t < NOW);

const JS_VENDOR: Frame[] = [
  { filename: "node_modules/react-dom/cjs/react-dom-client.production.js", function: "performWorkOnRoot", lineno: 15876, in_app: false },
  { filename: "node_modules/react-dom/cjs/react-dom-client.production.js", function: "renderWithHooks", lineno: 5214, in_app: false },
];

const ERRORS: ErrorSpec[] = [
  {
    project: "checkout-api", platform: "go", release: "checkout-api@1.42.0",
    type: "*payments.ProviderTimeoutError", value: "payment provider did not respond within 10s (POST /v1/charges)",
    url: "https://api.example.com/v1/payments",
    frames: [
      { filename: "net/http/server.go", function: "(*conn).serve", lineno: 2092, in_app: false },
      { filename: "internal/httpapi/router.go", function: "(*Router).ServeHTTP", lineno: 77, in_app: true },
      { filename: "internal/checkout/handler.go", function: "(*Handler).ConfirmPayment", lineno: 142, in_app: true },
      { filename: "internal/payments/service.go", function: "(*Service).Capture", lineno: 96, in_app: true },
      { filename: "internal/payments/client.go", function: "(*Client).Charge", lineno: 211, in_app: true },
    ],
    times: () => [...incident(186), ...spread(9, 27 * DAY, 4 * DAY)],
  },
  {
    project: "checkout-api", platform: "go", release: "checkout-api@1.42.0",
    type: "context.deadlineExceededError", value: "inventory: reserve sku=ASJ-M-NAVY: context deadline exceeded",
    url: "https://api.example.com/v1/checkout/sessions",
    frames: [
      { filename: "internal/httpapi/router.go", function: "(*Router).ServeHTTP", lineno: 77, in_app: true },
      { filename: "internal/checkout/handler.go", function: "(*Handler).CreateSession", lineno: 64, in_app: true },
      { filename: "internal/inventory/client.go", function: "(*Client).Reserve", lineno: 58, in_app: true },
    ],
    times: () => [...incident(31), ...spread(6, 20 * DAY, 2 * DAY)],
  },
  {
    project: "checkout-api", platform: "go", release: "checkout-api@1.41.3",
    type: "*pgconn.PgError", value: 'duplicate key value violates unique constraint "orders_idempotency_key_key" (SQLSTATE 23505)',
    url: "https://api.example.com/v1/payments",
    frames: [
      { filename: "internal/checkout/handler.go", function: "(*Handler).ConfirmPayment", lineno: 156, in_app: true },
      { filename: "internal/orders/repository.go", function: "(*Repository).Create", lineno: 118, in_app: true },
    ],
    times: () => spread(14, 11 * DAY, 6 * HOUR),
  },
  {
    project: "storefront", platform: "javascript", release: "storefront@2026.10.28-1",
    ongoing: true,
    type: "ChunkLoadError", value: "Loading chunk 8133 failed. (error: https://shop.example.com/_next/static/chunks/8133-4c1f0d7e9b2a.js)",
    url: "https://shop.example.com/products/alpine-shell-jacket",
    frames: [
      { filename: "app:///_next/static/chunks/webpack-6a1f1c.js", function: "__webpack_require__.f.j", lineno: 1, colno: 3604, in_app: true },
      { filename: "app:///_next/static/chunks/webpack-6a1f1c.js", function: "__webpack_require__.e", lineno: 1, colno: 1210, in_app: true },
    ],
    times: () => spread(41, 3 * HOUR - 4 * 60_000, 60_000),
    browsers: ["Chrome", "Safari", "Chrome", "Edge"],
  },
  {
    project: "storefront", platform: "javascript", release: "storefront@2026.10.24-2",
    type: "TypeError", value: "undefined is not an object (evaluating 'product.variants.find')",
    url: "https://shop.example.com/products/trail-runner-pack-22l",
    frames: [
      ...JS_VENDOR,
      { filename: "app:///app/products/[slug]/variant-picker.tsx", function: "VariantPicker", lineno: 38, colno: 27, in_app: true },
      { filename: "app:///app/products/[slug]/use-selected-variant.ts", function: "useSelectedVariant", lineno: 14, colno: 31, in_app: true },
    ],
    times: () => spread(57, 4 * DAY, 3 * HOUR + 5 * 60_000),
    browsers: ["Mobile Safari", "Safari"],
  },
  {
    project: "storefront", platform: "javascript", release: "storefront@2026.10.28-1",
    type: "AbortError", value: "The user aborted a request.",
    url: "https://shop.example.com/search",
    frames: [
      { filename: "app:///app/search/use-search.ts", function: "runSearch", lineno: 52, colno: 17, in_app: true },
    ],
    times: () => spread(176, 30 * DAY, 10 * 60_000),
    status: "ignored",
    browsers: ["Chrome", "Safari", "Firefox"],
  },
  {
    project: "admin-dashboard", platform: "javascript", release: "admin-dashboard@3.18.0",
    type: "RangeError", value: "Invalid time value",
    url: "https://admin.example.com/orders/50412",
    frames: [
      ...JS_VENDOR,
      { filename: "app:///src/features/orders/OrderTimeline.tsx", function: "OrderTimeline", lineno: 61, colno: 19, in_app: true },
      { filename: "app:///src/lib/format-date.ts", function: "formatOrderDate", lineno: 9, colno: 12, in_app: true },
    ],
    times: () => weekdays(4, 9).filter((t) => t > NOW - 213 * HOUR),
    browsers: ["Chrome", "Edge"],
  },
  {
    project: "marketing-site", platform: "javascript", release: "marketing-site@1.9.2",
    type: "Error", value: "Hydration failed because the server rendered text didn't match the client.",
    url: "https://www.example.com/stores",
    frames: [
      ...JS_VENDOR,
      { filename: "app:///src/components/StoreHours.tsx", function: "StoreHours", lineno: 23, colno: 9, in_app: true },
    ],
    times: () => spread(33, 3 * DAY, 2 * HOUR),
    browsers: ["Chrome", "Safari", "Firefox"],
  },
  {
    project: "billing-worker", platform: "python", release: "billing-worker@0.31.1",
    type: "KeyError", value: "'vat_id'",
    frames: [
      { filename: "billing/worker.py", function: "run_job", lineno: 88, in_app: true },
      { filename: "billing/invoices.py", function: "render_invoice", lineno: 142, in_app: true },
      { filename: "billing/templates/eu.py", function: "tax_block", lineno: 31, in_app: true },
    ],
    times: () => spread(6, 13 * DAY, 12 * DAY),
    status: "resolved",
  },
  {
    project: "recommendations", platform: "python", release: "recommendations@2.4.0",
    type: "ValueError", value: "embedding dimension mismatch: expected 384, got 768",
    url: "https://reco.internal.example.com/v1/recommendations/product",
    frames: [
      { filename: "app/main.py", function: "product_recommendations", lineno: 64, in_app: true },
      { filename: "app/ranking.py", function: "rank_candidates", lineno: 118, in_app: true },
      { filename: "app/vectors.py", function: "cosine_scores", lineno: 41, in_app: true },
    ],
    // Fixed this afternoon: the last event is recent, and it has not recurred.
    times: () => spread(23, 26 * HOUR, 90 * 60_000),
    status: "resolved",
  },
  {
    project: "returns-portal", platform: "php", release: "returns-portal@1.9.2",
    type: "Illuminate\\Database\\QueryException", value: "SQLSTATE[23000]: Integrity constraint violation: 1062 Duplicate entry for key 'returns_order_line_unique'",
    url: "https://returns.example.com/api/returns",
    frames: [
      { filename: "vendor/laravel/framework/src/Illuminate/Database/Connection.php", function: "runQueryCallback", lineno: 829, in_app: false },
      { filename: "app/Http/Controllers/ReturnController.php", function: "store", lineno: 57, in_app: true },
      { filename: "app/Services/ReturnService.php", function: "createForLine", lineno: 33, in_app: true },
    ],
    times: () => weekdays(2, 20),
  },
  // ── a wider spread across services and statuses, for the cross-service list
  {
    project: "storefront", platform: "javascript", release: "storefront@2026.10.21-1",
    type: "ReferenceError", value: "analytics is not defined",
    url: "https://shop.example.com/products/merino-crew",
    frames: [
      { filename: "app:///_next/static/chunks/pages/_app-3f9c2a.js", function: "trackView", lineno: 1, colno: 48211, in_app: true },
    ],
    browsers: ["Chrome", "Safari", "Firefox"],
    times: () => spread(64, 16 * DAY, 8 * DAY),
    status: "resolved",
  },
  {
    project: "storefront", platform: "javascript", release: "storefront@2026.10.28-1",
    type: "SecurityError", value: "The operation is insecure. (localStorage in a private window)",
    url: "https://shop.example.com/cart",
    frames: [
      { filename: "app:///_next/static/chunks/cart-91d0b7.js", function: "loadSavedCart", lineno: 1, colno: 2210, in_app: true },
    ],
    browsers: ["Mobile Safari"],
    times: () => spread(38, 25 * DAY, 30 * 60_000),
    status: "ignored",
  },
  {
    project: "notifications", platform: "javascript", release: "notifications@4.7.1",
    ongoing: true,
    type: "TypeError", value: "Cannot read properties of undefined (reading 'email')",
    frames: [
      { filename: "/app/src/queue/worker.js", function: "processJob", lineno: 88, in_app: true },
      { filename: "/app/src/templates/order-shipped.js", function: "render", lineno: 23, in_app: true },
    ],
    times: () => spread(19, 2 * DAY, 25 * 60_000),
  },
  {
    project: "notifications", platform: "javascript", release: "notifications@4.6.0",
    type: "SMTPError", value: "421 4.7.0 Try again later, closing connection",
    frames: [
      { filename: "/app/src/mail/transport.js", function: "send", lineno: 141, in_app: true },
    ],
    times: () => spread(52, 18 * DAY, 9 * DAY),
    status: "resolved",
  },
  {
    project: "docs", platform: "javascript", release: "docs@2026.10.19",
    type: "Error", value: "Failed to fetch the search index (HTTP 404 /search/index.json)",
    url: "https://docs.example.com/guides/returns",
    frames: [
      { filename: "app:///_astro/search.4b1e2c.js", function: "loadIndex", lineno: 1, colno: 913, in_app: true },
    ],
    browsers: ["Chrome", "Firefox"],
    times: () => spread(27, 10 * DAY, 7 * DAY),
    status: "resolved",
  },
  {
    project: "admin-dashboard", platform: "javascript", release: "admin-dashboard@3.18.0",
    type: "Error", value: "ResizeObserver loop completed with undelivered notifications.",
    frames: [
      { filename: "app:///assets/index-7d21aa.js", function: "observe", lineno: 1, colno: 77102, in_app: false },
    ],
    browsers: ["Chrome"],
    times: () => spread(91, 28 * DAY, 40 * 60_000),
    status: "ignored",
  },
  {
    project: "billing-worker", platform: "python", release: "billing-worker@0.32.0",
    type: "RateLimitError", value: "billing provider returned 429 Too Many Requests (retry after 30s)",
    frames: [
      { filename: "billing/jobs/invoices.py", function: "run_monthly_invoices", lineno: 57, in_app: true },
      { filename: "billing/provider.py", function: "create_invoice", lineno: 122, in_app: true },
    ],
    times: () => spread(12, 26 * DAY, 20 * DAY),
    status: "ignored",
  },
  {
    project: "recommendations", platform: "python", release: "recommendations@2.4.1",
    ongoing: true,
    type: "TimeoutError", value: "feature store read timed out after 2.0s (segment=returning_buyers)",
    frames: [
      { filename: "reco/api/routes.py", function: "recommend", lineno: 44, in_app: true },
      { filename: "reco/features/store.py", function: "fetch", lineno: 131, in_app: true },
    ],
    times: () => spread(17, 30 * HOUR, 50 * 60_000),
  },
  {
    project: "returns-portal", platform: "php", release: "returns-portal@1.9.0",
    type: "ErrorException", value: 'Undefined array key "refund_method"',
    url: "https://returns.example.com/returns/create",
    frames: [
      { filename: "/var/www/app/Http/Controllers/ReturnController.php", function: "store", lineno: 73, in_app: true },
    ],
    times: () => spread(29, 15 * DAY, 11 * DAY),
    status: "resolved",
  },
  {
    project: "status-page", platform: "javascript", release: "status-page@0.14.2",
    type: "Error", value: "Incident feed returned 502 Bad Gateway",
    url: "https://status.example.com/",
    frames: [
      { filename: "app:///_app/immutable/nodes/0.1c9e2f.js", function: "load", lineno: 1, colno: 3311, in_app: true },
    ],
    browsers: ["Chrome", "Safari"],
    times: () => spread(9, 25 * HOUR, 40 * 60_000),
    status: "resolved",
  },
  {
    project: "marketing-site", platform: "javascript", release: "marketing-site@1.9.2",
    type: "TypeError", value: "Failed to fetch (newsletter signup)",
    url: "https://www.example.com/",
    frames: [
      { filename: "app:///_astro/Newsletter.8c2d1a.js", function: "subscribe", lineno: 1, colno: 640, in_app: true },
    ],
    browsers: ["Safari", "Chrome"],
    times: () => spread(14, 5 * DAY, 3 * HOUR),
  },
];

type PlannedEvent = { id: string; at: number; visitor: number | null };

async function seedErrors(seeded: Seeded[]) {
  console.log("\n▸ errors");
  await login();
  const plan: PlannedEvent[] = [];
  for (const e of ERRORS) {
    const s = seeded.find((x) => x.spec.name === e.project);
    if (!s) continue;
    // Several services raise a plain `Error`, so the type alone does not
    // identify a seeded group: match its message too.
    const already = await sql`SELECT 1 FROM error_groups WHERE project_id = ${s.projectId} AND error_type = ${e.type} AND position(${e.value.slice(0, 40)} in title) > 0 LIMIT 1`;
    if (already.length) { console.log(`  ${e.project}: ${e.type} already seeded, skipping`); continue; }
    await sendErrorEvents(s, e, e.times(), plan);
  }
  if (!plan.length) return;
  await placeErrorEvents(plan);
  for (const e of ERRORS.filter((x) => x.status)) {
    const s = seeded.find((x) => x.spec.name === e.project);
    if (!s) continue;
    await sql`UPDATE error_groups SET status = ${e.status!}
      WHERE project_id = ${s.projectId} AND error_type = ${e.type} AND position(${e.value.slice(0, 40)} in title) > 0`;
  }
  await refreshErrorAggregate();
}

/** `top-up`: a few fresh events for each ongoing error, inside the last 40 minutes. */
async function topUpErrors(seeded: Seeded[]) {
  await login();
  const plan: PlannedEvent[] = [];
  for (const e of ERRORS.filter((x) => x.ongoing)) {
    const s = seeded.find((x) => x.spec.name === e.project);
    if (!s) continue;
    await sendErrorEvents(s, e, spread(ri(1, 3), 40 * 60_000, 2 * 60_000), plan);
  }
  if (!plan.length) return;
  await placeErrorEvents(plan);
  await refreshErrorAggregate();
}

async function sendErrorEvents(s: Seeded, e: ErrorSpec, eventTimes: number[], plan: PlannedEvent[]) {
  const dsn = await api<{ public_key: string }>(`/projects/${s.projectId}/dsns/get-or-create`, {
    method: "POST", body: JSON.stringify({ environment_id: s.envId }),
  });
  const times = eventTimes.sort((a, b) => a - b);
  // Affected users are distinct visitors. Browser errors happen to the
  // service's own visitors, a smaller pool than the events so some people
  // hit the same error twice; server-side errors have no visitor to link.
  const pool: number[] =
    e.platform === "javascript" || e.platform === "php"
      ? (await sql`SELECT id FROM visitor WHERE project_id = ${s.projectId} ORDER BY random() LIMIT ${Math.ceil(times.length * 0.7)}`).map((r: { id: number }) => r.id)
      : [];
  for (const at of times) {
    const id = uuid().replace(/-/g, "");
    const browser = e.browsers ? pick(e.browsers) : null;
    const body: Record<string, unknown> = {
      event_id: id, platform: e.platform, level: "error", environment: "production", release: e.release,
      timestamp: at / 1000, server_name: e.platform === "javascript" ? undefined : `${e.project}-production-1`,
      exception: { values: [{ type: e.type, value: e.value, mechanism: { type: "generic", handled: false }, stacktrace: { frames: e.frames } }] },
      tags: browser ? { browser } : { runtime: e.platform === "go" ? "go1.23.2" : "CPython 3.12.7" },
      contexts: browser
        ? { browser: { name: browser, version: browser.includes("Safari") ? "18.1" : "130.0" }, os: { name: browser === "Mobile Safari" ? "iOS" : pick(["macOS", "Windows"]) } }
        : { runtime: { name: e.platform === "go" ? "go" : "CPython", version: e.platform === "go" ? "1.23.2" : "3.12.7" } },
      user: { id: `cus_${nanoid(10)}` },
      request: e.url ? { url: e.url, method: e.url.includes("/v1/") ? "POST" : "GET" } : undefined,
    };
    const res = await fetch(`${API}/api/${s.projectId}/store/?sentry_key=${dsn.public_key}&sentry_version=7`, {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(`ingest ${e.type} -> ${res.status}: ${(await res.text()).slice(0, 200)}`);
    plan.push({ id, at, visitor: pool.length ? pick(pool) : null });
  }
  console.log(`  ${e.project}: ${times.length} × ${e.type}`);
}

async function placeErrorEvents(plan: PlannedEvent[]) {
  // Ingest stamps receipt time; move each event to the moment the story needs.
  // error_events is a hypertable, and an UPDATE cannot move a row into another
  // chunk's time range, so delete and re-insert it (same id) in one statement.
  await Bun.sleep(1500);
  const cols = "id, error_group_id, project_id, environment_id, deployment_id, visitor_id, ip_geolocation_id, source, fingerprint_hash, exception_type, exception_value, data, trace_id_indexed";
  for (let i = 0; i < plan.length; i += 500) {
    const chunk = plan
      .slice(i, i + 500)
      .map((p) => [p.id, new Date(p.at).toISOString(), p.visitor == null ? "" : String(p.visitor)]);
    const picked = cols.replace("visitor_id", "COALESCE(NULLIF(new_visitor, '')::int, visitor_id)");
    await sql`
      WITH moved AS (
        DELETE FROM error_events e USING (VALUES ${sql(chunk)}) AS v(event_id, at, visitor)
        WHERE e.data->'sentry'->>'event_id' = v.event_id
        RETURNING e.*, v.at::timestamptz AS new_at, v.visitor AS new_visitor
      )
      INSERT INTO error_events (${sql.unsafe(cols)}, timestamp, created_at)
      SELECT ${sql.unsafe(picked)}, new_at, new_at FROM moved`;
  }
  await sql`
    UPDATE error_groups g SET first_seen = x.first, last_seen = x.last, total_count = x.n, created_at = x.first
    FROM (SELECT error_group_id, min(timestamp) AS first, max(timestamp) AS last, count(*)::int AS n
          FROM error_events GROUP BY error_group_id) x
    WHERE g.id = x.error_group_id`;
}

async function refreshErrorAggregate() {
  await sql
    .unsafe(`CALL refresh_continuous_aggregate('error_events_hourly', NULL, now() - interval '1 hour')`)
    .then(() => console.log("  refreshed error_events_hourly"))
    .catch((err) => console.log(`  error_events_hourly: not refreshed (${String(err.message).slice(0, 80)})`));
}

// ── stage: traces ─────────────────────────────────────────────────────────
// Cross-service request trees: the shop calls the checkout API, which calls
// Postgres, Redis, the inventory service and the payment provider. Each
// service's spans are ingested into its own project under one trace id, so the
// global waterfall stitches them together exactly like real OTel exporters do.

const hex = (n: number) => Array.from({ length: n }, () => "0123456789abcdef"[Math.floor(rand() * 16)]).join("");
const ms = (t: number) => BigInt(Math.round(t * 1000)) * 1000n; // epoch ms (fractional) -> ns

interface SpanDraft { service: string; name: string; kind: number; start: number; dur: number; attrs: Record<string, AttrValue>; error?: string; children?: SpanDraft[] }

const SERVICE_META: Record<string, { project: string; version: string; lang: string }> = {
  storefront: { project: "storefront", version: "2026.10.28-1", lang: "nodejs" },
  "checkout-api": { project: "checkout-api", version: "1.42.0", lang: "go" },
  "search-api": { project: "search-api", version: "0.9.4", lang: "rust" },
  "billing-worker": { project: "billing-worker", version: "0.31.2", lang: "python" },
  "admin-dashboard": { project: "admin-dashboard", version: "3.18.0", lang: "nodejs" },
};

const pg = (op: string, table: string, stmt: string, med: number): SpanDraft => ({
  service: "checkout-api", name: `${op} ${table}`, kind: SpanKind.CLIENT, start: 0, dur: latency(med, 0.5),
  attrs: { "db.system": "postgresql", "db.operation.name": op, "db.collection.name": table, "db.query.text": stmt, "server.address": "postgres.internal", "server.port": 5432 },
});
const redis = (service: string, key: string): SpanDraft => ({
  service, name: "GET", kind: SpanKind.CLIENT, start: 0, dur: latency(0.8, 0.4),
  attrs: { "db.system": "redis", "db.operation.name": "GET", "db.query.text": `GET ${key}`, "server.address": "redis.internal" },
});
const httpServer = (service: string, method: string, route: string, status = 200): SpanDraft => ({
  service, name: `${method} ${route}`, kind: SpanKind.SERVER, start: 0, dur: 0,
  attrs: { "http.request.method": method, "http.route": route, "url.path": route.replace("[slug]", "alpine-shell-jacket").replace("{id}", String(ri(48000, 52000))), "http.response.status_code": status, "url.scheme": "https" },
});
const httpClient = (service: string, method: string, host: string, path: string, status = 200): SpanDraft => ({
  service, name: `${method}`, kind: SpanKind.CLIENT, start: 0, dur: 0,
  attrs: { "http.request.method": method, "server.address": host, "url.full": `https://${host}${path}`, "http.response.status_code": status },
});

/** Lay children out sequentially inside their parent with small gaps. */
function layout(s: SpanDraft, start: number): number {
  s.start = start;
  let cursor = start + latency(0.6, 0.5);
  for (const c of s.children ?? []) cursor = layout(c, cursor) + latency(0.4, 0.6);
  s.dur = Math.max(s.dur, cursor - start + latency(0.8, 0.5));
  return s.start + s.dur;
}

function checkoutTrace(failing: boolean): SpanDraft {
  const charge: SpanDraft = {
    ...httpClient("checkout-api", "POST", "api.payments.example.net", "/v1/charges", failing ? 504 : 200),
    name: "payments.charge", dur: failing ? ri(10000, 10040) : latency(310, 0.35),
    error: failing ? "payment provider did not respond within 10s (POST /v1/charges)" : undefined,
  };
  charge.attrs["peer.service"] = "payment-provider";
  const api: SpanDraft = {
    ...httpServer("checkout-api", "POST", "/v1/payments", failing ? 504 : 200),
    error: failing ? "upstream payment provider timeout" : undefined,
    children: [
      redis("checkout-api", "cart:{session}"),
      pg("SELECT", "carts", "SELECT id, items, currency FROM carts WHERE session_id = $1", 1.6),
      { ...httpClient("checkout-api", "POST", "inventory.internal", "/reserve"), name: "inventory.reserve", dur: latency(24, 0.4) },
      charge,
      ...(failing ? [] : [pg("INSERT", "orders", "INSERT INTO orders (cart_id, total_minor, idempotency_key) VALUES ($1, $2, $3)", 2.4), pg("UPDATE", "carts", "UPDATE carts SET status = 'converted' WHERE id = $1", 1.1)]),
    ],
  };
  return {
    ...httpServer("storefront", "POST", "/checkout", failing ? 502 : 303),
    error: failing ? "checkout-api responded 504" : undefined,
    children: [
      redis("storefront", "session:{id}"),
      { service: "storefront", name: "render CheckoutForm", kind: SpanKind.INTERNAL, start: 0, dur: latency(6, 0.4), attrs: { "next.span_type": "render" } },
      { ...httpClient("storefront", "POST", "api.example.com", "/v1/payments", failing ? 504 : 200), error: failing ? "504 Gateway Timeout" : undefined, children: [api] },
    ],
  };
}

function productTrace(): SpanDraft {
  return {
    ...httpServer("storefront", "GET", "/products/[slug]"),
    children: [
      redis("storefront", "product:{slug}"),
      {
        ...httpClient("storefront", "GET", "api.example.com", "/v1/products/alpine-shell-jacket"),
        children: [{ ...httpServer("checkout-api", "GET", "/v1/products/{id}"), children: [redis("checkout-api", "product:{id}"), pg("SELECT", "products", "SELECT * FROM products WHERE slug = $1", 2.1), pg("SELECT", "variants", "SELECT * FROM variants WHERE product_id = $1", 1.4)] }],
      },
      { ...httpClient("storefront", "GET", "api.example.com", "/v1/inventory/availability"), children: [{ ...httpServer("checkout-api", "GET", "/v1/inventory/availability"), children: [{ ...httpClient("checkout-api", "GET", "inventory.internal", "/availability"), name: "inventory.availability", dur: latency(14, 0.4) }] }] },
      {
        ...httpClient("storefront", "GET", "search.example.com", "/v1/search"),
        name: "GET /v1/search (related)",
        children: [{ ...httpServer("search-api", "GET", "/v1/search"), children: [{ service: "search-api", name: "tantivy.search", kind: SpanKind.INTERNAL, start: 0, dur: latency(3, 0.4), attrs: { "code.function.name": "search::query", "search.index": "products" } }] }],
      },
      { service: "storefront", name: "render ProductPage", kind: SpanKind.INTERNAL, start: 0, dur: latency(11, 0.4), attrs: { "next.span_type": "render" } },
    ],
  };
}

function cartTrace(): SpanDraft {
  return {
    ...httpServer("checkout-api", "GET", "/v1/cart"),
    children: [redis("checkout-api", "cart:{session}"), pg("SELECT", "carts", "SELECT id, items, currency FROM carts WHERE session_id = $1", 1.5)],
  };
}

function invoiceTrace(): SpanDraft {
  return {
    service: "billing-worker", name: "invoices.generate process", kind: SpanKind.CONSUMER, start: 0, dur: 0,
    attrs: { "messaging.system": "rabbitmq", "messaging.destination.name": "invoices", "messaging.operation.type": "process" },
    children: [
      { service: "billing-worker", name: "SELECT subscriptions", kind: SpanKind.CLIENT, start: 0, dur: latency(4, 0.4), attrs: { "db.system": "postgresql", "db.query.text": "SELECT * FROM subscriptions WHERE renews_at <= now()" } },
      { service: "billing-worker", name: "render_invoice", kind: SpanKind.INTERNAL, start: 0, dur: latency(180, 0.3), attrs: { "code.function.name": "render_invoice" } },
      { service: "billing-worker", name: "PutObject", kind: SpanKind.CLIENT, start: 0, dur: latency(42, 0.4), attrs: { "rpc.system": "aws-api", "rpc.service": "S3", "aws.s3.bucket": "invoices" } },
    ],
  };
}

function flatten(s: SpanDraft, traceId: string, parent: string | undefined, out: Map<string, SeedSpan[]>) {
  const spanId = hex(16);
  const span: SeedSpan = {
    traceId, spanId, parentSpanId: parent, name: s.name, kind: s.kind,
    startTimeUnixNano: ms(s.start), endTimeUnixNano: ms(s.start + s.dur), attributes: s.attrs,
    statusCode: s.error ? StatusCode.ERROR : StatusCode.OK, statusMessage: s.error,
  };
  if (!out.has(s.service)) out.set(s.service, []);
  out.get(s.service)!.push(span);
  for (const c of s.children ?? []) flatten(c, traceId, spanId, out);
}

async function seedTraces(seeded: Seeded[]) {
  console.log("\n▸ traces");
  const already = await sql`SELECT 1 FROM otel_spans LIMIT 1`.catch(() => []);
  if (already.length) { console.log("  traces already seeded, skipping"); return; }
  await login();
  const key = await mintApiKey(api, `showcase-traces-${Date.now()}`, PASSWORD);
  const projectOf = (service: string) => seeded.find((s) => s.spec.name === SERVICE_META[service].project)!;
  await sql`UPDATE projects SET cross_project_trace_sharing = true WHERE id IN ${sql(seeded.map((s) => s.projectId))}`;

  // When each trace happens: dense last day, the incident window, a thin week.
  const plan: { at: number; kind: "checkout" | "product" | "cart" | "invoice"; failing?: boolean }[] = [];
  const kinds = [["product", 46], ["cart", 30], ["checkout", 16], ["invoice", 8]] as const;
  for (let i = 0; i < 2600; i++) plan.push({ at: NOW - Math.pow(rand(), 1.15) * DAY, kind: weighted(kinds) });
  for (let i = 0; i < 900; i++) plan.push({ at: NOW - DAY - rand() * 6 * DAY, kind: weighted(kinds) });
  for (let i = 0; i < 260; i++) {
    const at = INCIDENT.start - 20 * 60_000 + rand() * (INCIDENT.minutes + 40) * 60_000;
    const inWindow = at >= INCIDENT.start && at < INCIDENT.start + INCIDENT.minutes * 60_000;
    plan.push({ at, kind: inWindow && rand() < 0.7 ? "checkout" : weighted(kinds), failing: inWindow && rand() < 0.68 });
  }

  const buffers = new Map<string, SeedSpan[]>();
  const flush = async (service: string, force = false) => {
    const spans = buffers.get(service) ?? [];
    // 500 spans per export, like the OTel SDK's default batch (512). Larger
    // requests currently hit Postgres' bind-parameter limit in span storage.
    if (!spans.length || (!force && spans.length < 500)) return;
    buffers.set(service, []);
    const meta = SERVICE_META[service];
    const group: ResourceSpanGroup = {
      resourceAttributes: {
        "service.name": service, "service.version": meta.version, "deployment.environment.name": "production",
        "telemetry.sdk.language": meta.lang, "telemetry.sdk.name": "opentelemetry", "host.name": `${service}-production-1`,
      },
      scopeName: meta.lang === "go" ? "go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp" : meta.lang === "python" ? "opentelemetry.instrumentation.celery" : meta.lang === "rust" ? "tracing-opentelemetry" : "next.js",
      spans,
    };
    const res = await fetch(`${API}/api/otel/v1/traces`, {
      method: "POST",
      headers: { "Content-Type": "application/x-protobuf", Authorization: `Bearer ${key}`, "X-Temps-Project-Id": String(projectOf(service).projectId) },
      body: encodeExportTraceServiceRequest([group]),
    });
    if (!res.ok) throw new Error(`OTLP ingest for ${service} -> ${res.status}: ${(await res.text()).slice(0, 300)}`);
  };

  let n = 0;
  for (const p of plan) {
    const root = p.kind === "checkout" ? checkoutTrace(!!p.failing) : p.kind === "product" ? productTrace() : p.kind === "cart" ? cartTrace() : invoiceTrace();
    layout(root, p.at);
    flatten(root, hex(32), undefined, buffers);
    n++;
    for (const service of buffers.keys()) await flush(service);
  }
  for (const service of buffers.keys()) await flush(service, true);
  console.log(`  ${n} traces (${plan.filter((p) => p.failing).length} failing checkouts during the incident)`);
}

// ── stage: ai gateway ─────────────────────────────────────────────────────
// `POST /ai/providers` tests the key against the real vendor before saving, so
// a placeholder key cannot go through the API. Store it the way the server
// would (AES-256-GCM under the instance master key, base64(nonce || ct+tag)):
// the provider lists as Active, and any real call fails, which is what a demo
// instance should do.
async function encryptLikeServer(plain: string): Promise<string> {
  const dir = process.env.TEMPS_DATA_DIR;
  if (!dir) throw new Error("TEMPS_DATA_DIR is required for the ai stage (it holds encryption_key)");
  const hexKey = (await Bun.file(`${dir.replace(/^~/, process.env.HOME || "")}/encryption_key`).text()).trim();
  const key = await crypto.subtle.importKey("raw", Buffer.from(hexKey, "hex"), "AES-GCM", false, ["encrypt"]);
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const sealed = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, key, new TextEncoder().encode(plain)));
  return Buffer.concat([nonce, sealed]).toString("base64");
}

const AI_WORKLOADS = [
  // project, provider, model, calls/day, input, output, latency ms, $/M in, $/M out, tag, streaming
  ["storefront", "openai", "gpt-5-mini", 1150, 900, 70, 640, 0.25, 2.0, "search-rerank", false],
  ["storefront", "openai", "gpt-5.4", 40, 2400, 520, 5200, 2.5, 15, "product-copy", true],
  ["admin-dashboard", "anthropic", "claude-sonnet-4-6", 85, 3600, 460, 6100, 3, 15, "support-reply-draft", true],
  ["billing-worker", "anthropic", "claude-haiku-4-5", 310, 1500, 110, 980, 1, 5, "invoice-classify", false],
  [null, "anthropic", "claude-sonnet-4-6", 24, 18000, 900, 14000, 3, 15, "ai-workspace", true],
] as const;

async function seedAi(seeded: Seeded[]) {
  console.log("\n▸ ai gateway");
  const keys = await sql`SELECT provider FROM ai_provider_keys`;
  for (const [provider, name, model] of [["openai", "OpenAI", "gpt-5-mini"], ["anthropic", "Anthropic", "claude-sonnet-4-6"]] as const) {
    if (keys.some((k) => k.provider === provider)) continue;
    const placeholder = `${provider === "openai" ? "sk-proj-" : "sk-ant-api03-"}showcase${nanoid(24)}`;
    await sql`INSERT INTO ai_provider_keys (provider, display_name, api_key_encrypted, is_active, default_model, created_at, updated_at)
              VALUES (${provider}, ${name}, ${await encryptLikeServer(placeholder)}, true, ${model}, ${new Date(NOW - 34 * DAY)}, ${new Date(NOW - 34 * DAY)})`;
    console.log(`  ${name}: placeholder key stored`);
  }
  await seedAiUsage(seeded);
}

/** Gateway request history; `topUp` adds only what happened since the last row. */
async function seedAiUsage(seeded: Seeded[], topUp = false) {
  const already = await sql`SELECT 1 FROM ai_usage_logs WHERE timestamp < now() - interval '1 day' LIMIT 1`;
  if (already.length && !topUp) { console.log("  usage already seeded, skipping"); return; }
  if (!already.length && topUp) { console.log("  no gateway usage to top up, run the ai stage first"); return; }
  const [{ last }] = await sql`SELECT max(timestamp) AS last FROM ai_usage_logs`;
  const fromAt = topUp && last ? new Date(last).getTime() : -Infinity;
  // The generator replays from its fixed seed, so salt ids a top-up would repeat.
  const salt = topUp ? `${Date.now().toString(36)}-` : "";
  const [{ id: userId }] = await sql`SELECT id FROM users WHERE email = ${EMAIL}`; // id 0 is the built-in System user
  const rows: Record<string, unknown>[] = [];
  const convs = Array.from({ length: 40 }, () => nanoid(32));
  for (const [project, provider, model, perDay, inTok, outTok, lat, pin, pout, tag, streaming] of AI_WORKLOADS) {
    const projectId = project ? seeded.find((s) => s.spec.name === project)?.projectId ?? null : null;
    const kind: Kind = project ? PROJECTS.find((p) => p.name === project)!.kind : "admin";
    for (let daysAgo = DAYS; daysAgo >= 0; daysAgo--) {
      const daily = perDay * dayFactor(kind, daysAgo) * (tag === "product-copy" ? 0.4 + rand() : 1);
      for (let h = 0; h < 24; h++) {
        const hourStart = NOW - daysAgo * DAY - (NOW % DAY) + h * HOUR;
        const count = Math.round(daily * hourShare(kind, h) * (0.8 + rand() * 0.4));
        for (let i = 0; i < count; i++) {
          const ts = hourStart + rand() * HOUR;
          if (ts > NOW || ts <= fromAt) continue;
          const status = weighted([[200, 992], [429, 5], [500, 2], [400, 1]] as const);
          const input = Math.round(inTok * (0.5 + rand()));
          const output = status === 200 ? Math.round(outTok * (0.4 + rand() * 1.2)) : 0;
          const cacheRead = provider === "anthropic" && tag !== "invoice-classify" ? Math.round(input * (0.5 + rand() * 0.4)) : 0;
          const dollars = ((input - cacheRead) * pin + cacheRead * pin * 0.1 + output * pout) / 1e6;
          rows.push({
            timestamp: new Date(ts), user_id: tag === "ai-workspace" ? userId : null, provider, model,
            input_tokens: input, output_tokens: output, cache_read_tokens: cacheRead, cache_write_tokens: 0,
            latency_ms: status === 200 ? latency(lat, 0.35) : ri(80, 400), estimated_cost_microcents: Math.round(dollars * 1e8),
            status, is_streaming: streaming, is_byok: false, conversation_id: tag === "ai-workspace" ? pick(convs) : null,
            request_id: `req_${salt}${nanoid(20)}`, trace_id: null, tags: [tag], project_id: projectId,
          });
        }
      }
    }
  }
  if (!rows.length) { console.log("  gateway usage already up to date"); return; }
  await insertBatched("ai_usage_logs", rows, Object.keys(rows[0]));
  console.log(topUp ? `  ${rows.length} gateway requests since the last run` : `  ${rows.length} gateway requests over ${DAYS} days`);
}

// ── stage: ai chat ────────────────────────────────────────────────────────
// Conversations are only written by a real LLM turn, and this instance has no
// working provider, so the thread is inserted directly. Every number in it is
// read back from the data seeded above, so the answer agrees with the errors,
// traces and logs screens.
async function seedChat(seeded: Seeded[]) {
  console.log("\n▸ ai chat");
  const api_ = seeded.find((s) => s.spec.name === "checkout-api");
  if (!api_) return;
  const exists = await sql`SELECT 1 FROM ai_conversations WHERE title = 'Why did checkouts fail on Monday?' LIMIT 1`;
  if (exists.length) { console.log("  already seeded, skipping"); return; }
  const [{ id: userId }] = await sql`SELECT id FROM users WHERE email = ${EMAIL}`; // id 0 is the built-in System user
  const [g] = await sql`SELECT g.id, g.total_count, g.first_seen, g.last_seen FROM error_groups g
                        WHERE g.project_id = ${api_.projectId} AND g.error_type = '*payments.ProviderTimeoutError'`;
  const [win] = await sql`SELECT min(timestamp) AS start, max(timestamp) AS end, count(*)::int AS n FROM error_events
                          WHERE error_group_id = ${g?.id ?? 0} AND timestamp > now() - interval '3 days'`;
  const [px] = await sql`SELECT count(*) FILTER (WHERE status_code >= 500)::int AS fails, count(*)::int AS total
                         FROM proxy_logs WHERE project_id = ${api_.projectId} AND path = '/v1/payments'
                         AND timestamp BETWEEN ${win.start} AND ${win.end}`;
  const fmt = (d: Date) => new Date(d).toISOString().slice(11, 16);
  const day = new Date(win.start).toLocaleDateString("en-US", { weekday: "long", timeZone: "UTC" });
  const mins = Math.max(1, Math.round((new Date(win.end).getTime() - new Date(win.start).getTime()) / 60_000));
  const pct = px.total ? Math.round((px.fails / px.total) * 100) : 0;

  const asked = new Date(NOW - 26 * 60_000);
  const publicId = uuid().replace(/-/g, "");
  const [conv] = await sql`
    INSERT INTO ai_conversations (public_id, project_id, context_type, context_id, title, status, created_by, created_at, last_activity_at, ai_provider, ai_model, turn_status, kind)
    VALUES (${publicId}, ${api_.projectId}, 'project', ${uuid()}, 'Why did checkouts fail on Monday?', 'active', ${userId}, ${asked}, ${new Date(asked.getTime() + 41_000)}, 'gateway', 'claude-sonnet-4-6', 'idle', 'ai')
    RETURNING id`;

  const tool = (id: string, command: string, result: unknown) => ({
    id, name: "temps", arguments: JSON.stringify({ command }), result: JSON.stringify(result),
  });
  const t1 = tool("t1", `errors list --project checkout-api --since 3d`, {
    items: [
      { id: g.id, type: "*payments.ProviderTimeoutError", message: "payment provider did not respond within 10s (POST /v1/charges)", events: win.n, first_seen: win.start, last_seen: win.end, status: "unresolved" },
      { type: "context.deadlineExceededError", message: "inventory: reserve sku=ASJ-M-NAVY: context deadline exceeded", events: 31, status: "unresolved" },
    ],
  });
  const t2 = tool("t2", `logs proxy --project checkout-api --path /v1/payments --from ${fmt(win.start)} --to ${fmt(win.end)} --stats`, {
    requests: px.total, status_5xx: px.fails, error_rate: `${pct}%`, p95_ms: 10012, top_errors: ["upstream timed out after 10000ms", "upstream reset the connection"],
  });
  const t3 = tool("t3", `traces list --project checkout-api --status error --from ${fmt(win.start)} --to ${fmt(win.end)} --limit 1`, {
    items: [{ root: "POST /checkout", services: ["storefront", "checkout-api"], duration_ms: 10071, slowest_span: { name: "payments.charge", duration_ms: 10012, peer_service: "payment-provider" } }],
  });
  const answer = `## Root cause: the payment provider stopped answering

On **${day} between ${fmt(win.start)} and ${fmt(win.end)} UTC** (${mins} min), \`payments.charge\` in **checkout-api** waited the full **10 s timeout** on every call to the provider.

- **${win.n} × \`ProviderTimeoutError\`**, all from \`internal/payments/client.go:211\`
- **${px.fails} of ${px.total}** \`POST /v1/payments\` requests returned 502/504 (${pct}%)
- Traces show the time is spent outside your code: \`payments.charge\` takes 10,012 ms while Postgres, Redis and inventory stay under 30 ms

Nothing changed on your side: no deploy ran in that window, and the errors stopped on their own at ${fmt(win.end)}.

**Suggested fixes**
1. Lower the charge timeout to **3 s with 2 retries**, reusing the idempotency key, so a stall costs one retry instead of a lost order
2. Add an alert on \`/v1/payments\` 5xx rate > 5% for 2 min. You would have been paged at ${fmt(new Date(new Date(win.start).getTime() + 2 * 60_000))}.

Want me to create that alert?`;

  const msgs = [
    { role: "user", content: `Why did checkouts fail on ${day} afternoon? A few customers emailed support.`, at: asked, meta: null, tin: null, tout: null },
    {
      role: "assistant", content: answer, at: new Date(asked.getTime() + 41_000), tin: 18420, tout: 612,
      meta: { tools: [t1, t2, t3], parts: [
        { type: "text", text: "Checking errors, request logs and traces for checkout-api around that time." },
        { type: "tool", tool: t1 }, { type: "tool", tool: t2 }, { type: "tool", tool: t3 },
        { type: "text", text: answer },
      ] },
    },
  ];
  for (const m of msgs) {
    await sql`INSERT INTO ai_messages (conversation_id, role, content, metadata, tokens_in, tokens_out, cost_microcents, created_at)
              VALUES (${conv.id}, ${m.role}, ${m.content}, ${m.meta ? sql.json(m.meta) : null}, ${m.tin}, ${m.tout}, ${m.tin ? 6400000 : null}, ${m.at})`;
  }
  console.log(`  conversation ${publicId}: open /chat?conversation=${publicId}`);
}

// ── main ──────────────────────────────────────────────────────────────────
const stage = process.argv[2] || "all";
const t0 = Date.now();
try {
  let seeded: Seeded[] = [];
  if (stage === "projects" || stage === "all") seeded = await seedProjects();
  if (!seeded.length) seeded = await loadSeeded();
  if (stage === "groups" || stage === "all") await seedGroups(seeded);
  if (stage === "services" || stage === "all") await seedServices(seeded);
  if (stage === "domains" || stage === "all") await seedDomains();
  if (stage === "email" || stage === "all") await seedEmail(seeded);
  if (stage === "deployments" || stage === "all") await seedDeployments(seeded);
  if (stage === "traffic" || stage === "all") {
    console.log("\n▸ traffic");
    const geos = await seedGeo();
    await seedAnalytics(seeded, geos);
    await seedProxyLogs(seeded, geos);
    await refreshAggregates();
  }
  if (stage === "top-up") {
    console.log("\n▸ top-up traffic to now");
    const geos = await seedGeo();
    await seedAnalytics(seeded, geos, true);
    await seedProxyLogs(seeded, geos, true);
    await refreshAggregates();
    await topUpErrors(seeded);
    await seedAiUsage(seeded, true);
  }
  if (stage === "uptime" || stage === "all") await seedUptime(seeded);
  if (stage === "errors" || stage === "all") await seedErrors(seeded);
  if (stage === "traces" || stage === "all") await seedTraces(seeded);
  if (stage === "ai" || stage === "all") await seedAi(seeded);
  if (stage === "chat" || stage === "all") await seedChat(seeded);
  console.log(`\ndone in ${((Date.now() - t0) / 1000).toFixed(1)}s`);
} finally {
  await sql.end();
}
