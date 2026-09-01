// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Backfill recent analytics events for all projects in the demo database.
 * Reuses existing visitor IDs for speed — only inserts events.
 *
 * Usage: DATABASE_URL=postgres://postgres:password@localhost:5432/temps_demo bun run backfill-recent.ts
 */

import postgres from "postgres";

const DATABASE_URL =
  process.env.DATABASE_URL ||
  "postgres://postgres:password@localhost:5432/temps_demo";

const sql = postgres(DATABASE_URL, { max: 10 });

const HOURS_BACK = 48;

// Daily unique visitor targets per project
const PROJECT_SCALES: Record<number, number> = {
  1: 4500,  // acme-marketing-site
  2: 2000,  // acme-dashboard
  3: 1400,  // acme-blog
  4: 1000,  // acme-docs
  5: 800,   // acme-mobile-web
  6: 600,   // acme-checkout
  7: 500,   // acme-api
  8: 350,   // acme-onboarding
  9: 200,   // acme-admin-panel
  10: 120,  // acme-status-page
};

const PROJECT_META: Record<number, { envId: number; deploymentId: number }> = {
  1: { envId: 1, deploymentId: 4 },
  2: { envId: 2, deploymentId: 7 },
  3: { envId: 3, deploymentId: 10 },
  4: { envId: 4, deploymentId: 14 },
  5: { envId: 5, deploymentId: 19 },
  6: { envId: 6, deploymentId: 22 },
  7: { envId: 7, deploymentId: 26 },
  8: { envId: 8, deploymentId: 29 },
  9: { envId: 9, deploymentId: 34 },
  10: { envId: 10, deploymentId: 37 },
};

const HOSTNAMES: Record<number, string> = {
  1: "acme-marketing.example.com",
  2: "acme-dashboard.example.com",
  3: "acme-blog.example.com",
  4: "acme-docs.example.com",
  5: "acme-mobile.example.com",
  6: "acme-checkout.example.com",
  7: "acme-api.example.com",
  8: "acme-onboarding.example.com",
  9: "acme-admin.example.com",
  10: "acme-status.example.com",
};

const PAGE_PATHS = [
  "/", "/about", "/pricing", "/docs", "/blog", "/features",
  "/contact", "/login", "/signup", "/dashboard", "/settings",
  "/docs/getting-started", "/docs/api-reference", "/blog/introducing-v2",
  "/changelog", "/terms", "/privacy",
];

const BROWSERS = ["Chrome", "Safari", "Firefox", "Edge", "Brave"];
const OS_LIST = ["Windows", "macOS", "iOS", "Android", "Linux"];
const DEVICE_TYPES = ["desktop", "desktop", "desktop", "mobile", "mobile", "tablet"];
const LANGUAGES = ["en-US", "en-GB", "es-ES", "de-DE", "fr-FR", "pt-BR", "ja-JP"];

function pick<T>(arr: T[]): T {
  return arr[Math.floor(Math.random() * arr.length)];
}

function randomInt(min: number, max: number): number {
  return Math.floor(Math.random() * (max - min + 1)) + min;
}

function generateHourlyCurve(totalDailyVisitors: number): number[] {
  const baseWeights = [
    0.015, 0.010, 0.008, 0.008, 0.010, 0.015,
    0.025, 0.040, 0.055, 0.065, 0.070, 0.075,
    0.080, 0.085, 0.082, 0.078, 0.075, 0.068,
    0.058, 0.050, 0.042, 0.035, 0.028, 0.020,
  ];
  const noisyWeights = baseWeights.map(w => Math.max(0.005, w * (1 + (Math.random() - 0.5) * 0.6)));
  const total = noisyWeights.reduce((a, b) => a + b, 0);
  return noisyWeights.map(w => Math.round((w / total) * totalDailyVisitors));
}

let geoIds: number[] = [];

async function main() {
  console.log("==============================================");
  console.log("  Backfill Recent Analytics (48h) — FAST");
  console.log("==============================================");

  const start = Date.now();
  const cutoff = new Date(Date.now() - HOURS_BACK * 3600_000);

  // Load geo IDs
  const geoRows = await sql`SELECT id FROM ip_geolocations ORDER BY id`;
  geoIds = geoRows.map((r: any) => r.id);
  console.log(`Loaded ${geoIds.length} geo IDs`);

  // Load existing visitor IDs per project (reuse them — no new visitor inserts)
  const visitorMap: Record<number, number[]> = {};
  for (const pid of Object.keys(PROJECT_SCALES).map(Number)) {
    const rows = await sql`SELECT id FROM visitor WHERE project_id = ${pid} ORDER BY id`;
    visitorMap[pid] = rows.map((r: any) => r.id);
    console.log(`Project ${pid}: ${visitorMap[pid].length} existing visitors`);
  }

  // Delete recent events
  console.log("\nDeleting events from last 48h...");
  await sql`DELETE FROM events WHERE timestamp >= ${cutoff}`;
  console.log("  Done");

  const now = new Date();
  const startHour = new Date(now);
  startHour.setMinutes(0, 0, 0);
  startHour.setHours(startHour.getHours() - HOURS_BACK);

  let grandTotalEvents = 0;

  for (const [pidStr, dailyScale] of Object.entries(PROJECT_SCALES)) {
    const projectId = Number(pidStr);
    const meta = PROJECT_META[projectId];
    const hostname = HOSTNAMES[projectId];
    const visitors = visitorMap[projectId];

    if (visitors.length === 0) {
      console.log(`  Project ${projectId}: no visitors, skipping`);
      continue;
    }

    // Previous day = 55-65% of current day traffic → +50-80% positive trend
    const prevScale = Math.round(dailyScale * (0.55 + Math.random() * 0.10));
    const curve1 = generateHourlyCurve(prevScale);  // previous 24h — lower
    const curve2 = generateHourlyCurve(dailyScale);  // current 24h — higher
    const fullCurve = [...curve1, ...curve2];

    let buffer: any[] = [];
    let projectEvents = 0;

    async function flush() {
      if (buffer.length === 0) return;
      await sql`
        INSERT INTO events ${sql(
          buffer,
          "project_id", "environment_id", "deployment_id", "timestamp", "session_id",
          "visitor_id", "hostname", "pathname", "page_path", "href", "page_title",
          "referrer", "is_entry", "is_exit", "is_bounce", "session_page_number",
          "browser", "browser_version", "operating_system", "operating_system_version",
          "device_type", "event_type", "is_crawler", "ip_geolocation_id",
          "scroll_depth", "screen_width", "screen_height", "language"
        )}
      `;
      projectEvents += buffer.length;
      buffer = [];
    }

    for (let h = 0; h < HOURS_BACK; h++) {
      const hourStart = new Date(startHour.getTime() + h * 3600_000);
      const visitorsThisHour = fullCurve[h];
      if (visitorsThisHour === 0) continue;

      for (let v = 0; v < visitorsThisHour; v++) {
        // Reuse an existing visitor ID (cycle through them)
        const visitorId = visitors[v % visitors.length];
        const sessionId = `bf-${projectId}-${h}-${v}`;
        const ts = new Date(hourStart.getTime() + randomInt(0, 3500) * 1000);
        const pagePath = pick(PAGE_PATHS);
        const device = pick(DEVICE_TYPES);

        buffer.push({
          project_id: projectId,
          environment_id: meta.envId,
          deployment_id: meta.deploymentId,
          timestamp: ts,
          session_id: sessionId,
          visitor_id: visitorId,
          hostname,
          pathname: pagePath,
          page_path: pagePath,
          href: `https://${hostname}${pagePath}`,
          page_title: `${pagePath === "/" ? "Home" : pagePath.slice(1)} - Acme`,
          referrer: Math.random() < 0.2 ? "https://www.google.com/" : null,
          is_entry: true,
          is_exit: true,
          is_bounce: Math.random() < 0.3,
          session_page_number: 1,
          browser: pick(BROWSERS),
          browser_version: `${randomInt(120, 125)}.0`,
          operating_system: pick(OS_LIST),
          operating_system_version: "14.0",
          device_type: device,
          event_type: "page_view",
          is_crawler: false,
          ip_geolocation_id: pick(geoIds),
          scroll_depth: randomInt(20, 100),
          screen_width: device === "mobile" ? 390 : device === "tablet" ? 768 : 1920,
          screen_height: device === "mobile" ? 844 : device === "tablet" ? 1024 : 1080,
          language: pick(LANGUAGES),
        });

        if (buffer.length >= 2000) {
          await flush();
        }
      }
    }

    await flush();
    grandTotalEvents += projectEvents;
    console.log(`  Project ${projectId}: ${projectEvents} events`);
  }

  // Refresh continuous aggregate
  console.log("\nRefreshing events_hourly...");
  await sql`CALL refresh_continuous_aggregate('events_hourly', ${cutoff}::timestamptz, NOW()::timestamptz)`;
  console.log("  Done");

  const elapsed = ((Date.now() - start) / 1000).toFixed(1);
  console.log(`\nCompleted in ${elapsed}s — ${grandTotalEvents} events inserted`);

  await sql.end();
}

main().catch((err) => {
  console.error("Fatal error:", err);
  sql.end();
  process.exit(1);
});
