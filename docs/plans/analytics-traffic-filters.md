# Analytics traffic quality: datacenter exclusion, referrer and IP bans

Status: Phases 0–2 implemented on this branch (§11), except the "last 7 days" backfill; Phase 3 planned
Branch: `fix/analytics-quality-metrics`

## 1. Problem

Two symptoms in production analytics:

1. **Datacenter traffic is counted as real visitors.** Bursts of traffic from
   cloud/hosting ASNs (for example a run of Singapore-hosted IPs) show up in
   visitors, pageviews, referrers and countries.
2. **A spam referrer can take over the Referrers report.** In one reported
   case, an unknown domain accounted for 46% of referred visits in a day,
   more than the project's own site and Google combined. Nothing in the UI lets
   the user say "this isn't real, hide it".

### Root causes found in the code

| # | Finding | Where |
|---|---|---|
| A | Hosting detection needs `GeoLite2-ASN.mmdb`, but only `GeoLite2-City` is committed and auto-downloaded. The ASN file must be mounted by hand, so **on almost every install `is_hosting_provider` is `NULL`**. *Partly fixed on this branch for Docker images; see §5.* | `crates/temps-geo/src/refresh.rs:48-69`, `crates/temps-geo/src/geoip_service.rs:457-480`, `Dockerfile:248` |
| A2 | The bundled City database was built on **2025-08-01**, 15 months old. With no license key nothing refreshes it, so every install without a key geolocates on stale data. *Replaced on this branch; see §5.* | `crates/temps-cli/GeoLite2-City.mmdb` |
| B | Every read path uses `COALESCE(ig.is_hosting_provider, false) = false`, so `NULL` (no ASN database) means "keep". Combined with A, nothing is excluded. | `crates/temps-analytics/src/analytics.rs:3016`, `crates/temps-analytics-performance/src/services/service.rs:355` |
| C | Even with the ASN database, the hosting flag is only applied to **live visitors** and **speed/web vitals**. The main reports (visitors, pageviews, referrers, pages, countries, funnels), on both the Postgres and ClickHouse backends, filter only on `is_crawler`, which is set from the **User-Agent alone**. A scraper with a Chrome UA on a Tencent/Alibaba IP is counted. | `crates/temps-analytics-events/src/services/events_service.rs:1558`, `clickhouse_backend.rs` (`is_crawler = 0`) |
| D | Classification is a substring match on the ASN **org name**. It misses large clouds whose org name has no keyword (for example Tencent's ASN org is a street address), and over-matches: `"cloud"` matches **Cloudflare**, which carries iCloud Private Relay and WARP. Those are real Safari/mobile users. | `crates/temps-geo/src/geoip_service.rs:37-80` |
| E | Only the ASN org name is stored, not the ASN number. Rules can't target a network precisely. | `crates/temps-entities/src/ip_geolocations.rs:24-28` |
| F | There is no referrer spam list, and no per-project user rules for referrers or IPs. `deployment_config.blacklist_ips` blocks proxy traffic, not analytics. | — |

Root cause D, checked against the new ASN database. Public cloud addresses
were run through today's name heuristic (`HOSTING_ORG_PATTERNS`):

| Sample IP (public cloud range) | ASN | ASN org name | Heuristic says |
|---|---|---|---|
| 43.134.0.1 / 43.156.0.1 | AS132203 | Tencent Building, Kejizhongyi Avenue | not hosting ❌ |
| 47.74.0.1 / 8.219.0.1 | AS45102 | Alibaba (US) Technology Co., Ltd. | not hosting ❌ |
| 34.87.0.1 | AS396982 | Google LLC | not hosting ❌ |
| 119.13.64.1 | AS136907 | HUAWEI CLOUDS | hosting ✅ |
| 52.95.110.1 | AS16509 | Amazon.com, Inc. | hosting ✅ |
| 165.22.0.1 | AS14061 | DigitalOcean, LLC | hosting ✅ |
| 1.1.1.1 / 104.16.0.1 | AS13335 | Cloudflare, Inc. | hosting ❌ (also carries Private Relay/WARP users) |

Tencent, Alibaba and Google Cloud are exactly the networks behind typical
Singapore-hosted bot traffic, and all three are missed. Classifying by ASN
**number** (§4.1) fixes all of these rows.

The good news: `is_crawler` + `crawler_name` already flow to every Postgres
query and to the ClickHouse replica (`ch_fanout.rs:440-446`). If we **flag
excluded traffic at ingest with the existing `is_crawler` column**, every
report on both backends excludes it with no query changes.

## 2. Goals

- Datacenter/cloud traffic is excluded from all analytics **by default**, on
  every install, with no manual file mounting.
- A user can ban a **referrer**, an **IP / CIDR range** or a **network (ASN)**
  in one click from the report where they spot it, and undo it from one
  settings card.
- Users can see what was filtered and why, so they can trust it. Plausible
  doesn't offer this.
- Ingest stays hot-path safe: no extra DB or network calls per event.

Non-goals (v1): behavioural/ML bot scoring, CAPTCHA, blocking requests at the
proxy (this is about **analytics** accuracy, not serving traffic).

## 3. Temps vs Plausible

| Capability | Plausible | Temps today | Temps after this plan |
|---|---|---|---|
| UA bot filtering | Yes, automatic | Yes (`BOT_UA_PATTERNS` + woothee) | Same |
| Datacenter IP exclusion | Yes, automatic: ~40,800 datacenter IP ranges | Effectively **no** (root causes A–C) | **Yes, automatic**, by ASN number + name heuristic, on by default, with a toggle to turn it off |
| VPN / Private Relay handling | Most VPN/Private Relay visits are kept and shown as "Anonymous VPN Service". Some fall in DC ranges and are dropped | Would drop Cloudflare-routed Private Relay if the flag were applied (D) | Explicit allowlist for Private Relay/WARP egress networks |
| Referrer spam list | Yes, automatic (known spam domains) | No | Yes, bundled public referrer-spam list |
| Unnatural-pattern detection | Yes ("own algorithm", undisclosed) | No | Phase 3: "looks like spam" hint on a referrer, user confirms |
| User IP block | Shields: exact IPs only, **no ranges, max 30** | No (analytics) | Exact IP **and CIDR**, max 500 |
| User country block | Shields: max 30 | No | Phase 2 (cheap, data is already resolved) |
| User page block | Shields: wildcards, max 30 | No | Out of scope v1 |
| Hostname allowlist | Shields: max 30 | Partly (site hostname resolution) | Out of scope v1 |
| **User referrer block** | **No** (only the automatic list) | No | **Yes**, exact host or `*.domain`, max 500 |
| **User ASN / network block** | **No** | No | **Yes** |
| What happens to blocked hits | Dropped at ingest, never stored, not visible | n/a | **Stored and flagged**, so they're excluded from every report but visible in a "Filtered traffic" panel and reversible |
| Visibility of filtered volume | None per site (network-wide numbers only) | None | Per-project counts by reason |
| Retroactive | No (drop is forward-only) | n/a | New traffic, plus optionally the last 7 days (see §7) |
| Default geo data | Country-level DB-IP database shipped in the image | City-level GeoLite2 committed to the repo and shipped in the image | Same, plus the ASN database (§5) |
| City-level geo | Opt-in: a MaxMind license key (env var) makes it download and keep GeoLite2 up to date | Default (bundled). Kept up to date only with a license key (encrypted settings row) | Same, for City **and** ASN |
| ASN data | None in the open-source geo code | Optional, mounted by hand | Bundled + refreshed with City |

Summary: Plausible's main advantage is **automatic** datacenter + spam-referrer
filtering that works out of the box. Fixing A–D closes that gap. We can then
do better on **control** (referrer and ASN bans, CIDR, higher limits) and on
**transparency** (see what was filtered, undo).

Sources: [Plausible: Excluding traffic](https://plausible.io/docs/excluding),
[Plausible: Bot traffic filtering](https://plausible.io/docs/bot-traffic-filtering).
I didn't confirm whether Plausible Community Edition ships the same datacenter
list; treat that as unknown.

## 4. Proposal

### 4.1 Make datacenter detection actually work (fixes A, D, E)

1. **Ship ASN data like City data.** Done for Docker images on this branch.
   The full delivery redesign (binary installs, refresh, freshness) is §5.
2. **Store the ASN number.** Migration: add `ip_geolocations.asn_number integer NULL`.
   `refresh_if_stale` already rewrites rows whose resolved values changed
   (`ip_address_service.rs:420-440`), so existing rows backfill naturally.
3. **Classify by ASN number first, name second.**
   - A curated `DATACENTER_ASNS: &[u32]` list compiled into `temps-geo` (AWS,
     GCP, Azure, Oracle, Alibaba, Tencent, Huawei Cloud, ByteDance/BytePlus,
     DigitalOcean, Linode/Akamai Cloud, Vultr/Choopa, OVH, Hetzner, Contabo,
     Scaleway, Leaseweb, M247, and so on). This catches the clouds whose org
     names carry no keyword.
   - Keep the name heuristic as a fallback, but remove over-broad patterns
     (`"cloud"`, `"server"`) in favour of explicit ones (`"google cloud"`,
     `"alibaba cloud"`).
   - A `PRIVACY_RELAY_ASNS` allowlist (Cloudflare 13335, Akamai 36183, Fastly
     54113) that is **never** classified as hosting, so Private Relay and WARP
     users stay counted. Apple publishes Private Relay egress ranges; using
     that list is a possible refinement.
4. **Surface the data-source state.** The existing geo settings/status
   endpoint gains `asn_database: { loaded, build_epoch, source }`. If it isn't
   loaded, the analytics filters card shows "Datacenter detection is off:
   the ASN database isn't installed" with a link to the geo settings. This
   follows the onboarding rule: never silently report "0 filtered".

### 4.2 Exclude datacenter traffic from every report (fixes B, C)

At ingest (`events_service.rs` `record_event`), right where `is_crawler` is
computed from the UA, also mark the event excluded when the resolved IP is
hosting and the project has `analytics_exclude_datacenter = true` (new column
on `projects`, **default `true`**).

The handler already has the `IpAddressInfo` from `get_or_create_ip` (cached
in memory). Pass `is_hosting_provider` / `asn_number` into `record_event`
instead of only `ip_geolocation_id`. Zero extra lookups.

Because the result goes into `is_crawler`, every existing Postgres and
ClickHouse query excludes it automatically. The ad-hoc
`COALESCE(ig.is_hosting_provider, false)` joins in live visitors and speed can
stay (harmless) or be simplified later.

**Exclusion reason.** Record why in `crawler_name` using a closed vocabulary
so cardinality stays bounded:

| `crawler_name` | Meaning |
|---|---|
| `<bot name>` | Existing UA match (unchanged) |
| `datacenter` | Hosting/cloud ASN |
| `spam-referrer` | Matched the bundled spam list |
| `rule:referrer` | Matched a user referrer ban |
| `rule:ip` | Matched a user IP/CIDR ban |
| `rule:asn` | Matched a user network ban |
| `rule:country` | Matched a user country ban (phase 2) |

This avoids a schema change on the Postgres `events` table and the ClickHouse
replica. If we later want a proper column, it's a mechanical follow-up.

The visitor row keeps the existing "escalate only" rule
(`events_service.rs:1599`) so live visitors exclude them too.

### 4.3 User rules: referrers, IPs, networks

New table `analytics_traffic_filters`:

```
id            serial PK
project_id    int FK projects ON DELETE CASCADE
kind          text   -- 'referrer' | 'ip' | 'asn' | 'country' (phase 2)
value         text   -- normalised: lowercase host / '*.host', IP or CIDR, ASN as digits, ISO code
note          text NULL
created_by    int FK users NULL
created_at    timestamptz
UNIQUE (project_id, kind, value)
```

- Limits: 500 rules per kind per project (Plausible allows 30). The bound
  keeps the in-memory matcher small.
- Validation: referrer → a hostname (scheme/path stripped), `*.` prefix
  allowed; ip → an `IpAddr` or an `IpNet` (the `ipnet` crate is already in
  the workspace), with CIDRs broader than /8 (v4) or /32 (v6) rejected;
  asn → u32.
- Endpoints (`temps-analytics-events` handlers, `AnalyticsWrite` to write,
  `AnalyticsRead` to read, all writes audit-logged):
  - `GET    /projects/{project_id}/analytics/traffic-filters`: rules, plus
    the datacenter toggle, plus ASN-database status
  - `POST   /projects/{project_id}/analytics/traffic-filters`
  - `DELETE /projects/{project_id}/analytics/traffic-filters/{filter_id}`
  - `PATCH  /projects/{project_id}/analytics/settings` `{ exclude_datacenter }`
  - `GET    /projects/{project_id}/analytics/filtered-traffic?days=7`: counts
    grouped by `crawler_name` reason

### 4.4 Bundled referrer spam list

Embed a public, permissively licensed referrer-spam domain list (the
Matomo-maintained `referrer-spam-list`, ~2.3k domains) with `include_str!`
into a `HashSet` built once. Match on registrable-domain suffix. Refresh by
updating the file in the repo. No runtime download, so there's nothing to
configure and nothing to fail.

### 4.5 UX: simple, at the point of need

1. **Inline "Exclude" actions** (the main entry point):
   - Referrers card / referrers detail: row menu → **"Exclude this referrer"**.
     A confirm dialog shows the domain, the visit count it would remove, and a
     checkbox "Also hide the last 7 days".
   - Visitor detail / live visitors: **"Exclude this IP"** and, when the ASN is
     known, **"Exclude network AS{n} ({org})"**.
   - Countries report (phase 2): **"Exclude this country"**.
2. **Settings → Analytics → "Traffic filters" card** (one place to review and undo):
   - Toggle **"Exclude datacenter & cloud traffic"** (on by default), with
     its status ("ASN database loaded, updated Oct 2026" or the onboarding
     message from §4.1).
   - Three compact lists (Referrers, IPs & ranges, Networks) with add/remove.
   - **"Filtered in the last 7 days"**: e.g. `1,240 datacenter · 285 excluded
     referrer · 42 bots`, which links to a view of the filtered referrers and
     networks so the user can spot false positives.
3. **Phase 3, "looks like spam" hint.** On the Referrers card, flag a referrer
   when, over the selected window, it has ≥ N visits and ≥ 95% bounce with
   ~0s duration, or ≥ 80% of its visits come from one hosting ASN. Show a
   small badge, "Looks automated", with a one-click Exclude. Nothing is ever
   excluded silently by a heuristic.

## 5. Delivering the geo databases (City + ASN)

### 5.1 How Temps delivers them today

| Path | City | ASN |
|---|---|---|
| Repository | `crates/temps-cli/GeoLite2-City.mmdb`, committed past a `GeoLite2-*.mmdb` ignore rule | Not in the repo |
| Docker images (`Dockerfile`, `Dockerfile.release`) | Copied to `/usr/share/temps/`, symlinked into `/app` (the working directory) | Not shipped. The comments say "mount it under `/app/data`" |
| Binary install, first start | `ensure_mmdb_present` downloads the committed file from `raw.githubusercontent.com/.../main/...` into the data dir | Nothing |
| Ongoing refresh | Only with a MaxMind license key (stored encrypted in settings; interval configurable). Without a key there is deliberately no recurring fetch, and the status endpoint says so | Nothing |
| Hot reload | A file watcher re-opens the database within 60s of the file changing | Loaded once at startup |
| Validation | Size bounds + parses as `.mmdb` | Nothing (optional) |

Problems with this:

- **No key means stale data forever.** The committed City file was 15 months
  old. Most self-hosters never add a key.
- **ASN is invisible.** It isn't shipped, refreshed or reported, so datacenter
  detection is silently off (root cause A).
- **A mounted file is masked.** `resolve_mmdb_path` checks the working
  directory before the data dir (`geoip_service.rs:332-346`). In Docker,
  `/app/GeoLite2-City.mmdb` always exists (the symlink), so a newer file an
  operator mounts at `/app/data/...`, as the compose comments suggest, is
  never used.
- **Repo bloat.** Every refresh of a committed ~50 MB binary stays in git
  history forever and makes every clone heavier.
- **No type check.** Nothing verifies that a file named `GeoLite2-ASN.mmdb`
  is actually an ASN database.

### 5.2 How Plausible delivers them

- **Community Edition default:** a **country-level DB-IP database** shipped
  inside the container image (`IP_GEOLOCATION_DB` overrides the path). Data is
  as fresh as the image, so upgrading Plausible refreshes it.
- **City-level:** opt-in. Setting `MAXMIND_LICENSE_KEY` (and optionally
  `MAXMIND_EDITION`, default `GeoLite2-City`) makes the `locus` library
  download the database from MaxMind, cache it on disk and keep it up to date.
  Configuration is by environment variable and needs a restart.
- **ASN:** the open-source geo module loads a single database (City or
  Country). There's no ASN database in it.
- **Datacenter/threat/VPN detection:** the ingest pipeline drops events whose
  request carries an IP classification of `dc_ip` or `threat_ip`, and labels
  `anonymous_vpn_ip` as country `A1`. I didn't verify where that
  classification is computed. It isn't the geo database, so I wouldn't
  assume Community Edition gets datacenter filtering.
- **Referrer spam / bots:** a bundled referrer blocklist library and
  `UAInspector`, checked at ingest.

So Plausible's self-hosted default is **less** than Temps': country-only data,
no ASN, refreshed by upgrading. Temps' gap is freshness and the missing ASN
path, not the approach.

### 5.3 What this branch already does (stopgap)

- Adds `crates/temps-cli/GeoLite2-ASN.mmdb` (GeoLite2-ASN, built
  2026-10-20) and replaces `GeoLite2-City.mmdb` with a current build
  (2026-10-16, previously 2025-08-01). Both were taken from another
  open-source analytics project's public repository. The blob hashes match
  GitHub's, and the `.mmdb` metadata was checked (`database_type`,
  `build_epoch`).
- Both Dockerfiles copy and symlink the ASN file exactly like City, so every
  Docker install gets datacenter detection without mounting anything.
- `.gitignore` allows only these two committed copies. Local copies at the
  repo root stay ignored.
- `NOTICE` covers both databases under the MaxMind GeoLite EULA.
- Binary installs: on first start, `temps serve` downloads the committed ASN
  copy into the data dir when it isn't present, the same way it already does
  for City. A failure is logged and never blocks startup.

**Decision: keep delivering both databases from the repository for now**,
the way other self-hosted analytics projects do. Item 4 below (release
assets) is deferred, not dropped. The ASN file still isn't refreshed on a
schedule.

### 5.4 Target design

1. **One code path for both databases.** Generalise `refresh.rs` around a
   `GeoDb { City, Asn }` descriptor (filename, MaxMind `edition_id`,
   expected `database_type`s, size bounds, required vs optional).
   `ensure_mmdb_present`, the scheduled refresh, the file watcher and the
   status endpoint all loop over both. ASN failures are non-fatal and
   reported. City failures keep today's behaviour.
2. **Check the type, not just the size.** After download and on load, check
   `metadata.database_type`: City accepts `GeoLite2-City`, `GeoIP2-City`,
   `DBIP-City-Lite`; ASN accepts `GeoLite2-ASN`, `GeoIP2-ISP`, `DBIP-ASN-Lite`.
   A wrong file is rejected with a message naming the file and the type it
   contained.
3. **Fix path precedence.** Resolve data dir → working directory → image
   bundle (`/usr/share/temps`), and use the data-dir copy only if it's valid
   and at least as new (`build_epoch`) as the bundled one. Refresh downloads
   always go to the data dir and never overwrite the image copy.
4. **Freshness without a key.** Stop committing the binaries to git:
   - A scheduled CI workflow (monthly) downloads City + ASN from MaxMind with
     the project's own license key, checks them, and publishes them as
     assets of a dated `geo-data-YYYY-MM-DD` GitHub release with SHA-256 sums.
   - It opens a PR bumping a small committed manifest, e.g.
     `crates/temps-geo/geo-data.json`
     `{ city: {url, sha256, build_epoch}, asn: {…} }`.
   - Docker builds fetch the pinned assets and verify the SHA-256. Every
     Temps release then ships data at most about a month old, like Plausible
     CE's image-refresh model but monthly.
   - `ensure_mmdb_present` on binary installs downloads the pinned assets
     and verifies the hash. That's stronger than today's size check, and
     there's still no recurring fetch for unlicensed installs.
   - With a license key: unchanged, both databases refresh directly from
     MaxMind on the configured interval.
5. **Make staleness visible.** The status endpoint reports per database:
   `loaded`, `source`, `build_epoch`, `age_days`. The geo settings page warns
   when the data is over 90 days old and points to the license key setting.
   `temps doctor` checks both databases.
6. **Optional key-free source.** Offer DB-IP Lite (City Lite + ASN Lite,
   CC BY 4.0, monthly, no account) as a refresh source for operators who
   don't want a MaxMind account. It needs an attribution line in the UI where
   geo data is shown.

### 5.5 Rollout

| Step | Scope |
|---|---|
| Done (this branch) | Commit fresh City + ASN, ship ASN in Docker images, first-start ASN download for binary installs, `database_type` check on downloads (item 2), ASN status on `/geo/status` (part of item 5), NOTICE, `.gitignore` |
| Next | Rest of items 1, 3 and 5: scheduled ASN refresh with a license key, hot-reload of the ASN file, path precedence, `temps doctor` |
| Phase 2 | Item 4: release-asset delivery + manifest + monthly workflow, then remove the committed binaries (repo stays usable, history keeps the old blobs) |
| Later | Item 6: DB-IP Lite source |

## 6. Ingest hot-path design

Ingest is per-event, so it follows the hot-path rules:

- A `TrafficFilterCache` (in `temps-analytics-events`) holds
  `project_id → Arc<CompiledRules>` in a `moka` cache with a 30s TTL, and is
  invalidated on rule writes. A miss is one indexed query per project per 30s,
  not per event. `CompiledRules` = `HashSet<IpAddr>`, `Vec<IpNet>` (≤ 500),
  `HashSet<String>` for referrer hosts (suffix walk over labels: O(labels)),
  `HashSet<u32>` for ASNs, plus `exclude_datacenter: bool`.
- The spam list is a static `LazyLock<HashSet<&'static str>>`.
- Geo/ASN is already resolved and memory-cached per IP, so there's no new I/O.
- Memory bound: ~500 × 4 kinds × small strings per active project, which is
  well under 1 MB for hundreds of projects.
- Rule propagation in split topology: the TTL bounds staleness to 30s on any
  node, with no cross-node coordination needed.

## 7. Retroactivity

Flagging at ingest means rules apply to **new** traffic. Users notice spam
after the fact, so the confirm dialog offers **"Also hide the last 7 days"**:

- Postgres: a background job runs a bounded, chunked `UPDATE events SET
  is_crawler = true, crawler_name = 'rule:…'` for the project over the last 7
  days. `events` chunks compress after 7 days
  (`m20250101_000001_initial_schema.rs:3229`), so 7 days is the window where
  updates are cheap. Going further back would mean decompressing chunks.
- ClickHouse: a matching `ALTER TABLE … UPDATE` mutation scoped to the
  project and date partitions, or re-fanning the updated rows. **Needs a
  decision** (see §9).
- Deleting a rule does not un-flag past rows in v1. The dialog says so.

## 8. Phasing

| Phase | Scope | Fixes |
|---|---|---|
| **0. Stopgap** ✅ | Fresh City + ASN committed, ASN shipped in Docker images and downloaded on first start (§5.3) | A, A2 |
| **1. Datacenter by default** ✅ | `asn_number` + ASN-number classification + relay allowlist; `project_analytics_settings.exclude_datacenter_traffic` (default true); flag at ingest; status in API; toggle + status in UI | A–E, symptom 1 |
| **2. User rules** ✅ (except backfill) | `analytics_traffic_filters` table, cache, API, audit; inline "Exclude" on referrers/visitors; settings card; filtered-traffic counts; bundled spam list; country rule. The "last 7 days" backfill is deferred until §9.4 (ClickHouse) is decided | F, symptom 2 |
| **3. Hints** | "Looks automated" referrer badge; ClickHouse backfill if not done in 2; consider dedicated `exclusion_reason` column | Polish |

Phase 1 alone fixes the Singapore-style flood. Phase 2 fixes the referrer
in the screenshot.

## 9. Decisions needed

1. **Flag-and-store vs drop.** I recommend flagging, which is reversible,
   auditable and visible. Cost: excluded traffic still uses storage. Should a
   user rule optionally **drop** (Plausible-style) for heavy floods?
2. **Geo data delivery.** Move from committed binaries to release assets
   plus a pinned manifest (§5.4 item 4)? That needs a MaxMind license key held
   as a CI secret. A maintainer should also confirm that the current GeoLite
   EULA allows redistribution in a public repo and images. We already do it
   for City, and this branch extends it to ASN.
3. **Default for existing projects.** Turn `exclude_datacenter` on for
   existing projects too (numbers will drop after upgrade, so a changelog note
   is needed), or only for new projects?
4. **ClickHouse backfill** for "last 7 days": a mutation (simple, heavy on big
   tables) or re-fan rows from Postgres (more code, cheap)?
5. **Corporate egress false positives.** Some real users browse through
   Microsoft/Google/Zscaler egress on cloud ASNs. Accept and document (the
   toggle is the escape hatch), or keep those ASNs out of the default list?

## 10. Testing

- `temps-geo`: classification by number, name fallback, relay allowlist
  (Cloudflare never hosting), `NULL` when the ASN database is missing. The
  §1 sample table becomes a regression test against the bundled ASN file.
- Geo delivery: `database_type` check rejects a City file named as ASN (and
  vice versa); path precedence prefers a valid, newer data-dir copy over the
  image bundle; SHA-256 mismatch on a pinned asset is rejected; status reports
  both databases with `age_days`.
- Ingest: datacenter IP + Chrome UA → `is_crawler = true`,
  `crawler_name = "datacenter"`. The same with the toggle off → counted.
  Referrer/IP/CIDR/ASN rule hits and misses, `*.domain` suffix matching,
  spam-list hit.
- Cache: a rule write invalidates the cache, and the TTL bounds staleness.
- Handlers: 401/403/400 (bad CIDR, too-broad CIDR, limit reached)/409
  duplicate/201; audit entries on create/delete/toggle.
- Read path: an existing referrers/visitors query excludes flagged rows on
  Postgres and ClickHouse (integration tests, which skip without Docker).
- Frontend: Exclude dialog, settings card onboarding state when the ASN
  database is missing.

## 11. Implementation status (this branch)

**Geo (`temps-geo`)**
- `GeoLocation` / `IpAddressInfo` carry `asn_number`. `classify_hosting()`
  checks the privacy-relay allowlist (by ASN, plus the org name "cloudflare"),
  then a curated `DATACENTER_ASNS` list, then the existing name heuristic.
  Every ASN in the list was checked against the bundled GeoLite2-ASN file.
- `ip_geolocations` gains `asn_number` and `hosting_classifier_version`
  (migration `m20261104_000001`). A cached IP row from an older classifier,
  or from before an ASN database was loaded, is re-resolved on its next
  lookup instead of waiting for the 30-day staleness window.
- `refresh.rs` has a `GeoDatabaseKind { City, Asn }`. Downloads are checked
  against the file's `database_type`. `temps serve` fetches the committed ASN
  copy on first start when it's missing (non-fatal).
- `/geo/status` reports `asn_database_loaded` and `asn_build_time`.

**Analytics (`temps-analytics-events`)**
- New table `project_analytics_settings` (migration `m20261104_000002`). The
  row is optional and defaults to exclusion on, so existing projects are
  protected after upgrade without a backfill. It's a separate table so the
  Phase 2 rules have a home.
- `ProjectAnalyticsSettingsService`: a 30s moka cache for ingest, shared
  with the API so a write invalidates the cache ingest reads.
- Ingest: a hosting IP with a real browser UA gets `is_crawler = true`,
  `crawler_name = "datacenter"`, unless the project opted out. Recognised
  bots keep their own name. An unknown verdict (no ASN database) never
  excludes. The hosting verdict comes from the same memory-cached geo lookup,
  so there's no new I/O per event.
- `GET`/`PATCH /projects/{project_id}/analytics/settings` (`AnalyticsRead` /
  `AnalyticsWrite`, audit `PROJECT_ANALYTICS_SETTINGS_UPDATED`). The response
  includes `datacenter_detection { available, reason, setup_path }` for the
  onboarding state.

**Phase 2: user rules and referrer spam (`temps-analytics-events`)**
- New table `analytics_traffic_filters` (migration `m20261104_000003`):
  `referrer`, `ip`, `asn` and `country` rules, unique per project/kind/value,
  at most 500 per kind. Values are normalised server-side: a pasted URL becomes
  its host (`www.` and `*.` stripped; a referrer rule covers subdomains), IPs
  and CIDRs are canonicalised with host bits cleared and ranges broader than
  /8 (IPv4) or /32 (IPv6) rejected, `AS132203` becomes `132203`, country codes
  are uppercased.
- `TrafficFilterService` compiles a project's rules into hash sets (plus a
  short CIDR list) cached for 30s and invalidated on write, so ingest does one
  in-memory lookup per event and no per-event query. A load error counts the
  event unfiltered and logs.
- The bundled Matomo referrer-spam list (public domain, ~2.3k domains,
  `data/referrer-spam-list.txt`, see `NOTICE`) is compiled in and matched by
  whole-label suffix. It is controlled by
  `project_analytics_settings.exclude_spam_referrers` (default on).
- Ingest order: user-agent bot (keeps its name), then the project's rules
  (`rule:<kind>`), then the spam list (`spam-referrer`), then datacenter
  (`datacenter`). Everything goes through `is_crawler`, so no read query
  changed. The ingest handler passes a `ClientNetwork` (IP, ASN, country,
  hosting verdict) from the lookup it already did.
- Endpoints (`AnalyticsRead` / `AnalyticsWrite`, writes audited as
  `ANALYTICS_TRAFFIC_FILTER_CREATED` / `_DELETED`):
  `GET`/`POST /projects/{id}/analytics/traffic-filters` (paginated, filter by
  kind), `DELETE /projects/{id}/analytics/traffic-filters/{filter_id}`,
  `GET /projects/{id}/analytics/filtered-traffic?days=7` (1–30 days; counted
  events plus filtered events by reason). The settings endpoint gained
  `exclude_spam_referrers` and `spam_referrer_list_size`.
- Visitor details now include `asn_number` and `asn_org`.

**Console**
- Analytics → **Traffic filters** (`analytics/settings`): "Filtered in the
  last 7 days" summary, the datacenter toggle with detection status (and an
  onboarding alert linking to the geolocation settings when no ASN database is
  loaded), the referrer-spam toggle, and **Your filters** (add form, list
  filterable by type, paginated, remove with confirmation).
- Inline **Exclude from analytics**: on a referrer's detail view in the
  Referrers card, and on a visitor's page (this IP, network, first referrer,
  country). The visitor's network is shown on the Location card.
- Visitor lists and details translate reason codes (`datacenter`,
  `rule:country`, ...) into readable labels.

**Decision 3 (§9) as implemented:** exclusion is on for existing projects
too. Visitor numbers will drop after upgrade on projects that receive cloud
traffic, so the release note needs to say so.

**Not in this branch:** retroactive cleanup ("also hide the last 7 days",
blocked on §9.4), the Phase 3 "looks automated" hint, scheduled ASN refresh,
and hot-reload of the ASN file. The ASN reader is opened once at startup;
replacing `GeoLite2-ASN.mmdb` on disk takes effect after a restart (the City
file is still watched and swapped live).

**Hardening from review:**

- Consumer VPN egress networks (M247, Datacamp/CDN77, Clouvider, Zenlayer)
  are never classified as hosting, like the privacy relays: exclusion is on
  by default and must not drop people browsing through a commercial VPN.
- When the City database has no record for an address but the ASN database
  does, the network is still returned, so datacenter exclusion and ASN rules
  work on freshly announced cloud ranges.
- A stored address that neither database knows is stamped "not hosting"
  under the current classifier version instead of being re-resolved on every
  cache miss.
- The ingest caches for settings and rules coalesce concurrent misses into
  one query, and a load that raced a write is served but not cached, so a
  change is not undone for a TTL by a read already in flight.
- `PATCH` of the settings takes a row lock, writes only the fields it names
  (two people toggling different switches both win), and audits the values
  read under that lock.
- A filter match excludes that one event but never flags an existing human
  visitor as a crawler (only a user-agent bot does), so a real visitor who
  once arrives over a VPN or from an excluded referrer keeps showing up.

