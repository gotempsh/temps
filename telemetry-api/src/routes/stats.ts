import type { Pool } from "pg";

export function createStatsRoutes(pool: Pool) {
  return {
    // GET /v1/stats/overview — aggregate product health numbers
    async getOverview(_req: Request): Promise<Response> {
      const { rows } = await pool.query<{
        event_type: string;
        count: string;
      }>(`
        SELECT event_type, COUNT(*)::text AS count
        FROM telemetry_events
        WHERE occurred_at >= NOW() - INTERVAL '30 days'
        GROUP BY event_type
        ORDER BY count DESC
      `);

      const counts: Record<string, number> = {};
      for (const row of rows) {
        counts[row.event_type] = parseInt(row.count, 10);
      }

      // Derived product health metrics
      const attempted = counts["deploy_attempted"] ?? 0;
      const succeeded = counts["deploy_succeeded"] ?? 0;
      const failed = counts["deploy_failed"] ?? 0;
      const deploySuccessRate =
        attempted > 0 ? Math.round((succeeded / attempted) * 100) : null;

      return Response.json({
        window: "last_30_days",
        event_counts: counts,
        derived: {
          deploy_success_rate_pct: deploySuccessRate,
          deploy_attempted: attempted,
          deploy_succeeded: succeeded,
          deploy_failed: failed,
        },
      });
    },

    // GET /v1/stats/active-instances — daily/weekly/monthly active instances
    async getActiveInstances(_req: Request): Promise<Response> {
      const [dai, wai, mai] = await Promise.all([
        pool.query<{ count: string }>(
          `SELECT COUNT(DISTINCT anonymous_id)::text AS count
           FROM telemetry_instance_days
           WHERE day = CURRENT_DATE`
        ),
        pool.query<{ count: string }>(
          `SELECT COUNT(DISTINCT anonymous_id)::text AS count
           FROM telemetry_instance_days
           WHERE day >= CURRENT_DATE - INTERVAL '7 days'`
        ),
        pool.query<{ count: string }>(
          `SELECT COUNT(DISTINCT anonymous_id)::text AS count
           FROM telemetry_instance_days
           WHERE day >= CURRENT_DATE - INTERVAL '30 days'`
        ),
      ]);

      return Response.json({
        daily_active_instances: parseInt(dai.rows[0]?.count ?? "0", 10),
        weekly_active_instances: parseInt(wai.rows[0]?.count ?? "0", 10),
        monthly_active_instances: parseInt(mai.rows[0]?.count ?? "0", 10),
      });
    },

    // GET /v1/stats/funnel — deployment success funnel by anonymous_id cohort
    async getFunnel(_req: Request): Promise<Response> {
      const { rows } = await pool.query<{
        anonymous_id: string;
        attempted: string;
        succeeded: string;
        failed: string;
      }>(`
        SELECT
          anonymous_id,
          COUNT(*) FILTER (WHERE event_type = 'deploy_attempted')::text AS attempted,
          COUNT(*) FILTER (WHERE event_type = 'deploy_succeeded')::text AS succeeded,
          COUNT(*) FILTER (WHERE event_type = 'deploy_failed')::text    AS failed
        FROM telemetry_events
        WHERE event_type IN ('deploy_attempted','deploy_succeeded','deploy_failed')
          AND occurred_at >= NOW() - INTERVAL '30 days'
        GROUP BY anonymous_id
        ORDER BY attempted DESC
        LIMIT 1000
      `);

      const cohorts = rows.map((r) => ({
        anonymous_id: r.anonymous_id,
        attempted: parseInt(r.attempted, 10),
        succeeded: parseInt(r.succeeded, 10),
        failed: parseInt(r.failed, 10),
      }));

      const neverSucceeded = cohorts.filter(
        (c) => c.attempted > 0 && c.succeeded === 0
      ).length;
      const atLeastOneSuccess = cohorts.filter((c) => c.succeeded > 0).length;

      return Response.json({
        window: "last_30_days",
        total_instances_with_deploys: cohorts.length,
        instances_with_at_least_one_success: atLeastOneSuccess,
        instances_that_never_succeeded: neverSucceeded,
        cohorts,
      });
    },

    // GET /v1/stats/countries — distinct active instances per country (30d).
    // Uses the denormalized instance-day table; "unknown" buckets instances
    // with no resolvable country (private IP, missing geo DB, etc.).
    async getCountries(_req: Request): Promise<Response> {
      const { rows } = await pool.query<{ country: string | null; instances: string }>(`
        SELECT country, COUNT(DISTINCT anonymous_id)::text AS instances
        FROM telemetry_instance_days
        WHERE day >= CURRENT_DATE - INTERVAL '30 days'
        GROUP BY country
        ORDER BY instances DESC
      `);

      return Response.json({
        window: "last_30_days",
        countries: rows.map((r) => ({
          country: r.country ?? "unknown",
          instances: parseInt(r.instances, 10),
        })),
      });
    },
  };
}
