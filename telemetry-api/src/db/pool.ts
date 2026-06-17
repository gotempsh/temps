import pg from "pg";

const { Pool } = pg;

let _pool: InstanceType<typeof Pool> | null = null;

export function getPool(): InstanceType<typeof Pool> {
  if (!_pool) {
    // Prefer POSTGRES_URL — Temps injects it automatically when a Postgres
    // service is linked to the project, so the connection stays in sync with
    // the managed service and there's no hand-set secret to drift. Fall back to
    // DATABASE_URL for local dev (see .env.example).
    const url = process.env.POSTGRES_URL ?? process.env.DATABASE_URL;
    if (!url) {
      throw new Error("POSTGRES_URL (or DATABASE_URL) environment variable is required");
    }
    _pool = new Pool({ connectionString: url });

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
