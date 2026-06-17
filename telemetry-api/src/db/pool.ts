import pg from "pg";

const { Pool } = pg;

let _pool: InstanceType<typeof Pool> | null = null;

// The telemetry data lives in a dedicated database on the linked Temps Postgres
// service. Temps injects POSTGRES_HOST/PORT/USER/PASSWORD for the linked
// service, but the injected POSTGRES_DB / POSTGRES_URL point at the service's
// default `postgres` database — NOT the per-project `telemetry_api_production`
// DB the data actually lives in. So we build the connection from the parts and
// force the database name. This MUST match the dashboard's pool so the public
// API and the private dashboard read/write the same place.
const TELEMETRY_DB = "telemetry_api_production";

function buildPoolConfig(): pg.PoolConfig {
  // Local dev shortcut: an explicit DATABASE_URL wins (see .env.example).
  const explicit = process.env.DATABASE_URL;
  if (explicit) {
    return { connectionString: explicit };
  }

  const host = process.env.POSTGRES_HOST;
  const password = process.env.POSTGRES_PASSWORD;
  if (!host || !password) {
    throw new Error(
      "Database config missing: set DATABASE_URL (local dev) or the POSTGRES_* vars (POSTGRES_HOST/PORT/USER/PASSWORD) injected by the linked Temps Postgres service.",
    );
  }

  return {
    host,
    port: parseInt(process.env.POSTGRES_PORT ?? "5432", 10),
    user: process.env.POSTGRES_USER ?? "postgres",
    password,
    // Forced — ignore POSTGRES_DB/POSTGRES_NAME which point at the wrong DB.
    database: process.env.TELEMETRY_DB_NAME ?? TELEMETRY_DB,
  };
}

export function getPool(): InstanceType<typeof Pool> {
  if (!_pool) {
    _pool = new Pool(buildPoolConfig());
    _pool.on("error", (err) => {
      console.error("[db] idle client error", err.message);
    });
  }
  return _pool;
}

export async function closePool(): Promise<void> {
  if (_pool) {
    await _pool.end();
    _pool = null;
  }
}
