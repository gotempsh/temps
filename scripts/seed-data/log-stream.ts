// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Streams realistic structured logs into the showcase services' containers,
 * so the Logs explorer has real traffic to show.
 *
 * Container log timestamps are assigned by Docker on receipt, so history
 * cannot be backdated: this writes live, through the real pipeline (Docker →
 * collector → head buffer → chunks), for as long as it runs. Each line goes
 * to the container's own stdout (`/proc/1/fd/1`), exactly where the app
 * would have written it.
 *
 *   bun run log-stream.ts [minutes=45]
 *
 * Everything it writes is fictional: example.com hosts, generated ids.
 */

const MINUTES = Number(process.argv[2] ?? 45);
const END = Date.now() + MINUTES * 60_000;

const hex = (n: number) =>
  Array.from({ length: n }, () => Math.floor(Math.random() * 16).toString(16)).join("");
const pick = <T>(a: readonly T[]): T => a[Math.floor(Math.random() * a.length)];
const ms = (lo: number, hi: number) => Math.round(lo + Math.random() ** 2 * (hi - lo));
const chance = (p: number) => Math.random() < p;

type Line = Record<string, unknown>;
interface Profile {
  container: string;
  /** Mean lines per second. */
  rate: number;
  line: () => Line;
}

const req = (method: string, path: string, status: number, duration: number, extra: Line = {}): Line => ({
  level: status >= 500 ? "error" : status >= 400 ? "warn" : "info",
  msg: `${method} ${path} ${status} ${duration}ms`,
  method, path, status, duration_ms: duration, request_id: `req_${hex(12)}`, ...extra,
});

const SKUS = ["merino-crew", "linen-shirt", "trail-runner", "canvas-tote", "wool-beanie", "rain-shell"];

const PROFILES: Profile[] = [
  {
    container: "storefront-1", rate: 2.2,
    line: () => {
      if (chance(0.02)) return { level: "error", msg: "Failed to load product variants", sku: pick(SKUS), error: "fetch failed: upstream returned 503", route: "/products/[slug]" };
      if (chance(0.05)) return { level: "warn", msg: "Slow server render", route: "/products/[slug]", render_ms: ms(800, 2400) };
      if (chance(0.06)) return { level: "debug", msg: "ISR cache hit", route: "/collections/[slug]", age_s: ms(1, 300) };
      return req("GET", pick([`/products/${pick(SKUS)}`, "/", "/collections/new-in", "/cart", "/search?q=jacket"]), 200, ms(8, 180));
    },
  },
  {
    container: "checkout-api-1", rate: 2.6,
    line: () => {
      const trace_id = hex(32);
      if (chance(0.025)) return { level: "error", msg: "payment provider did not respond within 10s", provider: "card-gateway", order_id: `ord_${hex(10)}`, trace_id, attempt: ms(1, 3) };
      if (chance(0.05)) return { level: "warn", msg: "payment provider latency above SLO", provider: "card-gateway", latency_ms: ms(1800, 6200), trace_id };
      if (chance(0.15)) return { level: "info", msg: "payment captured", order_id: `ord_${hex(10)}`, amount_cents: ms(1500, 24000), currency: "EUR", trace_id };
      if (chance(0.08)) return { level: "info", msg: "inventory reserved", sku: pick(SKUS).toUpperCase(), qty: ms(1, 3), trace_id };
      return req(pick(["POST", "GET", "POST"]), pick(["/v1/payments", "/v1/checkout/sessions", "/v1/cart", "/v1/orders"]), 200, ms(12, 420), { trace_id });
    },
  },
  {
    container: "search-api-1", rate: 1.6,
    line: () => {
      if (chance(0.01)) return { level: "warn", msg: "query exceeded budget, returning partial results", q: pick(["jacket", "linen", "tote bag", "running"]), took_ms: ms(300, 900) };
      if (chance(0.02)) return { level: "info", msg: "index segment merged", segment: `seg_${hex(6)}`, docs: ms(2000, 18000) };
      return { level: "info", msg: "query served", q: pick(["jacket", "linen shirt", "tote", "beanie", "trail", "rain"]), hits: ms(0, 240), took_ms: ms(2, 60) };
    },
  },
  {
    container: "notifications-1", rate: 0.9,
    line: () => {
      if (chance(0.04)) return { level: "warn", msg: "SMTP 421 4.7.0 try again later, retrying", retry_in_s: 30, template: "order-shipped" };
      if (chance(0.02)) return { level: "error", msg: "Cannot read properties of undefined (reading 'email')", job: `job_${hex(8)}`, template: "order-shipped" };
      return { level: "info", msg: pick(["email sent", "email queued", "push sent"]), template: pick(["order-confirmed", "order-shipped", "return-received", "password-reset"]), job: `job_${hex(8)}` };
    },
  },
  {
    container: "billing-worker-1", rate: 0.4,
    line: () => {
      if (chance(0.05)) return { level: "warn", msg: "billing provider rate limited, backing off", retry_after_s: 30 };
      if (chance(0.3)) return { level: "debug", msg: "invoice batch progress", done: ms(10, 480), total: 480 };
      return { level: "info", msg: pick(["invoice created", "invoice sent", "subscription renewed"]), customer: `cus_${hex(10)}`, amount_cents: ms(900, 9900) };
    },
  },
  {
    container: "recommendations-1", rate: 1.1,
    line: () => {
      if (chance(0.03)) return { level: "warn", msg: "feature store read slow", segment: "returning_buyers", latency_ms: ms(900, 2100) };
      if (chance(0.01)) return { level: "error", msg: "feature store read timed out after 2.0s", segment: "returning_buyers" };
      return req("POST", "/v1/recommend", 200, ms(18, 140), { items: ms(4, 12), model: "two-tower-v7" });
    },
  },
  {
    container: "returns-portal-1", rate: 0.5,
    line: () => {
      if (chance(0.03)) return { level: "error", msg: "SQLSTATE[23000]: Integrity constraint violation: 1062 Duplicate entry for key 'returns_order_line_unique'", order_line: ms(10000, 99999) };
      if (chance(0.2)) return { level: "info", msg: "return created", return_id: `ret_${hex(8)}`, reason: pick(["wrong size", "changed mind", "damaged"]) };
      return req(pick(["GET", "POST"]), pick(["/returns/create", "/returns", "/returns/status"]), 200, ms(20, 260));
    },
  },
  { container: "admin-dashboard-1", rate: 0.3, line: () => req("GET", pick(["/orders", "/customers", "/reports/daily", "/inventory"]), 200, ms(15, 240)) },
  { container: "marketing-site-1", rate: 0.8, line: () => (chance(0.02) ? { level: "warn", msg: "newsletter signup failed, provider returned 502", attempt: 1 } : req("GET", pick(["/", "/about", "/journal/autumn-edit", "/stores"]), 200, ms(3, 40))) },
  { container: "docs-1", rate: 0.5, line: () => req("GET", pick(["/guides/returns", "/guides/shipping", "/api/payments", "/changelog"]), 200, ms(2, 30)) },
  { container: "status-page-1", rate: 0.2, line: () => (chance(0.1) ? { level: "info", msg: "status refreshed", components: 6, degraded: 0 } : req("GET", "/", 200, ms(2, 25))) },
];

async function run(profile: Profile) {
  const proc = Bun.spawn(["docker", "exec", "-i", profile.container, "sh", "-c", "cat > /proc/1/fd/1"], {
    stdin: "pipe", stdout: "ignore", stderr: "inherit",
  });
  let lines = 0;
  while (Date.now() < END) {
    // Exponential gaps give the uneven rhythm of real traffic.
    await Bun.sleep(-Math.log(1 - Math.random()) * (1000 / profile.rate));
    proc.stdin.write(JSON.stringify(profile.line()) + "\n");
    lines++;
  }
  proc.stdin.end();
  await proc.exited;
  console.log(`  ${profile.container}: ${lines} lines`);
}

console.log(`▸ streaming logs into ${PROFILES.length} containers for ${MINUTES} min`);
await Promise.all(PROFILES.map(run));
console.log("done");
