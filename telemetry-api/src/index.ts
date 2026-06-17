import { getPool } from "./db/pool.js";
import { createEventsRoutes } from "./routes/events.js";
import { createStatsRoutes } from "./routes/stats.js";
import { initGeo } from "./geo.js";

const PORT = parseInt(process.env.PORT ?? "4200", 10);

async function main() {
  // Eagerly connect to confirm DB is reachable at startup
  const pool = getPool();
  const client = await pool.connect();
  await client.query("SELECT 1");
  client.release();
  console.log("[server] database connection ok");

  // Load the GeoLite2-Country DB once (degrades gracefully if absent).
  await initGeo();

  const events = createEventsRoutes(pool);
  const stats = createStatsRoutes(pool);

  const server = Bun.serve({
    port: PORT,
    async fetch(req) {
      const url = new URL(req.url);
      const method = req.method.toUpperCase();
      const path = url.pathname;

      // Health check — no auth required
      if (method === "GET" && path === "/health") {
        return Response.json({ ok: true });
      }

      // Ingest endpoints
      if (method === "POST" && path === "/v1/events") {
        return events.postEvent(req);
      }
      if (method === "POST" && path === "/v1/events/batch") {
        return events.postBatch(req);
      }

      // Stats endpoints (add auth in production via INGEST_API_KEY or network policy)
      if (method === "GET" && path === "/v1/stats/overview") {
        return stats.getOverview(req);
      }
      if (method === "GET" && path === "/v1/stats/active-instances") {
        return stats.getActiveInstances(req);
      }
      if (method === "GET" && path === "/v1/stats/funnel") {
        return stats.getFunnel(req);
      }
      if (method === "GET" && path === "/v1/stats/countries") {
        return stats.getCountries(req);
      }

      return Response.json({ error: "not found" }, { status: 404 });
    },
    error(err) {
      console.error("[server] unhandled error:", err);
      return Response.json({ error: "internal server error" }, { status: 500 });
    },
  });

  console.log(`[server] temps telemetry API listening on http://localhost:${server.port}`);
}

main().catch((err) => {
  console.error("[server] startup failed:", err);
  process.exit(1);
});
