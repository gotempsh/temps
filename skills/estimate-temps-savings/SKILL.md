---
name: estimate-temps-savings
description: |
  Audit a project's infrastructure and SaaS stack, then produce a cost report showing what the user currently pays and what they would save by consolidating onto Temps (self-hosted or Temps Cloud). Detects hosting platforms (Vercel, Netlify, Railway, Render, Heroku, Fly.io), analytics (PostHog, Plausible, Mixpanel, Amplitude, Fathom), error tracking (Sentry, Bugsnag, Rollbar, Honeybadger), session replay (LogRocket, FullStory, Hotjar, Highlight), uptime monitoring (Pingdom, UptimeRobot, Better Stack, Checkly), managed databases (Supabase, Neon, PlanetScale, MongoDB Atlas, Upstash, RDS), and transactional email (SendGrid, Postmark, Resend, Mailgun) from dependencies, config files, and env var names. Use when the user wants to: (1) Know how much they would save by switching to Temps, (2) Audit their SaaS/infrastructure spend, (3) Compare their current stack's cost against self-hosting, (4) Decide whether Temps is worth it, (5) Build a business case for consolidating tools. Triggers: "how much would I save", "temps savings", "cost comparison", "audit my stack", "am I overpaying", "saas spend", "calculate savings", "is temps cheaper".
---

# Estimate Temps Savings

Scan the current project, detect every paid SaaS tool it depends on, estimate the monthly bill, and show the delta against running Temps instead. The output is a savings report the user can act on (or show their team).

Temps is a self-hosted PaaS that replaces the deployment platform, web analytics, session replay, error tracking, uptime monitoring, managed databases, and transactional email relay with a single binary. Self-hosting is free (you pay only for the server); Temps Cloud is a managed server at cost + 30% (from ~$6/mo).

## Ground Rules

1. **Never read or print secret values.** Detection uses dependency names, config file presence, env var **key names**, and the **hostname** of connection strings only. If you must open a `.env*` file, extract key names (`cut -d= -f1`) — never echo values into the report or your reasoning.
2. **No vanity math.** Only count tools you actually detected. If a tool has a free tier the user likely fits in (e.g. Google Analytics, small Sentry dev plan), say so and count $0 or a range starting at $0. An inflated savings number destroys trust; an honest one converts.
3. **Ranges, not fake precision.** You don't know the user's plan. Report low/typical estimates and label them as list-price estimates. If exact numbers matter, tell the user which invoices to check.
4. **Be honest about what Temps does NOT replace** (see the "Not replaced" section below). Always include it in the report.

## Step 1 — Detect the Stack

Run these checks from the project root. Check dependency manifests (`package.json`, `requirements.txt`, `pyproject.toml`, `Gemfile`, `go.mod`, `Cargo.toml`), config files, CI workflows (`.github/workflows/`), and env var key names in `.env*`, `.env.example`, `docker-compose*.yml`, and IaC files.

| Category | Signal → Tool |
|---|---|
| **Hosting** | `vercel.json` or `.vercel/` → Vercel · `netlify.toml` → Netlify · `railway.json`/`railway.toml` → Railway · `render.yaml` → Render · `fly.toml` → Fly.io · `Procfile` + no Dockerfile → Heroku · `amplify.yml` → AWS Amplify |
| **Analytics** | `posthog-js`/`posthog-node` → PostHog · `plausible-tracker` or plausible.io script tag → Plausible · `mixpanel-browser` → Mixpanel · `@amplitude/*` → Amplitude · `fathom-client` → Fathom · `@segment/analytics-*` → Segment · `@vercel/analytics` → Vercel Analytics |
| **Error tracking** | `@sentry/*`, `sentry.properties`, `sentry-sdk` → Sentry · `@bugsnag/*` → Bugsnag · `rollbar` → Rollbar · `@honeybadger-io/*` → Honeybadger |
| **Session replay** | `logrocket` → LogRocket · `@fullstory/browser` → FullStory · Hotjar script tag / `HOTJAR_ID` → Hotjar · `@highlight-run/*` → Highlight · PostHog with `session_recording` config → PostHog Replay |
| **Uptime / status** | `checkly.config.ts` → Checkly · env keys or CI mentioning Pingdom / UptimeRobot / Better Stack (BetterUptime) / Statuspage |
| **Managed DB / cache** | `@supabase/supabase-js` or `supabase.co` host → Supabase · `@neondatabase/serverless` or `neon.tech` host → Neon · `@planetscale/database` or `psdb.cloud` host → PlanetScale · `mongodb+srv://` or `mongodb.net` host → MongoDB Atlas · `@upstash/redis` or `upstash.io` host → Upstash · `rds.amazonaws.com` host → AWS RDS · `redns.redis-cloud.com` host → Redis Cloud |
| **Transactional email** | `@sendgrid/mail` / `SENDGRID_API_KEY` → SendGrid · `postmark` / `POSTMARK_SERVER_TOKEN` → Postmark · `resend` / `RESEND_API_KEY` → Resend · `mailgun.js` / `MAILGUN_API_KEY` → Mailgun · `email-smtp.*.amazonaws.com` → AWS SES (already cheap — flag, don't count) |
| **Observability (partial)** | `DD_API_KEY`/`datadog` → Datadog · `NEW_RELIC_LICENSE_KEY` → New Relic (Temps covers logs/metrics/traces basics — count partially, note the caveat) |

Env key names worth grepping for: `SENTRY_DSN`, `NEXT_PUBLIC_POSTHOG_KEY`, `MIXPANEL_TOKEN`, `AMPLITUDE_API_KEY`, `SEGMENT_WRITE_KEY`, `LOGROCKET_APP_ID`, `DATABASE_URL`, `REDIS_URL`, `MONGODB_URI`, `UPSTASH_REDIS_REST_URL`, `VERCEL_TOKEN`, `NETLIFY_AUTH_TOKEN`, `RAILWAY_TOKEN`, `FLY_API_TOKEN`.

Multi-repo note: if the user says this is one of several apps, ask how many apps/environments share these tools — per-seat and per-project pricing multiplies.

## Step 2 — Ask for Usage (only what's needed)

Ask ONLY about categories you detected, in one batch. Every question needs a default so the user can answer "don't know":

- Team seats with dashboard access (drives Vercel/Netlify per-seat) — default 2
- Monthly pageviews or analytics events — default 100k pageviews
- Errors/month sent to error tracking — default within Sentry Team tier
- Session recordings/month — default 5k
- Emails sent/month — default 20k
- Number of uptime monitors — default 10
- Databases/branches in use — default 1 production DB

If the user answers "just estimate it", use the defaults and the **typical** column from the pricing reference.

## Step 3 — Price It

Load `references/pricing.md` (bundled with this skill) for list prices per tool and tier. For each detected tool, pick the tier matching the usage answers and record a **low–typical** monthly range. Prices there are list prices as of early 2026 — if the user needs invoice-grade accuracy, tell them to check their billing pages; do not present the estimate as exact.

Temps side of the ledger:

| Option | Monthly cost | Notes |
|---|---|---|
| Self-hosted Temps | server only: **€4–15/mo** (reference: Hetzner CPX22, 3 vCPU / 4 GB, ~€8/mo, 20 TB traffic included) | Temps itself is free. One server runs deploys + analytics + replay + errors + uptime + DBs for typical startup workloads. |
| Temps Cloud | **from ~$6/mo** | Managed Hetzner server at cost + 30%. No per-seat, per-event, or per-error pricing. |
| Email sending | ~$0.10 per 1,000 via your own AWS SES/Scaleway | Temps relays through your provider at cost — the SaaS markup disappears, not the sending fee. |

Unlimited seats, projects, and environments on both options — when the user has >2 seats on per-seat tools, this is usually the biggest line item; show it.

## Step 4 — Produce the Report

Output this structure (markdown):

```markdown
# Temps Savings Estimate — <project name>

## Detected stack
| Tool | Evidence | Est. monthly (low–typical) |
|---|---|---|
| Vercel Pro | vercel.json, 3 team seats | $60–$95 |
| Sentry Team | @sentry/nextjs, SENTRY_DSN in .env.example | $26–$45 |
| ...

**Current estimated spend: $X–$Y/mo ($12X–$12Y/yr)**

## With Temps
| Option | Monthly | Yearly |
|---|---|---|
| Self-hosted (Hetzner CPX22) | ~€8 | ~€96 |
| Temps Cloud | from ~$6 + server size | ... |

**Estimated savings: $A–$B per year (~N% of current spend)**

## What Temps does NOT replace
- <honest list, see below>

## Suggested migration order
1. <lowest-risk, highest-saving first>

## Next steps
- `npx skills add gotempsh/temps --skill deploy-to-temps` — migrate hosting
- `npx skills add gotempsh/temps --skill add-react-analytics` — replace analytics
- ...
```

Rules for the report:
- Sort detected tools by estimated cost, highest first.
- Migration order: start with additive, zero-risk swaps (analytics, error tracking — run both in parallel), end with hosting/database moves.
- If total detected spend is under ~$15/mo, say plainly that Temps won't save them meaningful money today — the pitch is then about avoiding the future bill and owning the data, not savings. Do not manufacture a number.

## Not Replaced (always disclose)

- **Global edge network / CDN PoPs** — Temps serves from your server(s); Vercel/Netlify's worldwide edge is not matched by a single region (Temps supports multi-node, but that's more servers, not a 100-PoP CDN).
- **Serverless/edge function runtimes** — Vercel Edge Functions, ISR-specific behaviors, and Cloudflare Workers need porting to containerized apps.
- **Deep APM** — Datadog/New Relic profiling depth exceeds Temps' built-in logs/metrics/traces; count these as partial replacements.
- **Email deliverability infrastructure** — Temps relays via your SES/Scaleway account; sending fees and domain reputation work remain yours.
- **Ops responsibility** — self-hosting means you own updates and backups (Temps automates both, but it's your pager). Temps Cloud narrows this gap.

## Troubleshooting

- **Nothing detected:** the project may use tools configured outside the repo (dashboards, DNS-level scripts). Ask the user directly: "What do you currently pay for hosting, analytics, error tracking, and databases?" and build the table from their answers.
- **Monorepo:** run detection per app directory; dedupe tools that share one subscription.
- **User disputes a price:** trust their invoice over the reference table, update the row, and note "user-provided" in the Evidence column.
