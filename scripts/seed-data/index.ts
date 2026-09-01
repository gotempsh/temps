// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { faker } from "@faker-js/faker";
import postgres from "postgres";

const DATABASE_URL =
  process.env.DATABASE_URL ||
  "postgres://postgres:password@localhost:5432/temps_development";

const sql = postgres(DATABASE_URL, { max: 10 });

// ============================================================
// Configuration
// ============================================================
const ANALYTICS_PROJECT_ID = 2;
const ANALYTICS_ENV_ID = 2; // production
const ANALYTICS_DEPLOYMENT_ID = 4;
const DAYS_BACK = 60;
const NUM_VISITORS = 2000;
const NUM_SESSIONS = 10_000;
// Events will be ~50-100k (variable pages per session)

// All projects for proxy logs
const PROJECTS = [
  { id: 2, envId: 2, deploymentId: 4, host: "temps-landing.example.com" },
  { id: 3, envId: 4, deploymentId: 6, host: "chainlaunch-web.example.com" },
  { id: 5, envId: 6, deploymentId: 59, host: "buildtolearn.example.com" },
  { id: 6, envId: 7, deploymentId: 19, host: "localup-saas.example.com" },
  { id: 7, envId: 8, deploymentId: 41, host: "localup-tunnel.example.com" },
  { id: 9, envId: 20, deploymentId: 140, host: "test-services.example.com" },
  {
    id: 10,
    envId: 25,
    deploymentId: 228,
    host: "storytell-dev.example.com",
  },
  { id: 19, envId: 35, deploymentId: 302, host: "test-xxx.example.com" },
  {
    id: 20,
    envId: 36,
    deploymentId: 325,
    host: "test-repo-deploy.example.com",
  },
];

const PROXY_LOGS_PER_PROJECT = 15_000;

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
  "/api",
  "/docs/getting-started",
  "/docs/api-reference",
  "/docs/deployment",
  "/docs/configuration",
  "/docs/troubleshooting",
  "/blog/introducing-v2",
  "/blog/performance-tips",
  "/blog/security-update",
  "/blog/roadmap-2026",
  "/blog/case-study-acme",
  "/pricing/enterprise",
  "/pricing/startup",
  "/features/analytics",
  "/features/deployments",
  "/features/monitoring",
  "/features/security",
  "/changelog",
  "/terms",
  "/privacy",
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
  "/api": "API Overview - Temps",
  "/docs/getting-started": "Getting Started Guide - Temps Docs",
  "/docs/api-reference": "API Reference - Temps Docs",
  "/docs/deployment": "Deployment Guide - Temps Docs",
  "/docs/configuration": "Configuration - Temps Docs",
  "/docs/troubleshooting": "Troubleshooting - Temps Docs",
  "/blog/introducing-v2": "Introducing Temps v2 - Blog",
  "/blog/performance-tips": "10 Performance Tips - Blog",
  "/blog/security-update": "Security Update March 2026 - Blog",
  "/blog/roadmap-2026": "2026 Roadmap - Blog",
  "/blog/case-study-acme": "Case Study: Acme Corp - Blog",
  "/pricing/enterprise": "Enterprise Pricing - Temps",
  "/pricing/startup": "Startup Plan - Temps",
  "/features/analytics": "Analytics Features - Temps",
  "/features/deployments": "Deployment Features - Temps",
  "/features/monitoring": "Monitoring Features - Temps",
  "/features/security": "Security Features - Temps",
  "/changelog": "Changelog - Temps",
  "/terms": "Terms of Service - Temps",
  "/privacy": "Privacy Policy - Temps",
};

const BROWSERS = [
  { name: "Chrome", versions: ["120.0", "121.0", "122.0", "123.0", "124.0"] },
  {
    name: "Safari",
    versions: ["17.0", "17.1", "17.2", "17.3", "17.4", "17.5"],
  },
  { name: "Firefox", versions: ["120.0", "121.0", "122.0", "123.0"] },
  { name: "Edge", versions: ["120.0", "121.0", "122.0"] },
  { name: "Brave", versions: ["1.60", "1.61", "1.62"] },
];

const OS_LIST = [
  { name: "Windows", versions: ["10.0", "11.0"] },
  {
    name: "macOS",
    versions: ["14.0", "14.1", "14.2", "14.3", "14.4", "15.0"],
  },
  { name: "iOS", versions: ["17.0", "17.1", "17.2", "17.3", "17.4"] },
  { name: "Android", versions: ["13", "14", "15"] },
  { name: "Linux", versions: ["6.1", "6.5", "6.6"] },
];

const DEVICE_TYPES = ["desktop", "desktop", "desktop", "mobile", "mobile", "tablet"]; // weighted

const REFERRERS = [
  "https://www.google.com/",
  "https://www.google.com/search?q=deployment+platform",
  "https://github.com/",
  "https://twitter.com/",
  "https://x.com/",
  "https://news.ycombinator.com/",
  "https://www.reddit.com/r/selfhosted/",
  "https://www.reddit.com/r/webdev/",
  "https://dev.to/",
  "https://medium.com/",
  "https://stackoverflow.com/",
  null, // direct
  null,
  null,
];

const CHANNELS = [
  "organic_search",
  "organic_search",
  "organic_search",
  "direct",
  "direct",
  "referral",
  "social",
  "paid_search",
  "email",
];

const UTM_SOURCES = [
  "google",
  "facebook",
  "twitter",
  "newsletter",
  "partner",
  "linkedin",
  "producthunt",
];
const UTM_MEDIUMS = [
  "cpc",
  "organic",
  "social",
  "email",
  "referral",
  "banner",
];
const UTM_CAMPAIGNS = [
  "spring-launch",
  "black-friday",
  "product-hunt",
  "beta-invite",
  "q1-2026",
  "developer-week",
];

const REFERRER_HOSTNAMES = [
  "www.google.com",
  "github.com",
  "twitter.com",
  "x.com",
  "news.ycombinator.com",
  "www.reddit.com",
  "dev.to",
  "medium.com",
  "stackoverflow.com",
  null,
];

// Static asset paths for proxy logs
const STATIC_PATHS = [
  "/assets/main.js",
  "/assets/main.css",
  "/assets/vendor.js",
  "/assets/app.css",
  "/favicon.ico",
  "/robots.txt",
  "/sitemap.xml",
  "/manifest.json",
  "/assets/logo.svg",
  "/assets/hero.webp",
  "/assets/fonts/inter.woff2",
  "/assets/fonts/mono.woff2",
  "/_next/static/chunks/main.js",
  "/_next/static/chunks/pages/_app.js",
  "/_next/static/css/styles.css",
];

const API_PATHS = [
  "/api/health",
  "/api/v1/projects",
  "/api/v1/deployments",
  "/api/v1/analytics",
  "/api/v1/users/me",
  "/api/v1/settings",
  "/api/v1/domains",
  "/api/v1/environments",
];

const HTTP_METHODS = ["GET", "GET", "GET", "GET", "GET", "POST", "PUT", "DELETE", "PATCH"];

const USER_AGENTS = [
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36",
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15",
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1",
  "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Mobile Safari/537.36",
  "Mozilla/5.0 (X11; Linux x86_64; rv:123.0) Gecko/20100101 Firefox/123.0",
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:122.0) Gecko/20100101 Firefox/122.0",
  "Mozilla/5.0 (iPad; CPU OS 17_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.3 Mobile/15E148 Safari/604.1",
  "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
  "Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)",
  "curl/8.4.0",
];

const BOT_UAS = [
  { ua: "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)", name: "Googlebot" },
  { ua: "Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)", name: "bingbot" },
  { ua: "Mozilla/5.0 (compatible; YandexBot/3.0; +http://yandex.com/bots)", name: "YandexBot" },
  { ua: "facebookexternalhit/1.1", name: "Facebook" },
  { ua: "Twitterbot/1.0", name: "Twitterbot" },
];

// ============================================================
// Helpers
// ============================================================
function pick<T>(arr: T[]): T {
  return arr[Math.floor(Math.random() * arr.length)];
}

function randomInt(min: number, max: number): number {
  return Math.floor(Math.random() * (max - min + 1)) + min;
}

function randomDate(daysBack: number): Date {
  const now = Date.now();
  return new Date(now - Math.random() * daysBack * 86400_000);
}

function randomIp(): string {
  // Mix of public-looking IPs from different ranges
  const ranges = [
    () => `${randomInt(1, 223)}.${randomInt(0, 255)}.${randomInt(0, 255)}.${randomInt(1, 254)}`,
    () => `2001:db8:${randomInt(0, 0xffff).toString(16)}::${randomInt(1, 0xfffe).toString(16)}`, // IPv6
    () => `${pick(["45", "52", "104", "142", "185", "198", "203", "216"])}.${randomInt(0, 255)}.${randomInt(0, 255)}.${randomInt(1, 254)}`,
  ];
  return pick(ranges)();
}

let geoIds: number[] = [];

async function loadGeoIds() {
  const rows = await sql`SELECT id FROM ip_geolocations ORDER BY id`;
  geoIds = rows.map((r: any) => r.id);
  console.log(`  Loaded ${geoIds.length} geolocation IDs`);
}

function randomGeoId(): number {
  return pick(geoIds);
}

// ============================================================
// Step 1: Insert visitors
// ============================================================
async function insertVisitors(): Promise<number[]> {
  console.log(`\nInserting ${NUM_VISITORS} visitors...`);

  const batchSize = 500;
  const allIds: number[] = [];

  for (let batch = 0; batch < NUM_VISITORS; batch += batchSize) {
    const rows = [];
    const end = Math.min(batch + batchSize, NUM_VISITORS);
    for (let i = batch; i < end; i++) {
      const firstSeen = randomDate(DAYS_BACK);
      const lastSeen = new Date(
        firstSeen.getTime() + Math.random() * (Date.now() - firstSeen.getTime())
      );
      const browser = pick(BROWSERS);
      const os = pick(OS_LIST);
      const device = pick(DEVICE_TYPES);

      rows.push({
        visitor_id: `seed-v-${faker.string.nanoid(12)}`,
        first_seen: firstSeen,
        last_seen: lastSeen,
        user_agent: pick(USER_AGENTS.slice(0, 8)), // exclude bots
        ip_address_id: randomGeoId(),
        is_crawler: false,
        project_id: ANALYTICS_PROJECT_ID,
        environment_id: ANALYTICS_ENV_ID,
        has_activity: true,
      });
    }

    const result = await sql`
      INSERT INTO visitor ${sql(rows, "visitor_id", "first_seen", "last_seen", "user_agent", "ip_address_id", "is_crawler", "project_id", "environment_id", "has_activity")}
      RETURNING id
    `;
    allIds.push(...result.map((r: any) => r.id));
    process.stdout.write(`  Visitors: ${allIds.length}/${NUM_VISITORS}\r`);
  }

  console.log(`  Inserted ${allIds.length} visitors (IDs ${allIds[0]}-${allIds[allIds.length - 1]})`);
  return allIds;
}

// ============================================================
// Step 2: Insert request_sessions
// ============================================================
interface SessionInfo {
  dbId: number;
  sessionId: string;
  visitorId: number;
  startedAt: Date;
  referrer: string | null;
  referrerHostname: string | null;
  channel: string;
}

async function insertSessions(visitorIds: number[]): Promise<SessionInfo[]> {
  console.log(`\nInserting ${NUM_SESSIONS} request_sessions...`);

  const batchSize = 500;
  const allSessions: SessionInfo[] = [];

  for (let batch = 0; batch < NUM_SESSIONS; batch += batchSize) {
    const rows = [];
    const metas: { sessionId: string; visitorId: number; startedAt: Date; referrer: string | null; referrerHostname: string | null; channel: string }[] = [];
    const end = Math.min(batch + batchSize, NUM_SESSIONS);

    for (let i = batch; i < end; i++) {
      const sessionId = `seed-s-${faker.string.nanoid(16)}`;
      const visitorId = pick(visitorIds);
      const startedAt = randomDate(DAYS_BACK);
      const lastAccessed = new Date(startedAt.getTime() + randomInt(30, 1800) * 1000);
      const referrer = pick(REFERRERS);
      const channel = pick(CHANNELS);
      const referrerHostname = referrer ? new URL(referrer).hostname : null;
      const hasUtm = Math.random() < 0.15;

      rows.push({
        session_id: sessionId,
        started_at: startedAt,
        last_accessed_at: lastAccessed,
        ip_address: randomIp(),
        user_agent: pick(USER_AGENTS.slice(0, 8)),
        referrer: referrer,
        data: "{}",
        visitor_id: visitorId,
        utm_source: hasUtm ? pick(UTM_SOURCES) : null,
        utm_medium: hasUtm ? pick(UTM_MEDIUMS) : null,
        utm_campaign: Math.random() < 0.1 ? pick(UTM_CAMPAIGNS) : null,
        utm_content: null,
        utm_term: null,
        channel,
        referrer_hostname: referrerHostname,
      });

      metas.push({ sessionId, visitorId, startedAt, referrer, referrerHostname, channel });
    }

    const result = await sql`
      INSERT INTO request_sessions ${sql(rows, "session_id", "started_at", "last_accessed_at", "ip_address", "user_agent", "referrer", "data", "visitor_id", "utm_source", "utm_medium", "utm_campaign", "utm_content", "utm_term", "channel", "referrer_hostname")}
      RETURNING id
    `;

    for (let j = 0; j < result.length; j++) {
      allSessions.push({
        dbId: result[j].id,
        ...metas[j],
      });
    }
    process.stdout.write(`  Sessions: ${allSessions.length}/${NUM_SESSIONS}\r`);
  }

  console.log(`  Inserted ${allSessions.length} sessions`);
  return allSessions;
}

// ============================================================
// Step 3: Insert analytics events (page_view + page_leave)
// ============================================================
async function insertEvents(sessions: SessionInfo[]): Promise<number> {
  console.log(`\nInserting analytics events (page_view + page_leave)...`);

  const batchSize = 2000;
  let totalEvents = 0;
  let eventBuffer: any[] = [];

  async function flushBuffer() {
    if (eventBuffer.length === 0) return;
    await sql`
      INSERT INTO events ${sql(
        eventBuffer,
        "project_id", "environment_id", "deployment_id", "timestamp", "session_id",
        "visitor_id", "hostname", "pathname", "page_path", "href", "page_title",
        "referrer", "is_entry", "is_exit", "is_bounce", "session_page_number",
        "browser", "browser_version", "operating_system", "operating_system_version",
        "device_type", "event_type", "is_crawler", "ip_geolocation_id",
        "scroll_depth", "screen_width", "screen_height", "language"
      )}
    `;
    totalEvents += eventBuffer.length;
    process.stdout.write(`  Events: ${totalEvents}\r`);
    eventBuffer = [];
  }

  for (const session of sessions) {
    const numPages = randomInt(1, 20);
    const isBounce = numPages === 1 && Math.random() < 0.4;
    let cursor = new Date(session.startedAt);
    const browser = pick(BROWSERS);
    const os = pick(OS_LIST);
    const device = pick(DEVICE_TYPES);
    const geoId = randomGeoId();
    const screenW = device === "mobile" ? pick([390, 412, 414]) : device === "tablet" ? pick([768, 810, 834]) : pick([1920, 1440, 1536, 2560]);
    const screenH = device === "mobile" ? pick([844, 915, 896]) : device === "tablet" ? pick([1024, 1080, 1194]) : pick([1080, 900, 864, 1440]);
    const lang = pick(["en-US", "en-GB", "es-ES", "de-DE", "fr-FR", "pt-BR", "ja-JP", "zh-CN", "ko-KR"]);

    for (let p = 0; p < numPages; p++) {
      const pagePath = pick(PAGE_PATHS);
      const pageTitle = PAGE_TITLES[pagePath] || "Temps Platform";
      const isEntry = p === 0;
      const isExit = p === numPages - 1;
      const dwellSeconds = randomInt(5, 180);

      // page_view event
      eventBuffer.push({
        project_id: ANALYTICS_PROJECT_ID,
        environment_id: ANALYTICS_ENV_ID,
        deployment_id: ANALYTICS_DEPLOYMENT_ID,
        timestamp: new Date(cursor),
        session_id: session.sessionId,
        visitor_id: session.visitorId,
        hostname: "temps-landing.example.com",
        pathname: pagePath,
        page_path: pagePath,
        href: `https://temps-landing.example.com${pagePath}`,
        page_title: pageTitle,
        referrer: isEntry ? session.referrer : null,
        is_entry: isEntry,
        is_exit: isExit,
        is_bounce: isBounce && isEntry,
        session_page_number: p + 1,
        browser: browser.name,
        browser_version: pick(browser.versions),
        operating_system: os.name,
        operating_system_version: pick(os.versions),
        device_type: device,
        event_type: "page_view",
        is_crawler: false,
        ip_geolocation_id: geoId,
        scroll_depth: randomInt(10, 100),
        screen_width: screenW,
        screen_height: screenH,
        language: lang,
      });

      // page_leave event (some time after page_view)
      const leaveTime = new Date(cursor.getTime() + dwellSeconds * 1000);
      eventBuffer.push({
        project_id: ANALYTICS_PROJECT_ID,
        environment_id: ANALYTICS_ENV_ID,
        deployment_id: ANALYTICS_DEPLOYMENT_ID,
        timestamp: leaveTime,
        session_id: session.sessionId,
        visitor_id: session.visitorId,
        hostname: "temps-landing.example.com",
        pathname: pagePath,
        page_path: pagePath,
        href: `https://temps-landing.example.com${pagePath}`,
        page_title: pageTitle,
        referrer: null,
        is_entry: false,
        is_exit: false,
        is_bounce: false,
        session_page_number: p + 1,
        browser: browser.name,
        browser_version: pick(browser.versions),
        operating_system: os.name,
        operating_system_version: pick(os.versions),
        device_type: device,
        event_type: "page_leave",
        is_crawler: false,
        ip_geolocation_id: geoId,
        scroll_depth: randomInt(40, 100),
        screen_width: screenW,
        screen_height: screenH,
        language: lang,
      });

      // Move cursor forward
      cursor = new Date(leaveTime.getTime() + randomInt(1, 10) * 1000);

      if (eventBuffer.length >= batchSize) {
        await flushBuffer();
      }
    }
  }

  await flushBuffer();
  console.log(`  Inserted ${totalEvents} analytics events`);
  return totalEvents;
}

// ============================================================
// Step 4: Insert proxy_logs for all projects
// ============================================================
async function insertProxyLogs(): Promise<number> {
  console.log(`\nInserting proxy logs for ${PROJECTS.length} projects (${PROXY_LOGS_PER_PROJECT} each)...`);

  const batchSize = 2000;
  let totalLogs = 0;

  for (const project of PROJECTS) {
    let buffer: any[] = [];

    async function flushProxy() {
      if (buffer.length === 0) return;
      await sql`
        INSERT INTO proxy_logs ${sql(
          buffer,
          "timestamp", "method", "path", "query_string", "host", "status_code",
          "response_time_ms", "request_source", "is_system_request", "routing_status",
          "project_id", "environment_id", "deployment_id", "upstream_host",
          "client_ip", "user_agent", "referrer", "request_id",
          "ip_geolocation_id", "browser", "browser_version",
          "operating_system", "device_type", "is_bot", "bot_name",
          "request_size_bytes", "response_size_bytes", "cache_status", "created_date"
        )}
      `;
      totalLogs += buffer.length;
      process.stdout.write(`  Proxy logs: ${totalLogs}/${PROJECTS.length * PROXY_LOGS_PER_PROJECT} (project ${project.id})\r`);
      buffer = [];
    }

    for (let i = 0; i < PROXY_LOGS_PER_PROJECT; i++) {
      const ts = randomDate(DAYS_BACK);
      const isBot = Math.random() < 0.08;
      const isApi = Math.random() < 0.2;
      const isStatic = !isApi && Math.random() < 0.35;
      const bot = isBot ? pick(BOT_UAS) : null;

      const path = isApi ? pick(API_PATHS) : isStatic ? pick(STATIC_PATHS) : pick(PAGE_PATHS);
      const method = isApi ? pick(HTTP_METHODS) : "GET";

      // Status code distribution: mostly 200, some 301/304, few 404/500
      let statusCode: number;
      const roll = Math.random();
      if (roll < 0.82) statusCode = 200;
      else if (roll < 0.88) statusCode = 304;
      else if (roll < 0.92) statusCode = 301;
      else if (roll < 0.96) statusCode = 404;
      else if (roll < 0.98) statusCode = 403;
      else if (roll < 0.995) statusCode = 500;
      else statusCode = 502;

      // Response time: mostly fast, some slow
      let responseTime: number;
      if (isStatic) responseTime = randomInt(1, 30);
      else if (isApi) responseTime = randomInt(10, 500);
      else responseTime = randomInt(20, 800);
      if (Math.random() < 0.02) responseTime = randomInt(1000, 5000); // occasional slow

      const browserInfo = pick(BROWSERS);
      const osInfo = pick(OS_LIST);
      const device = pick(DEVICE_TYPES);

      const cacheOptions = isStatic
        ? pick(["HIT", "HIT", "HIT", "MISS", "EXPIRED"])
        : pick(["MISS", "BYPASS", "BYPASS", null]);

      buffer.push({
        timestamp: ts,
        method,
        path,
        query_string: isApi && Math.random() < 0.3 ? `page=1&limit=20` : null,
        host: project.host,
        status_code: statusCode,
        response_time_ms: responseTime,
        request_source: isBot ? "bot" : "user",
        is_system_request: false,
        routing_status: statusCode >= 500 ? "error" : "success",
        project_id: project.id,
        environment_id: project.envId,
        deployment_id: project.deploymentId,
        upstream_host: `127.0.0.1:${30000 + project.id}`,
        client_ip: randomIp(),
        user_agent: isBot ? bot!.ua : pick(USER_AGENTS.slice(0, 8)),
        referrer: !isStatic && !isApi && Math.random() < 0.3 ? pick(REFERRERS.filter(Boolean)) : null,
        request_id: faker.string.uuid(),
        ip_geolocation_id: randomGeoId(),
        browser: isBot ? null : browserInfo.name,
        browser_version: isBot ? null : pick(browserInfo.versions),
        operating_system: isBot ? null : osInfo.name,
        device_type: isBot ? null : device,
        is_bot: isBot,
        bot_name: bot?.name || null,
        request_size_bytes: isApi && method !== "GET" ? randomInt(100, 5000) : randomInt(0, 500),
        response_size_bytes: isStatic ? randomInt(1000, 500_000) : isApi ? randomInt(100, 10_000) : randomInt(5000, 100_000),
        cache_status: cacheOptions,
        created_date: ts.toISOString().split("T")[0],
      });

      if (buffer.length >= batchSize) {
        await flushProxy();
      }
    }

    await flushProxy();
  }

  console.log(`  Inserted ${totalLogs} proxy logs across ${PROJECTS.length} projects`);
  return totalLogs;
}

// ============================================================
// Step 5: Update visitor last_seen from actual event data
// ============================================================
async function updateVisitorLastSeen(visitorIds: number[]) {
  console.log("\nUpdating visitor first_seen/last_seen from actual events...");

  await sql`
    UPDATE visitor v
    SET
      first_seen = sub.min_ts,
      last_seen = sub.max_ts
    FROM (
      SELECT visitor_id, MIN(timestamp) as min_ts, MAX(timestamp) as max_ts
      FROM events
      WHERE project_id = ${ANALYTICS_PROJECT_ID}
        AND visitor_id = ANY(${visitorIds})
      GROUP BY visitor_id
    ) sub
    WHERE v.id = sub.visitor_id
  `;

  console.log("  Updated visitor timestamps");
}

// ============================================================
// Main
// ============================================================
async function main() {
  console.log("==============================================");
  console.log("  Temps Analytics & Proxy Logs Seed Script");
  console.log("==============================================");
  console.log(`Database: ${DATABASE_URL.replace(/:[^@]*@/, ":***@")}`);
  console.log(`Analytics: project=${ANALYTICS_PROJECT_ID}, env=${ANALYTICS_ENV_ID}, days=${DAYS_BACK}`);
  console.log(`Proxy logs: ${PROJECTS.length} projects x ${PROXY_LOGS_PER_PROJECT} logs`);

  const start = Date.now();

  await loadGeoIds();

  // Analytics data
  const visitorIds = await insertVisitors();
  const sessions = await insertSessions(visitorIds);
  const eventCount = await insertEvents(sessions);
  await updateVisitorLastSeen(visitorIds);

  // Proxy logs
  const proxyCount = await insertProxyLogs();

  const elapsed = ((Date.now() - start) / 1000).toFixed(1);
  console.log("\n==============================================");
  console.log(`  Done in ${elapsed}s`);
  console.log(`  ${NUM_VISITORS} visitors`);
  console.log(`  ${sessions.length} sessions`);
  console.log(`  ${eventCount} analytics events`);
  console.log(`  ${proxyCount} proxy logs`);
  console.log("==============================================");

  await sql.end();
}

main().catch((err) => {
  console.error("Fatal error:", err);
  sql.end();
  process.exit(1);
});
