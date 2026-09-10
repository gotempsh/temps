// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Live Activity Seed Script
 *
 * Generates fake real-time analytics data to test the Live Globe View.
 * Continuously inserts events and updates visitor timestamps so the
 * live-visitors and recent-activity endpoints return fresh data.
 *
 * Usage:
 *   bun run live-seed.ts
 *   DATABASE_URL=postgres://... bun run live-seed.ts
 *
 * Press Ctrl+C to stop.
 */

import postgres from "postgres";

const DATABASE_URL =
  process.env.DATABASE_URL ||
  "postgres://postgres:password@localhost:5432/temps_development";

const sql = postgres(DATABASE_URL, { max: 5 });

// ============================================================
// Configuration
// ============================================================

// Will auto-detect from the database
let PROJECT_ID = 0;
let ENV_ID = 0;
let DEPLOYMENT_ID = 0;

/** How many simulated visitors are "active" at any time */
const ACTIVE_VISITORS = 12;

/** Interval between batches of events (ms) */
const TICK_INTERVAL_MS = 2000;

/** Events per tick (randomized around this) */
const EVENTS_PER_TICK_MIN = 1;
const EVENTS_PER_TICK_MAX = 5;

// ============================================================
// Realistic data pools
// ============================================================

const PAGE_PATHS = [
  "/",
  "/about",
  "/pricing",
  "/docs",
  "/blog",
  "/features",
  "/contact",
  "/login",
  "/signup",
  "/dashboard",
  "/settings",
  "/docs/getting-started",
  "/docs/api-reference",
  "/docs/deployment",
  "/blog/introducing-v2",
  "/blog/performance-tips",
  "/blog/roadmap-2026",
  "/pricing/enterprise",
  "/pricing/startup",
  "/features/analytics",
  "/features/deployments",
  "/features/monitoring",
  "/changelog",
];

const PAGE_TITLES: Record<string, string> = {
  "/": "Home - Temps Platform",
  "/about": "About Us - Temps",
  "/pricing": "Pricing Plans - Temps",
  "/docs": "Documentation - Temps",
  "/blog": "Blog - Temps",
  "/features": "Features - Temps",
  "/contact": "Contact Us - Temps",
  "/login": "Login - Temps",
  "/signup": "Sign Up - Temps",
  "/dashboard": "Dashboard - Temps",
  "/settings": "Account Settings - Temps",
  "/docs/getting-started": "Getting Started Guide - Temps Docs",
  "/docs/api-reference": "API Reference - Temps Docs",
  "/docs/deployment": "Deployment Guide - Temps Docs",
  "/blog/introducing-v2": "Introducing Temps v2 - Blog",
  "/blog/performance-tips": "10 Performance Tips - Blog",
  "/blog/roadmap-2026": "2026 Roadmap - Blog",
  "/pricing/enterprise": "Enterprise Pricing - Temps",
  "/pricing/startup": "Startup Plan - Temps",
  "/features/analytics": "Analytics Features - Temps",
  "/features/deployments": "Deployment Features - Temps",
  "/features/monitoring": "Monitoring Features - Temps",
  "/changelog": "Changelog - Temps",
};

const BROWSERS = ["Chrome", "Safari", "Firefox", "Edge", "Brave"];
const BROWSER_VERSIONS: Record<string, string[]> = {
  Chrome: ["120.0", "121.0", "122.0", "123.0", "124.0"],
  Safari: ["17.0", "17.1", "17.2", "17.3", "17.4"],
  Firefox: ["120.0", "121.0", "122.0", "123.0"],
  Edge: ["120.0", "121.0", "122.0"],
  Brave: ["1.60", "1.61", "1.62"],
};

const OS_LIST = [
  { name: "Windows", versions: ["10.0", "11.0"] },
  { name: "macOS", versions: ["14.0", "14.1", "14.2", "15.0"] },
  { name: "iOS", versions: ["17.0", "17.1", "17.2", "17.3"] },
  { name: "Android", versions: ["13", "14", "15"] },
  { name: "Linux", versions: ["6.1", "6.5", "6.6"] },
];

const DEVICE_TYPES = [
  "desktop",
  "desktop",
  "desktop",
  "mobile",
  "mobile",
  "tablet",
];

const REFERRERS = [
  null,
  null,
  null, // direct traffic (most common)
  "https://www.google.com/",
  "https://www.google.com/search?q=deployment+platform",
  "https://github.com/",
  "https://twitter.com/",
  "https://x.com/",
  "https://news.ycombinator.com/",
  "https://www.reddit.com/r/selfhosted/",
  "https://www.linkedin.com/",
  "https://dev.to/",
];

const HOSTNAMES = ["temps.example.com", "app.temps.io", "localhost:3000"];

// Cities with coordinates for geo data
const CITIES = [
  { city: "New York", country: "United States", code: "US", lat: 40.7128, lng: -74.006, tz: "America/New_York" },
  { city: "San Francisco", country: "United States", code: "US", lat: 37.7749, lng: -122.4194, tz: "America/Los_Angeles" },
  { city: "London", country: "United Kingdom", code: "GB", lat: 51.5074, lng: -0.1278, tz: "Europe/London" },
  { city: "Berlin", country: "Germany", code: "DE", lat: 52.52, lng: 13.405, tz: "Europe/Berlin" },
  { city: "Tokyo", country: "Japan", code: "JP", lat: 35.6762, lng: 139.6503, tz: "Asia/Tokyo" },
  { city: "Sydney", country: "Australia", code: "AU", lat: -33.8688, lng: 151.2093, tz: "Australia/Sydney" },
  { city: "Paris", country: "France", code: "FR", lat: 48.8566, lng: 2.3522, tz: "Europe/Paris" },
  { city: "Toronto", country: "Canada", code: "CA", lat: 43.6532, lng: -79.3832, tz: "America/Toronto" },
  { city: "Sao Paulo", country: "Brazil", code: "BR", lat: -23.5505, lng: -46.6333, tz: "America/Sao_Paulo" },
  { city: "Mumbai", country: "India", code: "IN", lat: 19.076, lng: 72.8777, tz: "Asia/Kolkata" },
  { city: "Singapore", country: "Singapore", code: "SG", lat: 1.3521, lng: 103.8198, tz: "Asia/Singapore" },
  { city: "Seoul", country: "South Korea", code: "KR", lat: 37.5665, lng: 126.978, tz: "Asia/Seoul" },
  { city: "Amsterdam", country: "Netherlands", code: "NL", lat: 52.3676, lng: 4.9041, tz: "Europe/Amsterdam" },
  { city: "Stockholm", country: "Sweden", code: "SE", lat: 59.3293, lng: 18.0686, tz: "Europe/Stockholm" },
  { city: "Madrid", country: "Spain", code: "ES", lat: 40.4168, lng: -3.7038, tz: "Europe/Madrid" },
  { city: "Dubai", country: "United Arab Emirates", code: "AE", lat: 25.2048, lng: 55.2708, tz: "Asia/Dubai" },
  { city: "Mexico City", country: "Mexico", code: "MX", lat: 19.4326, lng: -99.1332, tz: "America/Mexico_City" },
  { city: "Bangkok", country: "Thailand", code: "TH", lat: 13.7563, lng: 100.5018, tz: "Asia/Bangkok" },
  { city: "Lagos", country: "Nigeria", code: "NG", lat: 6.5244, lng: 3.3792, tz: "Africa/Lagos" },
  { city: "Nairobi", country: "Kenya", code: "KE", lat: -1.2921, lng: 36.8219, tz: "Africa/Nairobi" },
  { city: "Buenos Aires", country: "Argentina", code: "AR", lat: -34.6037, lng: -58.3816, tz: "America/Argentina/Buenos_Aires" },
  { city: "Warsaw", country: "Poland", code: "PL", lat: 52.2297, lng: 21.0122, tz: "Europe/Warsaw" },
  { city: "Cape Town", country: "South Africa", code: "ZA", lat: -33.9249, lng: 18.4241, tz: "Africa/Johannesburg" },
  { city: "Istanbul", country: "Turkey", code: "TR", lat: 41.0082, lng: 28.9784, tz: "Europe/Istanbul" },
];

// ============================================================
// Helpers
// ============================================================

function pick<T>(arr: T[]): T {
  return arr[Math.floor(Math.random() * arr.length)];
}

function randInt(min: number, max: number): number {
  return Math.floor(Math.random() * (max - min + 1)) + min;
}

function nanoid(len = 12): string {
  const chars = "abcdefghijklmnopqrstuvwxyz0123456789";
  let id = "";
  for (let i = 0; i < len; i++) {
    id += chars[Math.floor(Math.random() * chars.length)];
  }
  return id;
}

function generateIP(): string {
  return `${randInt(1, 223)}.${randInt(0, 255)}.${randInt(0, 255)}.${randInt(1, 254)}`;
}

function buildUserAgent(browser: string, os: { name: string; versions: string[] }): string {
  const osVersion = pick(os.versions);
  const browserVersion = pick(BROWSER_VERSIONS[browser] || ["1.0"]);

  if (os.name === "Windows") {
    return `Mozilla/5.0 (Windows NT ${osVersion}; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) ${browser}/${browserVersion} Safari/537.36`;
  }
  if (os.name === "macOS") {
    return `Mozilla/5.0 (Macintosh; Intel Mac OS X ${osVersion.replace(".", "_")}) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/${browserVersion} ${browser}/${browserVersion}`;
  }
  if (os.name === "iOS") {
    return `Mozilla/5.0 (iPhone; CPU iPhone OS ${osVersion.replace(".", "_")} like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/${browserVersion} Mobile/15E148 Safari/604.1`;
  }
  if (os.name === "Android") {
    return `Mozilla/5.0 (Linux; Android ${osVersion}) AppleWebKit/537.36 (KHTML, like Gecko) ${browser}/${browserVersion} Mobile Safari/537.36`;
  }
  return `Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) ${browser}/${browserVersion} Safari/537.36`;
}

// ============================================================
// Simulated Visitor State
// ============================================================

interface SimVisitor {
  visitorDbId: number;
  visitorGuid: string;
  sessionId: string;
  geoId: number;
  city: (typeof CITIES)[number];
  browser: string;
  os: (typeof OS_LIST)[number];
  deviceType: string;
  userAgent: string;
  currentPage: string;
  referrer: string | null;
  pageNumber: number;
}

let activeVisitors: SimVisitor[] = [];

// ============================================================
// Database setup
// ============================================================

async function detectProjectConfig() {
  // Find the first project that has environments and deployments
  const rows = await sql`
    SELECT
      p.id as project_id,
      e.id as env_id,
      d.id as deployment_id
    FROM projects p
    JOIN environments e ON e.project_id = p.id
    JOIN deployments d ON d.project_id = p.id
    ORDER BY p.id ASC
    LIMIT 1
  `;

  if (rows.length === 0) {
    console.error("No projects with environments and deployments found. Seed your database first.");
    process.exit(1);
  }

  PROJECT_ID = rows[0].project_id;
  ENV_ID = rows[0].env_id;
  DEPLOYMENT_ID = rows[0].deployment_id;

  console.log(`Using project_id=${PROJECT_ID}, env_id=${ENV_ID}, deployment_id=${DEPLOYMENT_ID}`);
}

async function ensureGeolocations(): Promise<number[]> {
  // Check existing geolocations
  const existing = await sql`SELECT id FROM ip_geolocations LIMIT 1`;
  if (existing.length > 0) {
    const all = await sql`SELECT id FROM ip_geolocations`;
    return all.map((r) => r.id);
  }

  // Insert geolocations for our cities
  console.log("Inserting ip_geolocations for live-seed cities...");
  const ids: number[] = [];
  for (const city of CITIES) {
    const ip = generateIP();
    const result = await sql`
      INSERT INTO ip_geolocations (ip_address, latitude, longitude, region, city, country, country_code, timezone, is_eu, created_at, updated_at)
      VALUES (${ip}, ${city.lat}, ${city.lng}, ${city.city}, ${city.city}, ${city.country}, ${city.code}, ${city.tz}, ${["DE", "FR", "NL", "SE", "ES", "PL"].includes(city.code)}, NOW(), NOW())
      RETURNING id
    `;
    ids.push(result[0].id);
  }
  console.log(`  Inserted ${ids.length} geolocations`);
  return ids;
}

async function createSimVisitor(geoIds: number[]): Promise<SimVisitor> {
  const city = pick(CITIES);
  const browser = pick(BROWSERS);
  const os = pick(OS_LIST);
  const deviceType = pick(DEVICE_TYPES);
  const userAgent = buildUserAgent(browser, os);
  const visitorGuid = `live-${nanoid(8)}`;
  const sessionId = `live-s-${nanoid(10)}`;
  const referrer = pick(REFERRERS);

  // Pick a geo ID that matches (or just pick any)
  const cityIndex = CITIES.indexOf(city);
  const geoId = cityIndex < geoIds.length ? geoIds[cityIndex] : pick(geoIds);

  // Insert visitor
  const now = new Date();
  const result = await sql`
    INSERT INTO visitor (visitor_id, project_id, environment_id, first_seen, last_seen, user_agent, ip_address_id, is_crawler, has_activity)
    VALUES (${visitorGuid}, ${PROJECT_ID}, ${ENV_ID}, ${now}, ${now}, ${userAgent}, ${geoId}, false, true)
    RETURNING id
  `;

  // Insert request_session
  await sql`
    INSERT INTO request_sessions (session_id, started_at, last_accessed_at, ip_address, user_agent, referrer, data, visitor_id)
    VALUES (${sessionId}, ${now}, ${now}, ${generateIP()}, ${userAgent}, ${referrer}, '{}', ${result[0].id})
  `;

  return {
    visitorDbId: result[0].id,
    visitorGuid,
    sessionId,
    geoId,
    city,
    browser,
    os,
    deviceType,
    userAgent,
    currentPage: pick(PAGE_PATHS),
    referrer,
    pageNumber: 1,
  };
}

async function insertEvent(visitor: SimVisitor, eventType: string = "page_view") {
  const now = new Date();
  const pagePath = eventType === "page_view" ? pick(PAGE_PATHS) : visitor.currentPage;
  const pageTitle = PAGE_TITLES[pagePath] || "Temps Platform";
  const hostname = pick(HOSTNAMES);

  visitor.currentPage = pagePath;
  visitor.pageNumber++;

  await sql`
    INSERT INTO events (
      timestamp, project_id, environment_id, deployment_id, session_id, visitor_id,
      hostname, pathname, page_path, href, page_title,
      referrer, referrer_hostname,
      is_entry, is_exit, is_bounce,
      browser, browser_version, operating_system, operating_system_version,
      device_type, ip_geolocation_id,
      event_type, event_name, user_agent, is_crawler, language,
      session_page_number
    )
    VALUES (
      ${now}, ${PROJECT_ID}, ${ENV_ID}, ${DEPLOYMENT_ID}, ${visitor.sessionId}, ${visitor.visitorDbId},
      ${hostname}, ${pagePath}, ${pagePath}, ${"https://" + hostname + pagePath}, ${pageTitle},
      ${visitor.referrer}, ${visitor.referrer ? new URL(visitor.referrer).hostname : null},
      ${visitor.pageNumber === 2}, false, false,
      ${visitor.browser}, ${pick(BROWSER_VERSIONS[visitor.browser] || ["1.0"])},
      ${visitor.os.name}, ${pick(visitor.os.versions)},
      ${visitor.deviceType}, ${visitor.geoId},
      ${eventType}, ${eventType === "custom" ? pick(["click_cta", "signup_start", "download_pdf", "video_play", "share"]) : null},
      ${visitor.userAgent}, false, ${pick(["en-US", "en-GB", "de-DE", "fr-FR", "es-ES", "ja-JP", "pt-BR"])},
      ${visitor.pageNumber}
    )
  `;

  // Update visitor last_seen
  await sql`
    UPDATE visitor SET last_seen = ${now} WHERE id = ${visitor.visitorDbId}
  `;

  // Update session last_accessed_at
  await sql`
    UPDATE request_sessions SET last_accessed_at = ${now} WHERE session_id = ${visitor.sessionId}
  `;
}

// ============================================================
// Main loop
// ============================================================

async function main() {
  console.log("Live Activity Seed Script");
  console.log("=========================");
  console.log(`Database: ${DATABASE_URL.replace(/:[^:@]+@/, ":***@")}`);
  console.log();

  await detectProjectConfig();
  const geoIds = await ensureGeolocations();

  console.log(`\nBootstrapping ${ACTIVE_VISITORS} simulated visitors...`);
  for (let i = 0; i < ACTIVE_VISITORS; i++) {
    const v = await createSimVisitor(geoIds);
    activeVisitors.push(v);
    // Insert initial page_view for each visitor
    await insertEvent(v, "page_view");
  }
  console.log(`  Created ${activeVisitors.length} visitors with initial events`);

  console.log(`\nStarting live event generation (every ${TICK_INTERVAL_MS}ms)...`);
  console.log("Press Ctrl+C to stop.\n");

  let totalEvents = activeVisitors.length; // count initial events

  const interval = setInterval(async () => {
    try {
      const numEvents = randInt(EVENTS_PER_TICK_MIN, EVENTS_PER_TICK_MAX);

      for (let i = 0; i < numEvents; i++) {
        // Randomly pick a visitor
        const visitor = pick(activeVisitors);

        // 80% page_view, 15% custom event, 5% rotate visitor out
        const roll = Math.random();

        if (roll < 0.05) {
          // Replace this visitor with a new one (simulates someone leaving + someone arriving)
          const idx = activeVisitors.indexOf(visitor);
          const newVisitor = await createSimVisitor(geoIds);
          await insertEvent(newVisitor, "page_view");
          activeVisitors[idx] = newVisitor;
          totalEvents++;
        } else if (roll < 0.20) {
          // Custom event
          await insertEvent(visitor, "custom");
          totalEvents++;
        } else {
          // Page view — visitor navigates to a new page
          await insertEvent(visitor, "page_view");
          totalEvents++;
        }
      }

      const now = new Date().toLocaleTimeString();
      process.stdout.write(
        `\r[${now}] Total events: ${totalEvents} | Active visitors: ${activeVisitors.length} | Last tick: +${numEvents} events`
      );
    } catch (err) {
      console.error("\nError in tick:", err);
    }
  }, TICK_INTERVAL_MS);

  // Graceful shutdown
  process.on("SIGINT", async () => {
    console.log("\n\nShutting down...");
    clearInterval(interval);
    await sql.end();
    console.log(`Total events generated: ${totalEvents}`);
    process.exit(0);
  });
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
