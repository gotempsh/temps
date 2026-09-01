// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Add more visitors on top of existing data. Does NOT delete anything.
 * Usage: DATABASE_URL=postgres://postgres:password@localhost:5432/temps_demo bun run add-visitors.ts
 */
import postgres from "postgres";

const DATABASE_URL = process.env.DATABASE_URL || "postgres://postgres:password@localhost:5432/temps_demo";
const sql = postgres(DATABASE_URL, { max: 10 });

const SCALES: Record<number, number> = {
  1: 5000, 2: 2200, 3: 1600, 4: 1100, 5: 900, 6: 700, 7: 550, 8: 400, 9: 250, 10: 150,
};

const META: Record<number, [number, number]> = {
  1: [1, 4], 2: [2, 7], 3: [3, 10], 4: [4, 14], 5: [5, 19],
  6: [6, 22], 7: [7, 26], 8: [8, 29], 9: [9, 34], 10: [10, 37],
};

const PAGES = ["/", "/about", "/pricing", "/docs", "/blog", "/features", "/contact", "/signup", "/dashboard", "/settings"];
const BROWSERS = ["Chrome", "Safari", "Firefox", "Edge"];
const OSS = ["Windows", "macOS", "iOS", "Android", "Linux"];
const DEVICES = ["desktop", "desktop", "mobile", "mobile", "tablet"];
const LANGS = ["en-US", "en-GB", "es-ES", "de-DE", "fr-FR"];

const pick = <T>(a: T[]): T => a[Math.floor(Math.random() * a.length)];
const ri = (a: number, b: number) => Math.floor(Math.random() * (b - a + 1)) + a;

const weights = [
  0.015, 0.010, 0.008, 0.008, 0.010, 0.015,
  0.025, 0.040, 0.055, 0.065, 0.070, 0.075,
  0.080, 0.085, 0.082, 0.078, 0.075, 0.068,
  0.058, 0.050, 0.042, 0.035, 0.028, 0.020,
];

function hourlyCurve(scale: number): number[] {
  const noisy = weights.map(w => Math.max(0.005, w * (1 + (Math.random() - 0.5) * 0.5)));
  const sum = noisy.reduce((a, b) => a + b, 0);
  return noisy.map(w => Math.max(1, Math.round((w / sum) * scale)));
}

async function main() {
  console.log("Adding visitors (no deletions)...\n");
  const start = Date.now();

  // Load visitor IDs per project
  const vids: Record<number, number[]> = {};
  for (const pid of Object.keys(SCALES).map(Number)) {
    const rows = await sql`SELECT id FROM visitor WHERE project_id = ${pid}`;
    vids[pid] = rows.map((r: any) => r.id);
  }

  const geoRows = await sql`SELECT id FROM ip_geolocations`;
  const geoIds = geoRows.map((r: any) => r.id);

  let total = 0;

  for (const [pidStr, dailyScale] of Object.entries(SCALES)) {
    const pid = Number(pidStr);
    const [envId, depId] = META[pid];
    const visitors = vids[pid];
    if (!visitors.length) continue;

    // Current 24h: full scale. Previous 24h: 55-65% (so trend is +50-80%)
    const prevScale = Math.round(dailyScale * (0.55 + Math.random() * 0.1));
    const prevCurve = hourlyCurve(prevScale);
    const currCurve = hourlyCurve(dailyScale);

    const now = new Date();
    const startHour = new Date(now);
    startHour.setMinutes(0, 0, 0);
    startHour.setHours(startHour.getHours() - 48);

    let buf: any[] = [];
    let projCount = 0;

    const flush = async () => {
      if (!buf.length) return;
      await sql`INSERT INTO events ${sql(buf,
        "project_id", "environment_id", "deployment_id", "timestamp", "session_id",
        "visitor_id", "hostname", "pathname", "page_path", "href", "page_title",
        "referrer", "is_entry", "is_exit", "is_bounce", "session_page_number",
        "browser", "browser_version", "operating_system", "operating_system_version",
        "device_type", "event_type", "is_crawler", "ip_geolocation_id",
        "scroll_depth", "screen_width", "screen_height", "language"
      )}`;
      projCount += buf.length;
      buf = [];
    };

    for (let h = 0; h < 48; h++) {
      const hourTs = new Date(startHour.getTime() + h * 3600_000);
      const curve = h < 24 ? prevCurve : currCurve;
      const hourIdx = h % 24;
      const count = curve[hourIdx];

      for (let v = 0; v < count; v++) {
        const vid = visitors[ri(0, visitors.length - 1)];
        const pg = pick(PAGES);
        const dev = pick(DEVICES);
        buf.push({
          project_id: pid, environment_id: envId, deployment_id: depId,
          timestamp: new Date(hourTs.getTime() + ri(0, 3500) * 1000),
          session_id: `av-${pid}-${h}-${v}`,
          visitor_id: vid,
          hostname: `acme-${pid}.example.com`,
          pathname: pg, page_path: pg,
          href: `https://acme-${pid}.example.com${pg}`,
          page_title: "Acme",
          referrer: v % 5 === 0 ? "https://www.google.com/" : null,
          is_entry: true, is_exit: true, is_bounce: v % 3 === 0, session_page_number: 1,
          browser: pick(BROWSERS), browser_version: "124.0",
          operating_system: pick(OSS), operating_system_version: "14.0",
          device_type: dev, event_type: "page_view", is_crawler: false,
          ip_geolocation_id: pick(geoIds),
          scroll_depth: ri(20, 100),
          screen_width: dev === "mobile" ? 390 : 1920,
          screen_height: dev === "mobile" ? 844 : 1080,
          language: pick(LANGS),
        });
        if (buf.length >= 2000) await flush();
      }
    }
    await flush();
    total += projCount;
    console.log(`  Project ${pid}: +${projCount} events`);
  }

  // Refresh aggregate
  console.log("\nRefreshing events_hourly...");
  const cutoff = new Date(Date.now() - 48 * 3600_000);
  await sql`CALL refresh_continuous_aggregate('events_hourly', ${cutoff}::timestamptz, NOW()::timestamptz)`;

  console.log(`\nDone in ${((Date.now() - start) / 1000).toFixed(1)}s — +${total} events added`);
  await sql.end();
}

main().catch(e => { console.error(e); sql.end(); process.exit(1); });
