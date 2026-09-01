// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Boost current 24h visitors to produce green positive deltas on the dashboard.
 * Additive only — does NOT delete anything.
 *
 * The previous period (from events_hourly SUM) is inflated by old seed data,
 * so we need to add enough unique visitors in the current 24h to exceed it.
 *
 * Usage: DATABASE_URL=postgres://postgres:password@localhost:5432/temps_demo bun run boost-today.ts
 */
import postgres from "postgres";

const DATABASE_URL = process.env.DATABASE_URL || "postgres://postgres:password@localhost:5432/temps_demo";
const sql = postgres(DATABASE_URL, { max: 10 });

// Target: current unique visitors should be ~1.4-1.7x the previous period SUM(unique_visitors)
// Previous period SUM values (from events_hourly):
//   P1: 5696, P2: 2287, P3: 1788, P4: 1232, P5: 970, P6: 704, P7: 636, P8: 442, P9: 253, P10: 147
// Current unique visitors already:
//   P1: 1624, P2: 795, P3: 561, P4: 407, P5: 326, P6: 242, P7: 184, P8: 141, P9: 66, P10: 46
//
// We need to add NEW unique visitors (not already in events for last 24h).
// Target current = prev * 1.5 (for ~+50% green delta)
// Additional needed = target - current_existing

const TARGETS: Record<number, { prevSum: number; currentExisting: number }> = {
  1:  { prevSum: 5696, currentExisting: 1624 },
  2:  { prevSum: 2287, currentExisting: 795 },
  3:  { prevSum: 1788, currentExisting: 561 },
  4:  { prevSum: 1232, currentExisting: 407 },
  5:  { prevSum: 970,  currentExisting: 326 },
  6:  { prevSum: 704,  currentExisting: 242 },
  7:  { prevSum: 636,  currentExisting: 184 },
  8:  { prevSum: 442,  currentExisting: 141 },
  9:  { prevSum: 253,  currentExisting: 66 },
  10: { prevSum: 147,  currentExisting: 46 },
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

// Natural hourly weights (24h curve peaking mid-day)
const weights = [
  0.015, 0.010, 0.008, 0.008, 0.010, 0.015,
  0.025, 0.040, 0.055, 0.065, 0.070, 0.075,
  0.080, 0.085, 0.082, 0.078, 0.075, 0.068,
  0.058, 0.050, 0.042, 0.035, 0.028, 0.020,
];

function hourlyCurve(scale: number): number[] {
  const noisy = weights.map(w => Math.max(0.005, w * (1 + (Math.random() - 0.5) * 0.4)));
  const sum = noisy.reduce((a, b) => a + b, 0);
  return noisy.map(w => Math.max(1, Math.round((w / sum) * scale)));
}

async function main() {
  console.log("Boosting current 24h visitors for green deltas...\n");
  const start = Date.now();

  // Load visitor IDs per project (we need UNUSED ones for current 24h)
  const allVids: Record<number, number[]> = {};
  const usedVids: Record<number, Set<number>> = {};

  for (const pid of Object.keys(TARGETS).map(Number)) {
    // All visitors for this project
    const allRows = await sql`SELECT id FROM visitor WHERE project_id = ${pid}`;
    allVids[pid] = allRows.map((r: any) => r.id);

    // Visitors already in current 24h
    const usedRows = await sql`
      SELECT DISTINCT visitor_id FROM events
      WHERE project_id = ${pid} AND timestamp >= NOW() - INTERVAL '24 hours'
    `;
    usedVids[pid] = new Set(usedRows.map((r: any) => r.visitor_id));
  }

  const geoRows = await sql`SELECT id FROM ip_geolocations`;
  const geoIds = geoRows.map((r: any) => r.id);

  let totalEvents = 0;

  for (const [pidStr, target] of Object.entries(TARGETS)) {
    const pid = Number(pidStr);
    const [envId, depId] = META[pid];
    const allVisitors = allVids[pid];
    const usedSet = usedVids[pid];

    if (!allVisitors.length) {
      console.log(`  Project ${pid}: no visitors available, skipping`);
      continue;
    }

    // Find unused visitors
    const unusedVisitors = allVisitors.filter(v => !usedSet.has(v));

    // Target: prevSum * multiplier for a nice green delta
    const multiplier = 1.4 + Math.random() * 0.3; // 1.4-1.7x
    const targetUnique = Math.round(target.prevSum * multiplier);
    const additionalNeeded = Math.max(0, targetUnique - target.currentExisting);

    if (additionalNeeded === 0) {
      console.log(`  Project ${pid}: already at target, skipping`);
      continue;
    }

    // We need additionalNeeded unique visitors. If we don't have enough unused,
    // we'll create new visitor records.
    let visitorsToUse: number[] = [];

    if (unusedVisitors.length >= additionalNeeded) {
      // Shuffle and take what we need
      const shuffled = unusedVisitors.sort(() => Math.random() - 0.5);
      visitorsToUse = shuffled.slice(0, additionalNeeded);
    } else {
      // Use all unused + create new visitors
      visitorsToUse = [...unusedVisitors];
      const newNeeded = additionalNeeded - unusedVisitors.length;

      console.log(`  Project ${pid}: creating ${newNeeded} new visitors...`);
      const newVisitorBuf: any[] = [];
      for (let i = 0; i < newNeeded; i++) {
        newVisitorBuf.push({
          project_id: pid,
          environment_id: envId,
          visitor_id: `boost-${pid}-${Date.now()}-${i}`,
          first_seen: new Date(),
          last_seen: new Date(),
        });
        if (newVisitorBuf.length >= 2000) {
          const inserted = await sql`INSERT INTO visitor ${sql(newVisitorBuf, "project_id", "environment_id", "visitor_id", "first_seen", "last_seen")} RETURNING id`;
          visitorsToUse.push(...inserted.map((r: any) => r.id));
          newVisitorBuf.length = 0;
        }
      }
      if (newVisitorBuf.length) {
        const inserted = await sql`INSERT INTO visitor ${sql(newVisitorBuf, "project_id", "environment_id", "visitor_id", "first_seen", "last_seen")} RETURNING id`;
        visitorsToUse.push(...inserted.map((r: any) => r.id));
      }
    }

    // Distribute events across last 24h with natural curve
    // Each unique visitor gets exactly 1 event (to maximize unique count per event)
    const curve = hourlyCurve(visitorsToUse.length);
    const now = new Date();
    const startHour = new Date(now);
    startHour.setMinutes(0, 0, 0);
    startHour.setHours(startHour.getHours() - 23); // 24h ago to now

    let vidIdx = 0;
    let buf: any[] = [];
    let projEvents = 0;

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
      projEvents += buf.length;
      buf = [];
    };

    for (let h = 0; h < 24; h++) {
      const hourTs = new Date(startHour.getTime() + h * 3600_000);
      // Only generate events for hours that are in the past
      if (hourTs > now) break;

      const count = curve[h];
      for (let v = 0; v < count && vidIdx < visitorsToUse.length; v++, vidIdx++) {
        const vid = visitorsToUse[vidIdx];
        const pg = pick(PAGES);
        const dev = pick(DEVICES);
        buf.push({
          project_id: pid,
          environment_id: envId,
          deployment_id: depId,
          timestamp: new Date(hourTs.getTime() + ri(0, 3500) * 1000),
          session_id: `boost-${pid}-${h}-${v}-${Date.now()}`,
          visitor_id: vid,
          hostname: `acme-${pid}.example.com`,
          pathname: pg,
          page_path: pg,
          href: `https://acme-${pid}.example.com${pg}`,
          page_title: "Acme",
          referrer: v % 5 === 0 ? "https://www.google.com/" : null,
          is_entry: true,
          is_exit: true,
          is_bounce: v % 3 === 0,
          session_page_number: 1,
          browser: pick(BROWSERS),
          browser_version: "124.0",
          operating_system: pick(OSS),
          operating_system_version: "14.0",
          device_type: dev,
          event_type: "page_view",
          is_crawler: false,
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
    totalEvents += projEvents;

    const newTotal = target.currentExisting + visitorsToUse.length;
    const delta = Math.round(((newTotal - target.prevSum) / target.prevSum) * 100);
    console.log(`  Project ${pid}: +${projEvents} events, +${visitorsToUse.length} unique visitors → ~${newTotal} total (${delta >= 0 ? '+' : ''}${delta}% vs prev)`);
  }

  // Refresh aggregate so current period shows up in hourly data
  console.log("\nRefreshing events_hourly...");
  const cutoff = new Date(Date.now() - 48 * 3600_000);
  await sql`CALL refresh_continuous_aggregate('events_hourly', ${cutoff}::timestamptz, NOW()::timestamptz)`;

  console.log(`\nDone in ${((Date.now() - start) / 1000).toFixed(1)}s — +${totalEvents} events added`);
  await sql.end();
}

main().catch(e => { console.error(e); sql.end(); process.exit(1); });
