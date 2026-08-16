# SaaS List-Price Reference

List prices as of **early 2026**, USD unless noted. These drift — treat as estimates, and prefer the user's actual invoice when they have one. "Typical" assumes a small team (2–5 seats) at the default usage from Step 2 of the skill.

## Hosting / Deployment

| Tool | Entry paid tier | Typical small-team monthly | Pricing model |
|---|---|---|---|
| Vercel | Pro $20/seat | $40–$100 | Per-seat + usage (1 TB fast data transfer included, ~$0.15/GB after; function duration billed) |
| Netlify | Pro $19/seat | $38–$80 | Per-seat + bandwidth overages ($55/100GB after 1TB) |
| Heroku | Basic dyno $7 | $25–$75 | Per-dyno; Standard-1X $25; Postgres Basic $9, Standard-0 $50 |
| Railway | usage, ~$5 min | $10–$40 | Per-resource usage (vCPU/GB-hour) |
| Render | $7–$25/service | $15–$60 | Per-service instances + managed Postgres from $19 |
| Fly.io | usage | $10–$50 | Per-VM usage + volumes + egress |
| AWS Amplify | usage | $10–$40 | Build minutes + hosting + egress |

## Web Analytics

| Tool | Free tier | Typical monthly | Model |
|---|---|---|---|
| PostHog (analytics) | 1M events/mo | $0–$60 | ~$0.00031/event after free tier, decreasing with volume |
| Plausible | none (30-day trial) | $9 (10k views) / $19 (100k) / $69 (1M) | Pageview tiers |
| Fathom | none | $15 (100k views) → $25 (200k) | Pageview tiers |
| Mixpanel | generous free (up to ~20M events) | $0–$28+ | Event-based; Growth from ~$28 |
| Amplitude | 50k MTU free | $0–$61+ | Plus from $61 |
| Segment (CDP) | 1k visitors free | $120+ | Team plan $120; scales fast with MTU |
| Vercel Analytics | basic included on Pro | $10–$50 | Plus $10/mo + per-event over 25k |
| Google Analytics | free | $0 | Don't count as savings |

## Error Tracking

| Tool | Free tier | Typical monthly | Model |
|---|---|---|---|
| Sentry | 5k errors (dev) | $26 (Team) – $80 (Business) | Volume-based; overages per error/span/replay |
| Bugsnag | 7.5k events free | $59+ | Event volume |
| Rollbar | 5k events free | $41+ (Essentials) | Event volume |
| Honeybadger | 1k errors free | $26+ | Tiered |

## Session Replay

| Tool | Free tier | Typical monthly | Model |
|---|---|---|---|
| LogRocket | 1k sessions free | $69–$350 | Per-session tiers |
| FullStory | limited free | $250–$800+ (quote) | Enterprise quotes; rarely under $250 |
| Hotjar | 35 daily sessions free | $32 (Plus) – $80 (Business) | Daily session tiers |
| Highlight.io | 500 sessions free | $50–$150 | Usage |
| PostHog Replay | 5k recordings/mo free | $0–$50 | ~$0.005/recording after free |

## Uptime Monitoring / Status Pages

| Tool | Free tier | Typical monthly | Model |
|---|---|---|---|
| Pingdom | none | $10–$15 (Synthetic starter) | Per-monitor bundles |
| UptimeRobot | 50 monitors @ 5min | $0–$29 | Solo $7, Team $29 |
| Better Stack | 10 monitors free | $0–$29+ | Per-monitor + team |
| Checkly | 10k API checks free | $40+ (Team) | Check-run volume |
| Atlassian Statuspage | limited free | $29–$99 | Page tiers |

## Managed Databases / Cache / Storage

| Tool | Free tier | Typical monthly | Model |
|---|---|---|---|
| Supabase | 500MB free | $25 (Pro) + usage | Per-project + compute add-ons |
| Neon | 0.5GB free | $19 (Launch) – $69 (Scale) | Compute-hours + storage |
| PlanetScale | none (removed 2024) | $39+ (Scaler Pro) | Per-branch compute |
| MongoDB Atlas | M0 free | $57+ (M10 dedicated) | Instance size |
| AWS RDS | none | $13–$60 (t4g.micro–small + storage) | Instance-hours + storage + I/O |
| Upstash Redis | 10k cmd/day free | $0–$20 | Per-request |
| Redis Cloud | 30MB free | $5–$50 | Fixed tiers |

Temps equivalents: managed Postgres/TimescaleDB, MySQL/MariaDB, MongoDB, Redis, and S3-compatible storage as containers on your server — cost is included in the server. Honesty note: a $6 VPS is not an HA multi-AZ RDS cluster; if the user runs production HA today, compare against a bigger multi-node Temps setup, not the minimum.

## Transactional Email

| Tool | Free tier | Typical monthly | Model |
|---|---|---|---|
| SendGrid | 100/day free | $19.95 (50k emails) | Volume tiers |
| Postmark | 100/mo free | $15 (10k) – $55 (125k) | Volume tiers |
| Resend | 3k/mo free | $20 (50k) | Volume tiers |
| Mailgun | 100/day free | $35 (50k, Foundation) | Volume tiers |
| AWS SES | 62k free from EC2 | ~$0.10 per 1,000 | Already at cost — flag as "keep, route through Temps" |

Temps relays through the user's own SES/Scaleway account, so replacing e.g. Postmark 125k ($55) with SES via Temps (~$12.50) is a real line item.

## Observability (partial replacement only)

| Tool | Typical monthly | Caveat |
|---|---|---|
| Datadog | $15/host + $0.10/1k spans + RUM per-session | Temps covers logs, metrics, traces, and RUM basics; deep APM/profiling is not matched — count 30–60% of the bill, say so |
| New Relic | usage (100GB free) | Same caveat |

## Temps Cost Side

| Item | Cost |
|---|---|
| Temps (software) | $0 — FSL-licensed, self-host free |
| Hetzner CX22 (2 vCPU / 4 GB) | ~€3.79/mo |
| Hetzner CPX22 (3 vCPU / 4 GB) — reference deployment | ~€8/mo |
| Hetzner CPX32 (4 vCPU / 8 GB) | ~€15/mo |
| Included traffic | 20 TB/mo (Hetzner) — compare against per-GB egress on Vercel/Netlify |
| Temps Cloud | server cost + 30%, from ~$6/mo |
| Seats / projects / environments / events / errors / recordings | unlimited, $0 |
