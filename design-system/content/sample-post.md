---
title: Track AI crawler traffic at the proxy
lede: Assistants read acme.sh far more often than people do. The proxy already sees every one of those requests, so the question is not how to collect the data — it is which four fields to keep.
author: maya
date: 2026-09-01
readingMinutes: 6
---

Six weeks ago the bandwidth line on acme.sh went up by a third and nothing
about the product had changed. No campaign, no release, no press. The
analytics page was flat: same sessions, same countries, same pages. The proxy
log said something else, and it said it in the user agent.

Assistants now read the site more often than people do. That is not a
complaint — a page that gets quoted in an answer is a page doing its job — but
it is traffic somebody pays for, and until you can name it you cannot decide
anything about it. This is how we ended up counting it at the proxy, what the
four fields are, and what we changed once we could see them.

## What the proxy already knows

Every request through Temps' proxy is written to `proxy_logs` with the host,
the path, the status, the bytes out and the user agent. Nothing extra has to
be installed and no script has to run in a browser — which matters here,
because a crawler does not execute the analytics snippet. That is exactly why
the analytics page was flat while the bill was not.

A crawler is identified by its user agent and, for the ones that publish them,
by the address it comes from. The user agent is enough to start:

```bash
bunx @temps-sdk/cli logs query \
  --project acme-storefront \
  --range 30d \
  --filter 'ua:~"GPTBot|ClaudeBot|PerplexityBot|Google-Extended"' \
  --group-by ua --format table
```

That is the whole discovery step. Thirty days, grouped by agent, and the
answer arrives as a table you can paste into an issue.

### The four fields worth keeping

We tried keeping more and it made the question harder to answer, not easier.
What survived:

1. **Agent** — the crawler's own name, taken verbatim from the user agent and
   never re-cased. `GPTBot` is what it calls itself.
2. **Path** — which pages get read. This is the one that changes what you
   write next.
3. **Status** — a crawler collecting `404`s for a month is a sitemap problem,
   not a crawler problem.
4. **Bytes out** — the number the bill is made of.

Everything else (headers, referrers, timing percentiles) turned out to be
detail we looked at once and never again.

## Counting it

The rule is one line of matching against the user agent, and it belongs in
version control rather than in a dashboard somebody configured by hand:

```ts
// acme-storefront/src/crawlers.ts
const AGENTS = ['GPTBot', 'ClaudeBot', 'PerplexityBot', 'Google-Extended', 'CCBot'] as const

export type Crawler = (typeof AGENTS)[number]

/** The crawler behind a user agent, or null for a browser. Verbatim, never re-cased. */
export function crawlerOf(ua: string): Crawler | null {
  return AGENTS.find((a) => ua.includes(a)) ?? null
}
```

Point the logs screen at the result and the shape of the month is visible in
one pass: which agent, which paths, and where the bytes went.

```figure
{
  "src": "/figures/logs-light.png",
  "dark": "/figures/logs-dark.png",
  "alt": "The Temps logs screen filtered to crawler user agents: a volume-by-level chart above a list of request lines, with facets for project, environment and source down the right.",
  "caption": "the logs screen, filtered to crawler agents · a token for the filter, a facet for the share",
  "width": 1440,
  "height": 900
}
```

The facets on the right are the part that matters. Each one adds a token to
the query, the token stays in the URL, and the URL is what you paste into the
issue. Nothing narrows the list by a state the reader cannot see or remove.

Over thirty days the volume splits like this:

```chart
{
  "title": "crawler requests \u00b7 acme.sh",
  "range": "30d",
  "verdict": "GPTBot doubled after the docs moved on Aug 12; every other agent is flat.",
  "unit": "req",
  "series": [
    {
      "key": "gptbot",
      "name": "GPTBot"
    },
    {
      "key": "other",
      "name": "every other agent"
    }
  ],
  "markers": [
    {
      "id": "dep_88c",
      "x": "08-12",
      "note": "docs moved"
    }
  ],
  "data": [
    {
      "t": "08-03",
      "gptbot": 3900,
      "other": 3400
    },
    {
      "t": "08-04",
      "gptbot": 4100,
      "other": 3550
    },
    {
      "t": "08-05",
      "gptbot": 3800,
      "other": 3300
    },
    {
      "t": "08-06",
      "gptbot": 4200,
      "other": 3600
    },
    {
      "t": "08-07",
      "gptbot": 4050,
      "other": 3480
    },
    {
      "t": "08-08",
      "gptbot": 3700,
      "other": 3200
    },
    {
      "t": "08-09",
      "gptbot": 3600,
      "other": 3150
    },
    {
      "t": "08-10",
      "gptbot": 4300,
      "other": 3700
    },
    {
      "t": "08-11",
      "gptbot": 4150,
      "other": 3520
    },
    {
      "t": "08-12",
      "gptbot": 3950,
      "other": 3400
    },
    {
      "t": "08-13",
      "gptbot": 4400,
      "other": 3650
    },
    {
      "t": "08-14",
      "gptbot": 4250,
      "other": 3580
    },
    {
      "t": "08-15",
      "gptbot": 7600,
      "other": 3600
    },
    {
      "t": "08-16",
      "gptbot": 8100,
      "other": 3720
    },
    {
      "t": "08-17",
      "gptbot": 7900,
      "other": 3500
    },
    {
      "t": "08-18",
      "gptbot": 8400,
      "other": 3800
    },
    {
      "t": "08-19",
      "gptbot": 8250,
      "other": 3660
    },
    {
      "t": "08-20",
      "gptbot": 7700,
      "other": 3300
    },
    {
      "t": "08-21",
      "gptbot": 7500,
      "other": 3250
    },
    {
      "t": "08-22",
      "gptbot": 8600,
      "other": 3900
    },
    {
      "t": "08-23",
      "gptbot": 8300,
      "other": 3700
    },
    {
      "t": "08-24",
      "gptbot": 8050,
      "other": 3560
    },
    {
      "t": "08-25",
      "gptbot": 8800,
      "other": 3850
    },
    {
      "t": "08-26",
      "gptbot": 8450,
      "other": 3620
    },
    {
      "t": "08-27",
      "gptbot": 8200,
      "other": 3480
    },
    {
      "t": "08-28",
      "gptbot": 8900,
      "other": 3950
    },
    {
      "t": "08-29",
      "gptbot": 8600,
      "other": 3760
    },
    {
      "t": "08-30",
      "gptbot": 8350,
      "other": 3600
    },
    {
      "t": "08-31",
      "gptbot": 9100,
      "other": 4000
    },
    {
      "t": "09-01",
      "gptbot": 8750,
      "other": 3820
    }
  ]
}
```

## What the numbers said

| agent | requests | bytes out | 404s |
| --- | ---: | ---: | ---: |
| GPTBot | 184,220 | 3.1 GB | 2,904 |
| ClaudeBot | 61,905 | 1.1 GB | 88 |
| PerplexityBot | 30,800 | 612 MB | 41 |
| CCBot | 9,410 | 184 MB | 6,220 |

Two things fell out of that table immediately.

The first is the `404` column. CCBot was spending two thirds of its requests
on paths that stopped existing when the docs moved, because it was working
from a sitemap that had not been regenerated since the move.[^sitemap] That is
our bug, not the crawler's, and it was a five-minute fix.

The second is that the traffic is concentrated: four paths accounted for more
than half of everything read. They are the four pages we would have picked as
the ones worth being quoted from, which was a relief, and they are also the
four pages that had been sitting at the bottom of the rewrite list.

> The crawler is not the audience. The person reading the answer that quotes
> you is the audience, and the crawler is how they get there.

That reframing is the reason we stopped treating this as a cost line.

## What we changed

- Regenerated the sitemap on every deploy rather than nightly, which took the
  `404` share from 3.4% to 0.1%.
- Split `robots.txt` so the pages behind the sign-in wall are refused
  explicitly instead of returning a redirect the crawler retries.
- Set a rate limit per agent — generous, and the same for all of them, so the
  rule is one sentence and not a table of exceptions.
- Rewrote the four most-read pages with the answer in the first paragraph.

Not everything landed. The open items, honestly:

- [x] Sitemap regenerated per deploy
- [x] `robots.txt` split by area
- [ ] Per-agent rate limits (written, not yet applied to production)
- [ ] Address-range verification for the agents that publish theirs

The last one is the interesting gap: a user agent is a claim, not proof, and
anything can send `GPTBot` in a header. Verifying against the published ranges
turns the count from an estimate into a fact, and it is the next thing on the
list.

```callout
{
  "state": "warn",
  "title": "A user agent is a claim",
  "body": "Until the address is checked against the crawler's published ranges, treat these counts as a floor, not a measurement. Anything can send `GPTBot` in a header."
}
```

---

## If you want to do this on your own site

Open the logs screen, press `kbd:/` to reach the query bar, and type one
token: `ua:~"GPTBot"`. If a number comes back, you already have the data and
the rest is deciding which four fields you keep. Press `kbd:e` on a line to
expand it and see the request the way the proxy saw it.

We kept agent, path, status and bytes. Six weeks in, no one has asked for a
fifth.

[^sitemap]: The docs moved from `docs.acme.sh/guide/*` to `acme.sh/docs/*` on
    Aug 12. The redirects were correct; the sitemap was the thing nobody
    regenerated.
