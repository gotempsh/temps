// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState, type CSSProperties, type ReactNode } from 'react'
import { cn } from '@/lib/utils'
import { Activity, Bot, Compass, ExternalLink, Globe, Link, Mail, Megaphone, Monitor, Search, Share2, Smartphone, Tablet, Tag, Zap } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  ChartFooter, Detail, Ledger, Lede, Live, Metric, MetricGrid, Num, Phrase, RangePicker, PageState, Section, Segmented, Columns, Status, StatusLine, GeoMap, EchoDialog, KeyValue, StatusStrip, TimeChart, Timeline,
  Breakdown, Sparkline, Funnel, Flow, type BreakdownRow, type KV, type LedgerRow, type State, type StatusBucket, type TimeRange,
} from '@/components/op'
import type { Notify, Plan } from './ConsoleV1Observe'
import { useFresh } from './console-fresh'

/* ────────────────────────────────────────────────────────────────────────
   Analytics and Uptime on v1, built on the observe primitives (viz.tsx).
   Shapes follow web: PropertyBreakdownResponse {items{value,count,percentage},
   total} per group_by with filter_country/filter_region for drill-down;
   PagePathsResponse + PagePathsSparklineResponse; FunnelMetricsResponse
   step_conversions; PageFlowResponse transitions; StatusBucketedResponse
   buckets; GroupedPageMetric p75 vitals.
   ──────────────────────────────────────────────────────────────────────── */

const RANGES = [{ label: '24h', days: 1 }, { label: '7d', days: 7 }, { label: '30d', days: 30 }, { label: '90d', days: 90 }] as const
const TOTAL = 12_418

const HOURLY = Array.from({ length: 48 }, (_, i) => {
  const t = `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`
  const wave = Math.max(0.15, Math.sin(((i / 2 - 5) / 24) * Math.PI * 2))
  const visitors = Math.round(120 + wave * 420 + (i % 7 === 0 ? 60 : 0))
  return { t, visitors, prev: Math.round(visitors * (0.85 + ((i * 7) % 10) / 40)) }
})

const LOCATIONS: BreakdownRow[] = [
  { label: 'United States', count: 4312, children: [
    { label: 'California', count: 1610, children: [{ label: 'San Francisco', count: 720 }, { label: 'Los Angeles', count: 480 }, { label: 'San Diego', count: 190 }] },
    { label: 'New York', count: 980, children: [{ label: 'New York', count: 910 }, { label: 'Buffalo', count: 70 }] },
    { label: 'Texas', count: 640 }, { label: 'Washington', count: 410 },
  ] },
  { label: 'Germany', count: 1820, children: [{ label: 'Berlin', count: 760 }, { label: 'Bavaria', count: 520 }, { label: 'Hamburg', count: 240 }] },
  { label: 'United Kingdom', count: 1404, children: [{ label: 'England', count: 1290 }, { label: 'Scotland', count: 90 }] },
  { label: 'Spain', count: 980, children: [{ label: 'Madrid', count: 520 }, { label: 'Catalonia', count: 330 }] },
  { label: 'France', count: 812 }, { label: 'Portugal', count: 611 }, { label: 'Netherlands', count: 590 }, { label: 'Canada', count: 512 }, { label: 'Brazil', count: 410 },
]
const BROWSERS: BreakdownRow[] = [
  { label: 'Chrome', count: 7120, children: [{ label: '128', count: 5210 }, { label: '127', count: 1480 }, { label: '≤126', count: 430 }] },
  { label: 'Safari', count: 3210, children: [{ label: '17.6', count: 2410 }, { label: '17.5', count: 610 }, { label: '≤17.4', count: 190 }] },
  { label: 'Firefox', count: 1340, children: [{ label: '130', count: 1180 }, { label: '≤129', count: 160 }] },
  { label: 'Edge', count: 520 }, { label: 'Samsung Internet', count: 148 }, { label: 'Other', count: 80 },
]
const CHANNELS: BreakdownRow[] = [
  { label: 'direct', count: 4810 },
  { label: 'organic search', count: 3920, children: [{ label: 'google.com', count: 3610 }, { label: 'bing.com', count: 210 }, { label: 'duckduckgo.com', count: 100 }] },
  { label: 'referral', count: 1980, children: [{ label: 'github.com', count: 940 }, { label: 'news.ycombinator.com', count: 610 }, { label: 'reddit.com', count: 430 }] },
  { label: 'social', count: 1210, children: [{ label: 'x.com', count: 720 }, { label: 'linkedin.com', count: 490 }] },
  { label: 'ai agents', count: 498, state: 'sampled', children: [{ label: 'ChatGPT-User', count: 312 }, { label: 'PerplexityBot', count: 121 }, { label: 'ClaudeBot', count: 65 }] },
]
const DEVICES: BreakdownRow[] = [{ label: 'desktop', count: 7940 }, { label: 'mobile', count: 4110 }, { label: 'tablet', count: 368 }]

type PagePath = { path: string; views: number; sessions: number; avg_s: number; bounce: number; spark: number[] }
const spark = (seed: number) => Array.from({ length: 24 }, (_, i) => Math.round(20 + Math.abs(Math.sin((i + seed) / 3.5)) * 60 + ((i * seed) % 7)))
const PAGES: PagePath[] = [
  { path: '/', views: 9812, sessions: 6210, avg_s: 34, bounce: 41, spark: spark(1) },
  { path: '/pricing', views: 4120, sessions: 3480, avg_s: 88, bounce: 22, spark: spark(4) },
  { path: '/docs/getting-started', views: 3310, sessions: 2610, avg_s: 142, bounce: 18, spark: spark(2) },
  { path: '/blog/self-hosted-vercel', views: 2890, sessions: 2710, avg_s: 201, bounce: 61, spark: spark(9) },
  { path: '/download', views: 1980, sessions: 1820, avg_s: 26, bounce: 12, spark: spark(6) },
  { path: '/docs/deploy/compose', views: 1410, sessions: 1190, avg_s: 168, bounce: 20, spark: spark(3) },
  { path: '/roadmap', views: 1210, sessions: 1140, avg_s: 96, bounce: 35, spark: spark(7) },
  { path: '/changelog', views: 880, sessions: 810, avg_s: 58, bounce: 44, spark: spark(5) },
  { path: '/compare/coolify', views: 720, sessions: 690, avg_s: 132, bounce: 28, spark: spark(8) },
  { path: '/login', views: 610, sessions: 590, avg_s: 12, bounce: 9, spark: spark(10) },
]

const FUNNEL = [
  { name: 'Viewed /pricing', count: 3480, avgSeconds: 0 },
  { name: 'Clicked "Start free"', count: 1240, avgSeconds: 48 },
  { name: 'Created account', count: 910, avgSeconds: 95 },
  { name: 'Connected a repository', count: 402, avgSeconds: 310 },
  { name: 'First deploy succeeded', count: 318, avgSeconds: 640 },
]
// Every funnel the project has defined. The list on the right switches the one on the left:
// a name on a page is either wired or it is text (brand §6, "a drawn control is a wired one").
const FUNNELS: { id: string; name: string; steps: typeof FUNNEL }[] = [
  { id: 'f1', name: 'Pricing → first deploy', steps: FUNNEL },
  { id: 'f2', name: 'Docs → download', steps: [
    { name: 'Viewed /docs/getting-started', count: 4120, avgSeconds: 0 },
    { name: 'Reached /docs/install', count: 1980, avgSeconds: 72 },
    { name: 'Clicked "Download"', count: 750, avgSeconds: 41 },
  ] },
  { id: 'f3', name: 'Blog → pricing', steps: [
    { name: 'Read a blog post', count: 2960, avgSeconds: 0 },
    { name: 'Viewed /pricing', count: 620, avgSeconds: 128 },
    { name: 'Created account', count: 278, avgSeconds: 96 },
  ] },
]
const completed = (steps: typeof FUNNEL) => (steps[steps.length - 1].count / steps[0].count) * 100
const TRANSITIONS = [
  { from: '/', to: '/pricing', count: 2210, share: 36 }, { from: '/', to: '/docs/getting-started', count: 1480, share: 24 }, { from: '/pricing', to: '/download', count: 1310, share: 38 },
  { from: '/docs/getting-started', to: '/docs/deploy/compose', count: 980, share: 38 }, { from: '/blog/self-hosted-vercel', to: '/', count: 610, share: 23 }, { from: '/pricing', to: '/compare/coolify', count: 420, share: 12 },
]
const ENTRIES = [{ to: '/', count: 5120, share: 41 }, { to: '/blog/self-hosted-vercel', count: 2710, share: 22 }, { to: '/pricing', count: 1610, share: 13 }, { to: '/docs/getting-started', count: 1240, share: 10 }]
const EXITS = [{ from: '/download', count: 1610, share: 88 }, { from: '/blog/self-hosted-vercel', count: 1650, share: 61 }, { from: '/', count: 2540, share: 41 }, { from: '/pricing', count: 760, share: 22 }]

// ── Icons for the dimensions: the kind of a thing is an icon, its state is a glyph ──
const flag = (cc: string) => <span className="text-[13px] leading-none">{String.fromCodePoint(...[...cc.toUpperCase()].map((c) => 0x1f1e6 + c.charCodeAt(0) - 65))}</span>
const ChromeMark = () => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><circle cx="12" cy="12" r="10" /><circle cx="12" cy="12" r="4" /><path d="M21.2 8H12M3.9 17.5 8.5 9.5M12 22l4.5-8" /></svg>
const FirefoxMark = () => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M12 22a10 10 0 1 0-9.8-8c.6 2.5 2.4 3.5 4.3 3.5-1.6-2-1-4.6.5-5.5.2 1.8 1.6 2.4 2.8 1.5-.8-1.8 0-3.6 1.7-4.5C10 7 10.7 5 12.5 4c1.6 1 2.2 3 1.5 4.5 2.7.2 4.5 2.4 4.5 5" /></svg>
const EdgeMark = () => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M20 15c-1.5 3-4.5 5-8 5a8 8 0 0 1-8-8c0-4 3-7 7-7 3.5 0 6 2 6.5 5.5H9c0 3 2 5 5 5 2 0 4-.5 6-1.5Z" /></svg>
const BROWSER_ICON: Record<string, ReactNode> = { Chrome: <ChromeMark />, Safari: <Compass />, Firefox: <FirefoxMark />, Edge: <EdgeMark />, 'Samsung Internet': <Globe />, Other: <Globe /> }
const CHANNEL_ICON: Record<string, ReactNode> = { direct: <Link />, 'organic search': <Search />, referral: <ExternalLink />, social: <Share2 />, 'ai agents': <Bot />, email: <Mail />, paid: <Megaphone /> }
const DEVICE_ICON: Record<string, ReactNode> = { desktop: <Monitor />, mobile: <Smartphone />, tablet: <Tablet /> }
const withIcons = (rows: BreakdownRow[], pick: (r: BreakdownRow) => ReactNode): BreakdownRow[] => rows.map((r) => ({ ...r, icon: pick(r), children: r.children }))
const COUNTRY_CODE: Record<string, string> = { 'United States': 'us', Germany: 'de', 'United Kingdom': 'gb', Spain: 'es', France: 'fr', Portugal: 'pt', Netherlands: 'nl', Canada: 'ca', Brazil: 'br' }

// ── Speed: five vitals, one selected at a time; the trend, the map and the ledger follow it. ──
type VitalKey = 'TTFB' | 'FCP' | 'LCP' | 'INP' | 'CLS'
type Vital = { k: VitalKey; name: string; good: number; poor: number; unit: 'ms' | '' }
const VITALS: Vital[] = [
  { k: 'LCP', name: 'largest paint', good: 2500, poor: 4000, unit: 'ms' },
  { k: 'INP', name: 'interaction delay', good: 200, poor: 500, unit: 'ms' },
  { k: 'CLS', name: 'layout shift', good: 0.1, poor: 0.25, unit: '' },
  { k: 'TTFB', name: 'first byte', good: 800, poor: 1800, unit: 'ms' },
  { k: 'FCP', name: 'first paint', good: 1800, poor: 3000, unit: 'ms' },
]
const VITAL = Object.fromEntries(VITALS.map((v) => [v.k, v])) as Record<VitalKey, Vital>
const rate = (k: VitalKey, v: number): State => (v > VITAL[k].poor ? 'error' : v > VITAL[k].good ? 'warn' : 'ok')
const RATE_WORD: Record<State, string> = { ok: 'good', warn: 'needs work', error: 'poor', idle: 'no samples', sampled: 'sampled' }
const fmtV = (k: VitalKey, v: number) => (k === 'CLS' ? v.toFixed(2) : v >= 1000 ? `${(v / 1000).toFixed(2)}s` : `${Math.round(v)}ms`)
type Vitals = Record<VitalKey, number>
const P75: Record<'desktop' | 'mobile', Vitals> = { desktop: { TTFB: 880, FCP: 1120, LCP: 1540, INP: 64, CLS: 0.01 }, mobile: { TTFB: 1230, FCP: 1980, LCP: 2710, INP: 210, CLS: 0.06 } }
const vitalSpark = (k: VitalKey, seed: number) => spark(seed).map((x) => (VITAL[k].good * (0.35 + x / 120)))
const TREND = (k: VitalKey) => Array.from({ length: 84 }, (_, i) => { const base = P75.desktop[k] * (0.55 + Math.abs(Math.sin(i / 5.1)) * 0.5); const spike = i === 61 || i === 78 ? P75.desktop[k] * 4.2 : i % 17 === 9 ? P75.desktop[k] * 1.9 : 0; return { t: `${['Aug 30', 'Aug 31', 'Sep 01', 'Sep 02', 'Sep 03', 'Sep 04', 'Sep 05'][Math.floor(i / 12)]} ${String((i % 12) * 2).padStart(2, '0')}:00`, p75: k === 'CLS' ? +(base + spike).toFixed(3) : Math.round(base + spike) } })
type PerfRow = { label: string; geo?: string; icon?: ReactNode; samples: number; v: Vitals }
const PERF: Record<string, PerfRow[]> = {
  pages: [
    { label: '/', samples: 606, v: { TTFB: 496, FCP: 975, LCP: 1170, INP: 126, CLS: 0.01 } },
    { label: '/docs/quickstart', samples: 162, v: { TTFB: 937, FCP: 1500, LCP: 1570, INP: 86, CLS: 0.01 } },
    { label: '/pricing', samples: 89, v: { TTFB: 469, FCP: 1010, LCP: 1220, INP: 62, CLS: 0.02 } },
    { label: '/blog/5-best-alternatives-2026', samples: 77, v: { TTFB: 417, FCP: 1170, LCP: 1170, INP: 26, CLS: 0 } },
    { label: '/blog/pricing-complete-guide-2026', samples: 75, v: { TTFB: 5610, FCP: 6860, LCP: 6860, INP: 16, CLS: 0.02 } },
    { label: '/blog/free-tier-limits-2026', samples: 51, v: { TTFB: 10510, FCP: 11090, LCP: 11090, INP: 43, CLS: 0 } },
    { label: '/blog/railway-pricing-2026', samples: 45, v: { TTFB: 898, FCP: 2230, LCP: 2230, INP: 5, CLS: 0 } },
    { label: '/roadmap', samples: 39, v: { TTFB: 102, FCP: 376, LCP: 416, INP: 47, CLS: 0 } },
    { label: '/tools/vps-security-check', samples: 37, v: { TTFB: 357, FCP: 1740, LCP: 1740, INP: 7, CLS: 0 } },
    { label: '/managed', samples: 36, v: { TTFB: 262, FCP: 506, LCP: 623, INP: 128, CLS: 0 } },
  ],
  countries: [
    { label: 'United States', geo: 'United States of America', icon: flag('us'), samples: 4312, v: { TTFB: 1210, FCP: 1480, LCP: 1900, INP: 70, CLS: 0.01 } },
    { label: 'Germany', geo: 'Germany', icon: flag('de'), samples: 1820, v: { TTFB: 210, FCP: 820, LCP: 1120, INP: 58, CLS: 0.01 } },
    { label: 'United Kingdom', geo: 'United Kingdom', icon: flag('gb'), samples: 1404, v: { TTFB: 410, FCP: 990, LCP: 1310, INP: 61, CLS: 0.01 } },
    { label: 'Spain', geo: 'Spain', icon: flag('es'), samples: 980, v: { TTFB: 240, FCP: 860, LCP: 1150, INP: 55, CLS: 0.02 } },
    { label: 'France', geo: 'France', icon: flag('fr'), samples: 812, v: { TTFB: 260, FCP: 900, LCP: 1190, INP: 60, CLS: 0.01 } },
    { label: 'Portugal', geo: 'Portugal', icon: flag('pt'), samples: 611, v: { TTFB: 290, FCP: 940, LCP: 1240, INP: 57, CLS: 0.01 } },
    { label: 'Brazil', geo: 'Brazil', icon: flag('br'), samples: 540, v: { TTFB: 1420, FCP: 2100, LCP: 2650, INP: 95, CLS: 0.03 } },
    { label: 'Canada', geo: 'Canada', icon: flag('ca'), samples: 420, v: { TTFB: 790, FCP: 1300, LCP: 1700, INP: 66, CLS: 0.01 } },
    { label: 'India', geo: 'India', icon: flag('in'), samples: 380, v: { TTFB: 1650, FCP: 2400, LCP: 3100, INP: 140, CLS: 0.04 } },
    { label: 'Nigeria', geo: 'Nigeria', icon: flag('ng'), samples: 90, v: { TTFB: 2400, FCP: 3300, LCP: 4300, INP: 210, CLS: 0.05 } },
    { label: 'Australia', geo: 'Australia', icon: flag('au'), samples: 210, v: { TTFB: 640, FCP: 1250, LCP: 1600, INP: 72, CLS: 0.01 } },
    { label: 'Japan', geo: 'Japan', icon: flag('jp'), samples: 160, v: { TTFB: 910, FCP: 1400, LCP: 1800, INP: 80, CLS: 0.02 } },
    { label: 'China', geo: 'China', icon: flag('cn'), samples: 120, v: { TTFB: 1380, FCP: 2000, LCP: 2600, INP: 120, CLS: 0.03 } },
    { label: 'Mexico', geo: 'Mexico', icon: flag('mx'), samples: 140, v: { TTFB: 1020, FCP: 1600, LCP: 2100, INP: 88, CLS: 0.02 } },
  ],
  regions: [
    { label: 'California', samples: 1610, v: { TTFB: 980, FCP: 1300, LCP: 1700, INP: 68, CLS: 0.01 } },
    { label: 'New York', samples: 980, v: { TTFB: 1340, FCP: 1600, LCP: 2050, INP: 71, CLS: 0.01 } },
    { label: 'Texas', samples: 640, v: { TTFB: 1410, FCP: 1700, LCP: 2150, INP: 74, CLS: 0.01 } },
    { label: 'Berlin', samples: 760, v: { TTFB: 190, FCP: 800, LCP: 1090, INP: 55, CLS: 0.01 } },
    { label: 'Bavaria', samples: 520, v: { TTFB: 230, FCP: 840, LCP: 1150, INP: 60, CLS: 0.01 } },
  ],
  cities: [
    { label: 'San Francisco', samples: 720, v: { TTFB: 940, FCP: 1280, LCP: 1660, INP: 66, CLS: 0.01 } },
    { label: 'Los Angeles', samples: 480, v: { TTFB: 1010, FCP: 1330, LCP: 1740, INP: 70, CLS: 0.01 } },
    { label: 'Berlin', samples: 760, v: { TTFB: 190, FCP: 800, LCP: 1090, INP: 55, CLS: 0.01 } },
    { label: 'London', samples: 690, v: { TTFB: 400, FCP: 980, LCP: 1290, INP: 60, CLS: 0.01 } },
    { label: 'Madrid', samples: 510, v: { TTFB: 230, FCP: 850, LCP: 1140, INP: 54, CLS: 0.02 } },
  ],
  devices: [
    { label: 'desktop', icon: <Monitor />, samples: 7920, v: P75.desktop },
    { label: 'mobile', icon: <Smartphone />, samples: 3890, v: P75.mobile },
    { label: 'tablet', icon: <Tablet />, samples: 520, v: { TTFB: 1050, FCP: 1700, LCP: 2300, INP: 150, CLS: 0.04 } },
  ],
  browsers: [
    { label: 'Chrome', icon: <ChromeMark />, samples: 7100, v: { TTFB: 860, FCP: 1100, LCP: 1500, INP: 60, CLS: 0.01 } },
    { label: 'Safari', icon: <Compass />, samples: 3200, v: { TTFB: 920, FCP: 1250, LCP: 1720, INP: 90, CLS: 0.02 } },
    { label: 'Firefox', icon: <FirefoxMark />, samples: 1100, v: { TTFB: 870, FCP: 1150, LCP: 1560, INP: 58, CLS: 0.01 } },
    { label: 'Edge', icon: <EdgeMark />, samples: 640, v: { TTFB: 890, FCP: 1130, LCP: 1540, INP: 62, CLS: 0.01 } },
  ],
  os: [
    { label: 'macOS', samples: 5200, v: { TTFB: 850, FCP: 1090, LCP: 1490, INP: 58, CLS: 0.01 } },
    { label: 'Windows', samples: 3100, v: { TTFB: 900, FCP: 1160, LCP: 1580, INP: 66, CLS: 0.01 } },
    { label: 'iOS', samples: 2500, v: { TTFB: 1200, FCP: 1900, LCP: 2600, INP: 190, CLS: 0.05 } },
    { label: 'Android', samples: 1400, v: { TTFB: 1300, FCP: 2100, LCP: 2900, INP: 240, CLS: 0.07 } },
    { label: 'Linux', samples: 350, v: { TTFB: 800, FCP: 1050, LCP: 1400, INP: 50, CLS: 0.01 } },
  ],
}
type PerfDim = 'pages' | 'countries' | 'regions' | 'cities' | 'devices' | 'browsers' | 'os'
const PERF_DIMS = [['pages', 'pages'], ['countries', 'countries'], ['regions', 'regions'], ['cities', 'cities'], ['devices', 'devices'], ['browsers', 'browsers'], ['os', 'OS']] as const
const PERF_ONE: Record<PerfDim, string> = { pages: 'page', countries: 'country', regions: 'region', cities: 'city', devices: 'device', browsers: 'browser', os: 'OS' }
const worst = (v: Vitals): State => (VITALS.some((x) => rate(x.k, v[x.k]) === 'error') ? 'error' : VITALS.some((x) => rate(x.k, v[x.k]) === 'warn') ? 'warn' : 'ok')

// ── Languages: ranked by language, locales underneath. The question is "what should I serve", not "which headers did I see". ──
const LANGUAGES: BreakdownRow[] = [
  { label: 'English', count: 7460, icon: <span className="font-mono text-[10px]">en</span>, children: [{ label: 'en-US', count: 6410 }, { label: 'en-GB', count: 870 }, { label: 'en (no region)', count: 180 }] },
  { label: 'German', count: 1820, icon: <span className="font-mono text-[10px]">de</span>, children: [{ label: 'de-DE', count: 1640 }, { label: 'de-AT', count: 120 }, { label: 'de-CH', count: 60 }] },
  { label: 'Spanish', count: 1290, icon: <span className="font-mono text-[10px]">es</span>, children: [{ label: 'es-ES', count: 940 }, { label: 'es-MX', count: 260 }, { label: 'es-AR', count: 90 }] },
  { label: 'Portuguese', count: 812, icon: <span className="font-mono text-[10px]">pt</span>, children: [{ label: 'pt-PT', count: 520 }, { label: 'pt-BR', count: 292 }] },
  { label: 'French', count: 640, icon: <span className="font-mono text-[10px]">fr</span> },
  { label: 'Chinese', count: 190, icon: <span className="font-mono text-[10px]">zh</span> },
  { label: 'unknown', count: 206, state: 'idle', icon: <span className="font-mono text-[10px]">—</span> },
]

// ── Campaigns: a campaign is a tuple (source · medium · campaign), not five independent lists. Untagged traffic is a sentence, not a 99% bar. ──
type Campaign = { id: string; name: string; source: string; medium: string; visitors: number; sessions: number; signups: number; first: string; last: string; spark: number[]; variants?: { term?: string; content?: string; visitors: number }[] }
const CAMPAIGNS: Campaign[] = [
  { id: 'c1', name: 'launch-sept', source: 'x.com', medium: 'social', visitors: 1240, sessions: 1510, signups: 61, first: '3d ago', last: '4m ago', spark: spark(3), variants: [{ content: 'video', visitors: 810 }, { content: 'thread', visitors: 430 }] },
  { id: 'c2', name: 'hn-self-hosted', source: 'news.ycombinator.com', medium: 'referral', visitors: 980, sessions: 1120, signups: 74, first: '2d ago', last: '12m ago', spark: spark(8) },
  { id: 'c3', name: 'newsletter-38', source: 'buttondown', medium: 'email', visitors: 610, sessions: 640, signups: 22, first: '6d ago', last: '3h ago', spark: spark(5), variants: [{ content: 'top-cta', visitors: 410 }, { content: 'footer', visitors: 200 }] },
  { id: 'c4', name: 'vercel-alternative', source: 'google', medium: 'cpc', visitors: 402, sessions: 455, signups: 9, first: '9d ago', last: '31m ago', spark: spark(2), variants: [{ term: 'vercel alternative', visitors: 290 }, { term: 'self hosted vercel', visitors: 112 }] },
  { id: 'c5', name: 'sponsor-podcast', source: 'changelog.com', medium: 'sponsor', visitors: 148, sessions: 160, signups: 4, first: '14d ago', last: '2d ago', spark: spark(6) },
]
const UNTAGGED = 9038

// ── Events: instrumented, or broken? Each event carries its own health, so the page can say "checkout_started stopped firing after dep_91a". ──
type Ev = { name: string; fires: number; visitors: number; last: string; state: State; note: string; spark: number[]; pages: BreakdownRow[]; props: { key: string; values: BreakdownRow[] }[] }
const EVENTS: Ev[] = [
  { name: 'signup', fires: 318, visitors: 318, last: '2m ago', state: 'ok', note: 'steady · 13/h', spark: spark(1), pages: [{ label: '/signup', count: 318 }], props: [{ key: 'plan', values: [{ label: 'free', count: 291 }, { label: 'team', count: 27 }] }] },
  { name: 'checkout_started', fires: 0, visitors: 0, last: '26h ago', state: 'error', note: 'stopped after dep_91a · was 41/h', spark: [40, 44, 38, 42, 45, 41, 39, 43, 40, 42, 44, 38, 41, 40, 43, 0, 0, 0, 0, 0, 0, 0, 0, 0], pages: [{ label: '/pricing', count: 0 }], props: [] },
  { name: 'repo_connected', fires: 402, visitors: 380, last: '5m ago', state: 'ok', note: 'steady · 17/h', spark: spark(4), pages: [{ label: '/app/projects/new', count: 402 }], props: [{ key: 'provider', values: [{ label: 'github', count: 351 }, { label: 'gitlab', count: 39 }, { label: 'bitbucket', count: 12 }] }] },
  { name: 'deploy_succeeded', fires: 1204, visitors: 296, last: '1m ago', state: 'ok', note: 'steady · 50/h', spark: spark(7), pages: [{ label: '(server)', count: 1204 }], props: [{ key: 'preset', values: [{ label: 'nextjs', count: 640 }, { label: 'docker', count: 312 }, { label: 'static', count: 252 }] }] },
  { name: 'docs_search', fires: 2110, visitors: 1480, last: '30s ago', state: 'warn', note: 'half of usual · 88/h, was 170/h', spark: spark(9).map((v, i) => (i > 14 ? v / 2 : v)), pages: [{ label: '/docs/*', count: 2110 }], props: [{ key: 'query', values: [{ label: 'compose', count: 410 }, { label: 'env vars', count: 380 }, { label: 'pitr', count: 210 }] }] },
  { name: 'pricing_toggle_yearly', fires: 890, visitors: 812, last: '4m ago', state: 'ok', note: 'steady · 37/h', spark: spark(11), pages: [{ label: '/pricing', count: 890 }], props: [] },
]

const TABS = ['overview', 'audience', 'campaigns', 'pages', 'events', 'funnels', 'speed'] as const
type Tab = (typeof TABS)[number]

export function AnalyticsScreen({ dense, plan, notify, go }: { dense: boolean; plan: Plan; notify: Notify; go: (v: string) => void }) {
  const [tab, setTab] = useState<Tab>('overview')
  const [range, setRange] = useState('24h')
  const [compare, setCompare] = useState(false)
  const [sel, setSel] = useState<TimeRange | null>(null)
  const [q, setQ] = useState('')
  const [fresh] = useFresh()
  const [pagesView, setPagesView] = useState<'list' | 'flow'>('list')
  const [device, setDevice] = useState<'desktop' | 'mobile'>('desktop')
  const [vital, setVital] = useState<VitalKey>('LCP')
  const [geoView, setGeoView] = useState<'list' | 'map'>('list')
  const [perfDim, setPerfDim] = useState<PerfDim>('pages')
  const [funnel, setFunnel] = useState(0)
  const perfRows: LedgerRow[] = [...PERF[perfDim]].filter((r) => matchesQ(q, r.label)).sort((a, b) => b.v[vital] - a.v[vital]).map((r) => ({
    id: r.label, state: worst(r.v), onOpen: () => notify('ok', `filter speed by ${r.label}`, `${r.samples} samples`),
    sort: { label: r.label, samples: r.samples, ...r.v },
    mobile: <><span className="block truncate font-mono">{r.label}</span><span className="block text-[11px] text-muted-foreground">{vital} {fmtV(vital, r.v[vital])} · {RATE_WORD[rate(vital, r.v[vital])]} · {r.samples.toLocaleString()} samples</span></>,
    cells: [
      <span className="flex min-w-0 items-center gap-2">{r.icon && <span className="flex w-4 shrink-0 justify-center text-muted-foreground [&_svg]:h-3.5 [&_svg]:w-3.5">{r.icon}</span>}<span className="truncate font-mono">{r.label}</span></span>,
      <Num value={r.samples} />,
      ...VITALS.map((v) => { const st = rate(v.k, r.v[v.k]); return <span key={v.k} className={cn(st === 'error' && 'text-destructive', st === 'warn' && 'text-warning', v.k === vital && 'font-medium')}>{st !== 'ok' && <span aria-hidden className="mr-1">{st === 'error' ? '×' : '◐'}</span>}{fmtV(v.k, r.v[v.k])}</span> }),
    ],
  }))
  const broken = EVENTS.find((e) => e.state === 'error')
  const status = fresh ? <StatusLine state="idle">No visits recorded yet. The snippet is not on the site.</StatusLine> : (
    <StatusLine state="error" more={{ label: '+3', items: [
      { state: 'warn', children: <><Phrase onClick={() => setTab('funnels')}>Connected a repository</Phrase> loses 56% of new accounts; the step above it loses 27%.</> },
      { state: 'warn', children: <><Phrase onClick={() => setTab('speed')}>TTFB</Phrase> needs work on desktop: 880ms at p75, driven by the United States (1.2s) and two blog posts above 5s.</> },
      { state: 'sampled', children: <>498 visits are AI agents (ChatGPT-User, PerplexityBot); they are counted apart, never as visitors.</> },
    ] }}>
      <Phrase onClick={() => go(`event:${broken?.name}`)}>{broken?.name}</Phrase> stopped firing 26h ago, right after dep_91a. Was 41 an hour.
    </StatusLine>
  )
  const locations = withIcons(LOCATIONS, (r) => (COUNTRY_CODE[String(r.label)] ? flag(COUNTRY_CODE[String(r.label)]) : undefined))
  const channels = withIcons(CHANNELS, (r) => CHANNEL_ICON[String(r.label)])
  const browsers = withIcons(BROWSERS, (r) => BROWSER_ICON[String(r.label)])
  const devices = withIcons(DEVICES, (r) => DEVICE_ICON[String(r.label)])
  const pageRows: LedgerRow[] = PAGES.filter((p) => matchesQ(q, p.path)).map((p) => ({
    id: p.path, state: (p.bounce > 50 ? 'warn' : 'ok') as State,
    mobile: <><span className="block font-mono">{p.path}</span><span className="block text-[11px] text-muted-foreground"><Num value={p.views} /> views · {p.bounce}% bounce</span></>,
    cells: [
      <span className="font-mono">{p.path}</span>,
      <span className="block w-full text-foreground"><Sparkline points={p.spark} state={p.bounce > 50 ? 'warn' : undefined} /></span>,
      <Num value={p.views} />, <Num value={p.sessions} />,
      <Num value={p.avg_s} unit="s" />,
      p.bounce > 50 ? <Status state="warn" label={`${p.bounce}%`} /> : <Num value={p.bounce} unit="%" />,
    ],
  }))
  const campaignRows: LedgerRow[] = CAMPAIGNS.filter((c) => matchesQ(q, c.name, c.source, c.medium)).map((c) => ({
    id: c.id, state: c.signups / c.visitors < 0.01 ? 'warn' : 'ok', onOpen: () => notify('ok', `open campaign ${c.name}`, c.variants ? `${c.variants.length} variants by ${c.variants[0].term ? 'term' : 'content'}` : 'no variants'),
    sort: { name: c.name, visitors: c.visitors, signups: c.signups, conv: c.signups / c.visitors, last: c.last },
    mobile: <><span className="block truncate font-mono">{c.name} <span className="text-muted-foreground">· {c.source}</span></span><span className="block text-[11px] text-muted-foreground"><Num value={c.visitors} /> visitors · {((c.signups / c.visitors) * 100).toFixed(1)}% signed up</span></>,
    cells: [
      <span className="flex min-w-0 items-center gap-2"><span className="flex w-4 shrink-0 justify-center text-muted-foreground [&_svg]:h-3.5 [&_svg]:w-3.5">{CHANNEL_ICON[c.medium === 'cpc' ? 'paid' : c.medium === 'sponsor' ? 'paid' : c.medium] ?? <Tag />}</span><span className="truncate font-mono">{c.name}</span>{c.variants && <span className="shrink-0 border px-1 text-[10px] text-muted-foreground">{c.variants.length} variants</span>}</span>,
      <span className="truncate text-muted-foreground">{c.source} <span className="text-[11px]">· {c.medium}</span></span>,
      <span className="block w-full"><Sparkline points={c.spark} /></span>,
      <Num value={c.visitors} />,
      <Num value={c.signups} />,
      c.signups / c.visitors < 0.01 ? <Status state="warn" label={`${((c.signups / c.visitors) * 100).toFixed(1)}%`} /> : <Num value={((c.signups / c.visitors) * 100).toFixed(1)} unit="%" />,
      <span className="text-muted-foreground">{c.last}</span>,
    ],
  }))
  const eventRows: LedgerRow[] = EVENTS.filter((e) => matchesQ(q, e.name, e.note)).map((e) => ({
    id: e.name, state: e.state, onOpen: () => go(`event:${e.name}`),
    sort: { name: e.name, fires: e.fires, visitors: e.visitors, state: e.state },
    mobile: <><span className="block font-mono">{e.name}</span><span className="block text-[11px] text-muted-foreground">{e.note}</span></>,
    cells: [
      <span className="flex min-w-0 items-center gap-2"><span className="flex w-4 shrink-0 justify-center text-muted-foreground [&_svg]:h-3.5 [&_svg]:w-3.5"><Zap /></span><span className="truncate font-mono">{e.name}</span></span>,
      <span className={e.state === 'error' ? 'text-destructive' : e.state === 'warn' ? 'text-warning' : 'text-muted-foreground'}>{e.note}</span>,
      <span className="block w-full"><Sparkline points={e.spark} state={e.state === 'error' ? 'error' : e.state === 'warn' ? 'warn' : undefined} /></span>,
      <Num value={e.fires} />, <Num value={e.visitors} />,
      <span className="text-muted-foreground">{e.last}</span>,
    ],
  }))
  const tagged = CAMPAIGNS.reduce((a, c) => a + c.visitors, 0)
  return (
    <Detail title="Analytics" meta={fresh ? 'acme-storefront · production · no data yet' : `acme-storefront · production · ${TOTAL.toLocaleString()} visitors · ${range}`} status={status} tabs={TABS} tab={tab} onTab={(t) => { setTab(t); setQ('') }}
      actions={<>
        <label className="inline-flex h-7 items-center gap-1.5 text-xs"><input type="checkbox" checked={compare} onChange={(e) => setCompare(e.target.checked)} className="accent-foreground" /> compare with previous {range}</label>
        <RangePicker ranges={RANGES} value={range} onChange={setRange} retentionDays={plan.retentionDays} retentionLabel={plan.retention} onGated={(r) => notify('warn', `${r.label} is beyond this plan's retention`, `currently ${plan.retention}`)} />
        <Button size="sm" variant="outline" className="h-7 text-xs" asChild><a href="/settings/analytics">snippet <ExternalLink /></a></Button>
      </>}>

      {fresh && (
        <PageState state="unconfigured" title="No visits recorded yet"
          missing={`the tracking snippet on acme-storefront. Add one script tag; visitors, pages, countries, events and Core Web Vitals start filling in within a minute of the first visit. ${tab === 'overview' ? '' : `The ${tab} view needs the same snippet; nothing else to set up.`}`}
          example={<div className="space-y-2 font-mono text-[11px]">
            <p>● 12,418 visitors · 24h · 318 signups (2.6%) · LCP 1.54s good · TTFB 880ms needs work</p>
            <div className="flex h-12 items-end gap-px">{HOURLY.map((h, i) => <span key={i} className="flex-1 bg-foreground/60" style={{ height: `${20 + (h.visitors / 900) * 80}%` }} />)}</div>
            <p className="text-muted-foreground">🇺🇸 United States 35% · direct 39% · Chrome 58% · desktop 64%</p>
            <pre className="op-inset whitespace-pre-wrap border px-3 py-2">{`<script defer src="https://temps.acme.sh/t.js" data-project="acme-storefront"></script>`}</pre>
            <p className="text-muted-foreground">No cookies, no consent banner needed: visitors are counted by a daily rotating hash. Bots and AI agents are counted apart from the start.</p>
          </div>}
          settingsHref="/settings/analytics" settingsLabel="copy the snippet" />
      )}
      {!fresh && tab === 'overview' && (
        <div className="space-y-6">
          <MetricGrid cols={4}>
            <Metric label="visitors" value={TOTAL} baseline={compare ? '+8% vs previous 24h' : `${range} · unique by visitor id`} />
            <Metric label="sessions" value={14_902} baseline={compare ? '+6% vs previous 24h' : '1.2 per visitor'} />
            <Metric label="signups" value={318} baseline="2.6% of visitors · goal event" />
            <Metric label="bounce" value={38} unit="%" baseline="one page then left" state="warn" />
          </MetricGrid>
          <div className="space-y-2">
            <TimeChart data={HOURLY} unit="visitors" height={200} xInterval={11}
              series={compare ? [{ key: 'visitors', name: 'visitors' }, { key: 'prev', name: 'previous' }] : [{ key: 'visitors', name: 'visitors' }]}
              markers={[{ id: 'dep_91a', x: '20:30' }]} selection={sel} onSelect={setSel}
              readoutFormat={(p) => `${p.t} · ${Number(p.visitors).toLocaleString()} visitors${compare ? ` · previous ${Number(p.prev).toLocaleString()}` : ''}`} />
            <ChartFooter><span>visitors / 30 min · {range}</span><span>· ┆ deploy</span><span>· drag to measure a window</span>{sel && <span>· {sel.from} → {sel.to} selected · the lists below cover the whole range</span>}</ChartFooter>
          </div>
          <div className="op-grid grid gap-6 md:grid-cols-2 xl:grid-cols-4">
            <Section title="Where" meta="country → region → city"><Breakdown rows={locations} total={TOTAL} limit={6} more={{ label: 'audience', onClick: () => setTab('audience') }} /></Section>
            <Section title="How they arrived" meta="channel → source"><Breakdown rows={channels} total={TOTAL} limit={6} more={{ label: 'campaigns', onClick: () => setTab('campaigns') }} /></Section>
            <Section title="Pages" meta="top 5"><Breakdown rows={PAGES.slice(0, 5).map((p) => ({ label: <span className="font-mono">{p.path}</span>, key: p.path, count: p.views, onOpen: () => setTab('pages') }))} total={PAGES.reduce((a, p) => a + p.views, 0)} unit="views" limit={5} more={{ label: 'all pages', onClick: () => setTab('pages') }} /></Section>
            <Section title="Events" meta="by fires"><Breakdown rows={EVENTS.map((e) => ({ label: <span className="font-mono">{e.name}</span>, key: e.name, count: e.fires, state: e.state === 'ok' ? undefined : e.state, onOpen: () => go(`event:${e.name}`) }))} total={EVENTS.reduce((a, e) => a + e.fires, 0)} unit="fires" limit={5} more={{ label: 'all events', onClick: () => setTab('events') }} /></Section>
          </div>
        </div>
      )}

      {!fresh && tab === 'audience' && (
        <div className="space-y-6">
          <p className="text-xs text-muted-foreground">Who the visitors are. Every list ranks by visitors and opens in place; the share is of all {TOTAL.toLocaleString()} visitors in {range}.</p>
          <div className="op-grid grid gap-6 md:grid-cols-2 xl:grid-cols-4">
            <Section title="Where" meta="country → region → city"><Breakdown rows={locations} total={TOTAL} limit={8} /></Section>
            <Section title="Language" meta="language → locale"><Breakdown rows={LANGUAGES} total={TOTAL} limit={8} /></Section>
            <Section title="Browser" meta="browser → version"><Breakdown rows={browsers} total={TOTAL} limit={8} /></Section>
            <Section title="Device"><Breakdown rows={devices} total={TOTAL} /></Section>
          </div>
          <p className="text-xs text-muted-foreground">"unknown" language means the browser sent no Accept-Language header: most crawlers, and some privacy browsers. It is never guessed from the country.</p>
        </div>
      )}

      {!fresh && tab === 'campaigns' && (
        <Ledger status={null} dense={dense}
          columns={[{ label: 'campaign', key: 'name' }, 'source · medium', range, { label: 'visitors', key: 'visitors', numeric: true }, { label: 'signups', key: 'signups', numeric: true }, { label: 'signed up', key: 'conv', numeric: true }, { label: 'last seen', key: 'last' }]}
          grid="minmax(12rem,1.6fr) minmax(10rem,1.4fr) minmax(7rem,1fr) minmax(70px,max-content) minmax(70px,max-content) minmax(80px,max-content) minmax(80px,max-content)"
          rows={campaignRows} total={CAMPAIGNS.length} filter={q} onFilter={setQ} placeholder="filter campaigns, sources, mediums"
          hint={<><Num value={UNTAGGED} /> of {(UNTAGGED + tagged).toLocaleString()} visits carried no utm tags and are counted under channels, not here · <a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'link builder', 'utm_source, utm_medium, utm_campaign filled from a form; copies the URL') }}>build a tagged link</a></>}
          action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'link builder', 'utm_source, utm_medium, utm_campaign filled from a form; copies the URL')}><Tag /> tagged link</Button>}
          footer={<span>a campaign is source · medium · campaign together; term and content are its variants, inside the row · ◐ under 1% signed up</span>} />
      )}

      {!fresh && tab === 'pages' && pagesView === 'list' && (
        <Ledger status={null} dense={dense}
          columns={[{ label: 'path', key: 'path' }, range, { label: 'views', key: 'views', numeric: true }, { label: 'sessions', key: 'sessions', numeric: true }, { label: 'avg time', key: 'avg', numeric: true }, { label: 'bounce', key: 'bounce', numeric: true }]}
          grid="minmax(12rem,2fr) minmax(8rem,1.5fr) minmax(70px,max-content) minmax(70px,max-content) minmax(70px,max-content) minmax(60px,max-content)"
          rows={pageRows} total={PAGES.length} filter={q} onFilter={setQ} placeholder="filter paths" hint="◐ bounce above 50%"
          action={<Segmented options={[['list', 'list'], ['flow', 'flow']] as const} value={pagesView} onChange={setPagesView} className="h-7 [&>button]:h-7" />} />
      )}
      {!fresh && tab === 'pages' && pagesView === 'flow' && (
        <div className="space-y-6">
          <div className="flex items-center justify-between gap-2"><p className="text-xs text-muted-foreground">Where sessions go, as ranked transitions. A Sankey would hide the numbers.</p><Segmented options={[['list', 'list'], ['flow', 'flow']] as const} value={pagesView} onChange={setPagesView} className="h-7 [&>button]:h-7" /></div>
          <Section title="Transitions" meta={`top ${TRANSITIONS.length} · share of the from-page's exits`}><Flow rows={TRANSITIONS} /></Section>
          <div className="op-grid grid gap-6 lg:grid-cols-2">
            <Section title="Entry pages" meta="share of sessions"><Flow rows={ENTRIES} /></Section>
            <Section title="Exit pages" meta="share of that page's views that ended there"><Flow rows={EXITS} /></Section>
          </div>
        </div>
      )}

      {!fresh && tab === 'events' && (
        <Ledger status={null} dense={dense}
          columns={[{ label: 'event', key: 'name' }, { label: 'health', key: 'state' }, range, { label: 'fires', key: 'fires', numeric: true }, { label: 'visitors', key: 'visitors', numeric: true }, 'last seen']}
          grid="minmax(11rem,1.4fr) minmax(12rem,1.8fr) minmax(7rem,1fr) minmax(70px,max-content) minmax(70px,max-content) minmax(70px,max-content)"
          rows={eventRows} total={EVENTS.length} filter={q} onFilter={setQ} placeholder="filter events"
          hint="health compares the last 24h with the 7 days before · × stopped · ◐ far below usual"
          action={<Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'track an event', 'temps.track("event_name", { key: "value" }) · one line in the SDK')}>how to track one</Button>}
          footer={<span>an event is instrumentation: the page says when it breaks, not only how often it fires</span>} />
      )}

      {/* Analytics is a tool page with seven facets, not a record, so the two-column funnels layout is the
          page's own op-grid. The record recipe's main-and-aside pair belongs to pages that carry a Lede. */}
      {!fresh && tab === 'funnels' && (
        <div className="op-grid grid gap-6 lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
          <div>
            <Section title={FUNNELS[funnel].name} meta={`${FUNNELS[funnel].steps[0].count.toLocaleString()} entered · ${completed(FUNNELS[funnel].steps).toFixed(1)}% completed · ${range}`} action={<a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'would open the funnel editor') }} className="text-xs">edit steps</a>}>
              <Funnel steps={FUNNELS[funnel].steps} />
            </Section>
          </div>
          <div>
            <Section title="Other funnels" meta={String(FUNNELS.length - 1)}>
              <ol className="op-rows border bg-background text-xs">
                {FUNNELS.map((f, i) => i === funnel ? null : (
                  <li key={f.id}>
                    <button type="button" onClick={() => setFunnel(i)} className="flex w-full items-center justify-between gap-3 px-3 py-2 text-left hover:bg-muted/40">
                      <span className="truncate underline underline-offset-4">{f.name}</span>
                      <span className="shrink-0 font-mono text-muted-foreground">{completed(f.steps).toFixed(1)}%</span>
                    </button>
                  </li>
                ))}
              </ol>
            </Section>
          </div>
        </div>
      )}

      {!fresh && tab === 'speed' && (
        <div className="space-y-6">
          <Section title="Core Web Vitals" meta={`p75 · real visitors · ${device} · ${range}`} action={<Segmented options={[['desktop', 'desktop'], ['mobile', 'mobile']] as const} value={device} onChange={setDevice} className="h-7 [&>button]:h-7" />}>
            <div className="op-tiles" style={{ '--tiles': 5 } as CSSProperties}>
              {VITALS.map((v) => { const val = P75[device][v.k]; const st = rate(v.k, val); const on = vital === v.k; return (
                <button key={v.k} type="button" aria-pressed={on} onClick={() => setVital(v.k)} className={cn('min-w-0 p-3 text-left transition-colors hover:bg-muted/40', on && 'bg-muted/60')}>
                  <p className="flex items-baseline gap-2"><span className="op-label">{v.k}</span><span className="truncate text-[11px] text-muted-foreground">{v.name}</span></p>
                  <p className="mt-1 flex items-baseline gap-2 text-lg leading-6"><span className="font-mono">{fmtV(v.k, val)}</span><span className="text-xs"><Status state={st} label={RATE_WORD[st]} /></span></p>
                  <div className="mt-2 text-foreground"><Sparkline points={vitalSpark(v.k, VITALS.indexOf(v) + 2)} state={st === 'ok' ? undefined : st} /></div>
                </button>
              ) })}
            </div>
            <p className="mt-2 font-mono text-[11px] text-muted-foreground">good ≤ {fmtV(vital, VITAL[vital].good)} · poor &gt; {fmtV(vital, VITAL[vital].poor)} · the selected vital drives the trend, the map and the sort below</p>
          </Section>
          <Section title={`${vital} over ${range}`} meta="p75 per 2h · ┆ deploy">
            <div className="border bg-background p-3">
              <TimeChart data={TREND(vital)} series={[{ key: 'p75', name: `${vital} p75` }]} unit={VITAL[vital].unit} height={200} xInterval={11} markers={[{ id: 'dep_91a', x: 'Sep 05 10:00' }]}
                thresholds={[{ y: VITAL[vital].good, label: 'good', state: 'ok' }, { y: VITAL[vital].poor, label: 'poor', state: 'error' }]}
                readoutFormat={(p) => `${p.t} · ${vital} ${fmtV(vital, Number(p.p75))} · ${RATE_WORD[rate(vital, Number(p.p75))]}`} />
            </div>
          </Section>
          <Section title={`${vital} by country`} meta={geoView === 'list' ? 'worst first · p75' : 'filled by state · hover to read'} action={<Segmented options={[['list', 'list'], ['map', 'map']] as const} value={geoView} onChange={setGeoView} className="h-7 [&>button]:h-7" />}>
            {geoView === 'list' ? (
              <Breakdown rows={[...PERF.countries].sort((a, b) => b.v[vital] - a.v[vital]).map((r) => ({ label: r.label, icon: r.icon, count: r.v[vital], state: rate(vital, r.v[vital]) === 'ok' ? undefined : rate(vital, r.v[vital]), onOpen: () => notify('ok', `filter speed by ${r.label}`, `${r.samples} samples`) }))} total={Math.max(...PERF.countries.map((r) => r.v[vital]))} unit={VITAL[vital].unit || 'cls'} percent={false} limit={8} />
            ) : (
              <GeoMap rows={PERF.countries.map((r) => ({ geo: r.geo ?? r.label, label: r.label, value: `${vital} ${fmtV(vital, r.v[vital])} · ${RATE_WORD[rate(vital, r.v[vital])]}`, state: rate(vital, r.v[vital]), note: `${r.samples.toLocaleString()} samples` }))} onOpen={(r) => notify('ok', `filter speed by ${r.label}`, r.value)} />
            )}
          </Section>
          <Ledger status={null} dense={dense}
            columns={[{ label: PERF_ONE[perfDim], key: 'label' }, { label: 'samples', key: 'samples', numeric: true }, ...VITALS.map((v) => ({ label: v.k, key: v.k, numeric: true }))]}
            grid="minmax(12rem,2fr) minmax(70px,max-content) repeat(5, minmax(70px,max-content))"
            rows={perfRows} total={PERF[perfDim].length} filter={q} onFilter={setQ} placeholder={`filter ${perfDim}`}
            hint={<>colour only where a vital is not good · ◐ needs work · × poor · row state is its worst vital</>}
            action={<Segmented options={PERF_DIMS} value={perfDim} onChange={setPerfDim} className="h-7 [&>button]:h-7" />}
            footer={<span>p75 per group · sorted by {vital}, worst first · AI agents and crawlers excluded · 12,330 samples</span>} />
        </div>
      )}
    </Detail>
  )
}

function matchesQ(q: string, ...fields: string[]) { const n = q.trim().toLowerCase(); return !n || fields.some((f) => f.toLowerCase().includes(n)) }

// ── Event detail: a record page. Lede says whether it is alive; content is the rate; aside is where it fires and with what. ──
export function EventScreen({ name, go }: { name: string; go: (v: string) => void }) {
  const e = EVENTS.find((x) => x.name === name) ?? EVENTS[0]
  const series = e.spark.map((v, i) => ({ t: `${String(i).padStart(2, '0')}:00`, fires: Math.round(v) }))
  const evFacts: KV[] = [
    { k: 'fires 24h', v: e.fires.toLocaleString(), mono: true, state: e.state === 'error' ? 'error' : undefined },
    { k: 'usual', v: e.state === 'error' ? '41 / h' : e.note.replace(/^steady · |^half of usual · /, '').split(',')[0], mono: true },
    { k: 'visitors', v: e.visitors.toLocaleString(), mono: true },
    { k: 'last fired', v: e.last, mono: true },
    { k: 'pages', v: e.pages.map((p) => String(p.label)).join(', '), mono: true },
    { k: 'properties', v: e.props.length ? e.props.map((p) => p.key).join(', ') : 'none', mono: true, state: e.props.length ? undefined : 'idle' },
  ]
  const lede = e.state === 'error'
    ? <Lede state="error" word="stopped" facts={evFacts}>last fired {e.last} · was 41 an hour · the drop starts at dep_91a</Lede>
    : e.state === 'warn'
      ? <Lede state="warn" word="below usual" facts={evFacts}>{e.note} · last {e.last}</Lede>
      : <Lede state="ok" word="firing" facts={evFacts}>{e.note} · last {e.last} · {e.visitors.toLocaleString()} visitors in 24h</Lede>
  // The verdict says what to do; the lede already says how it is doing.
  const status = e.state === 'error'
    ? <StatusLine state="error">Stopped right after <Phrase onClick={() => go('api-gateway')}>dep_91a</Phrase>: the call site in <span className="font-mono">src/checkout/Cart.tsx</span> is gone from that build. Roll back or restore the call.</StatusLine>
    : e.state === 'warn'
      ? <StatusLine state="warn">Halved since 10:00 with no deploy in between. Visitors to /docs are unchanged, so the search box itself is the suspect: open <span className="font-mono">/docs</span> and try a query.</StatusLine>
      : <StatusLine state="ok">Nothing to do: firing at its usual rate, last {e.last}.</StatusLine>
  return (
    <Detail title={<span className="font-mono">{e.name}</span>} meta={`event · acme-storefront · production`} status={status} lede={lede}
      actions={e.state === 'error' ? <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => go('api-gateway')}>open dep_91a</Button> : undefined}>
      <Columns>
        <div>
          <Section title="Fires per hour" meta="24h · ┆ deploy">
            <div className="border bg-background p-3">
              <TimeChart data={series} series={[{ key: 'fires', name: 'fires' }]} unit="/h" height={160} xInterval={5} markers={[{ id: 'dep_91a', x: '15:00' }]} readoutFormat={(p) => `${p.t} · ${p.fires} fires`} />
            </div>
          </Section>
          <Section title="Recent" meta={e.fires ? 'last 5' : 'none in 24h'}>
            {e.fires ? (
              <Timeline items={[0, 1, 2, 3, 4].map((i) => ({ t: `${i * 2 + 1}m ago`, label: e.name, icon: <Zap />, note: `${e.pages[0]?.label} · visitor v_${(8123 + i * 977).toString(36)}${e.props[0] ? ` · ${e.props[0].key}=${e.props[0].values[i % e.props[0].values.length].label}` : ''}` }))} />
            ) : (
              <div className="border bg-background px-3 py-3 text-xs text-muted-foreground">Nothing fired in the last 24h. The call site was <span className="font-mono">src/checkout/Cart.tsx:41</span> in dep_90e; check whether dep_91a still calls <span className="font-mono">temps.track("{e.name}")</span>.</div>
            )}
          </Section>
        </div>
        <div>
          <Section title="Where it fires" meta={`${e.pages.length} page${e.pages.length === 1 ? '' : 's'}`}><Breakdown rows={e.pages.map((p) => ({ ...p, label: <span className="font-mono">{p.label}</span> }))} total={e.fires || 1} unit="fires" /></Section>
          {e.props.map((p) => <Section key={p.key} title={<span className="font-mono">{p.key}</span>} meta="property · top values"><Breakdown rows={p.values} total={e.fires || 1} unit="fires" limit={5} /></Section>)}
          {e.props.length === 0 && <Section title="Properties" meta="none"><div className="border bg-background px-3 py-3 text-xs text-muted-foreground">This event is sent without properties. Add some to break it down: <span className="font-mono">temps.track("{e.name}", {'{'} plan: "team" {'}'})</span>.</div></Section>}
        </div>
      </Columns>
    </Detail>
  )
}

// ── Uptime ─────────────────────────────────────────────────────────────

type Monitor = { id: string; name: string; url: string; type: 'http' | 'tcp' | 'dns'; every: string; state: State; uptime: number; p50: number; p95: number; buckets: StatusBucket[] }
function buckets(seed: number, bad: number[] = [], degraded: number[] = []): StatusBucket[] {
  return Array.from({ length: 48 }, (_, i) => {
    const h = `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`
    const state: State = bad.includes(i) ? 'error' : degraded.includes(i) ? 'warn' : 'ok'
    const p50 = 80 + ((i * seed) % 40) + (state === 'warn' ? 400 : 0)
    return { start: h, state, checks: 60, down: state === 'error' ? 60 : 0, p50_ms: state === 'error' ? undefined : p50, p95_ms: state === 'error' ? undefined : p50 * 3 }
  })
}
const MONITORS: Monitor[] = [
  { id: 'mon_1', name: 'acme.sh', url: 'https://acme.sh/', type: 'http', every: '30s', state: 'ok', uptime: 100, p50: 92, p95: 210, buckets: buckets(3) },
  { id: 'mon_2', name: 'api-gateway', url: 'https://api.acme.sh/healthz', type: 'http', every: '30s', state: 'error', uptime: 97.9, p50: 140, p95: 880, buckets: buckets(5, [41], [40, 42]) },
  { id: 'mon_3', name: 'checkout', url: 'https://acme.sh/checkout', type: 'http', every: '1m', state: 'warn', uptime: 99.6, p50: 210, p95: 1320, buckets: buckets(7, [], [12, 13, 30]) },
  { id: 'mon_4', name: 'postgres · primary', url: 'db-primary.internal:5432', type: 'tcp', every: '30s', state: 'ok', uptime: 100, p50: 3, p95: 8, buckets: buckets(2) },
  { id: 'mon_5', name: 'acme.sh · dns', url: 'A acme.sh → 91.107.201.10', type: 'dns', every: '5m', state: 'ok', uptime: 100, p50: 18, p95: 41, buckets: buckets(9) },
]

export function UptimeScreen({ dense, notify, go }: { dense: boolean; notify: Notify; go: (v: string) => void }) {
  const [q, setQ] = useState('')
  const [paused, setPaused] = useState(false)
  const list = useMemo(() => MONITORS.filter((m) => matchesQ(q, m.name, m.url)), [q])
  const down = MONITORS.filter((m) => m.state === 'error')
  const rows: LedgerRow[] = list.map((m) => ({
    id: m.id, state: m.state, onOpen: () => go(`monitor:${m.id}`),
    mobile: <><span className="block"><span className="font-medium">{m.name}</span> <span className="text-[11px] text-muted-foreground">{m.uptime}%</span></span><StatusStrip buckets={m.buckets} height={10} className="mt-1" /></>,
    cells: [
      <span className="min-w-0"><span className="block truncate font-medium">{m.name}</span><span className="block truncate font-mono text-[11px] text-muted-foreground">{m.url}</span></span>,
      <StatusStrip buckets={m.buckets} height={dense ? 12 : 16} />,
      m.uptime < 99.9 ? <Status state="warn" label={`${m.uptime}%`} /> : <Num value={m.uptime} unit="%" />,
      <Num value={m.p50} unit="ms" />, <Num value={m.p95} unit="ms" />,
      <span className="text-muted-foreground">{m.type} · {m.every}</span>,
    ],
  }))
  return (
    <Ledger title="Uptime" meta={`${MONITORS.length} monitors · 24h · ${MONITORS.filter((m) => m.state === 'ok').length} up`}
      status={
        <StatusLine state={down.length ? 'error' : 'ok'} more={{ label: '+1 slow', items: [{ state: 'warn', children: <><Phrase onClick={() => setQ('checkout')}>checkout</Phrase> p95 is 1.3s over the last hour, above the 1s threshold.</> }] }}>
          {down.length ? <><Phrase onClick={() => setQ(down[0].name)}>{down[0].name}</Phrase> was down for 30 minutes at 20:30, right after dep_91a.</> : <>Everything is up.</>}
        </StatusLine>
      }
      columns={['monitor', '24h · one segment per 30 min', { label: 'uptime', key: 'uptime', numeric: true }, { label: 'p50', key: 'p50', numeric: true }, { label: 'p95', key: 'p95', numeric: true }, 'check']}
      grid="minmax(12rem,1.5fr) minmax(14rem,3fr) minmax(70px,max-content) minmax(70px,max-content) minmax(70px,max-content) minmax(90px,max-content)"
      rows={rows} total={MONITORS.length} filter={q} onFilter={setQ} placeholder="filter monitors" dense={dense}
      hint={<Live every="30s" paused={paused} onToggle={() => setPaused((p) => !p)} />}
      action={<><Button size="sm" variant="outline" className="h-7 text-xs" asChild><a href="/status?project=acme-storefront" target="_blank" rel="noreferrer">status page <ExternalLink /></a></Button><Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'add monitor', 'http · tcp · dns · every 30s')}>add monitor</Button></>}
      footer={<span>● up · ◐ slow · × down · hover or focus the strip and use ← → to read each segment</span>} />
  )
}

// ── Monitor record: is it up, how fast, what happened, how it is checked ──
const INCIDENTS = [
  { id: 'inc_31', t: '20:30 · 30 min', label: 'down · 60 of 60 checks failed', note: 'connection refused from all 3 regions · right after dep_91a · resolved by dep_91b', state: 'error' as State, icon: <Zap /> },
  { id: 'inc_30', t: '3d ago · 12 min', label: 'slow · p95 above 1s', note: 'api:8080 under load · recovered on its own', state: 'warn' as State, icon: <Activity /> },
  { id: 'inc_29', t: '9d ago · 4 min', label: 'down · certificate expired', note: 'acme renewal had failed twice · renewed by hand', state: 'error' as State, icon: <Zap /> },
]
const RT = (m: Monitor) => m.buckets.map((b) => ({ t: b.start, p50: b.p50_ms ?? 0, p95: b.p95_ms ?? 0 }))
export function MonitorScreen({ id, notify, go }: { id: string; notify: Notify; go: (v: string) => void }) {
  const m = MONITORS.find((x) => x.id === id) ?? MONITORS[0]
  const [range, setRange] = useState<'24h' | '7d' | '30d' | '90d'>('24h')
  const [paused, setPaused] = useState(false)
  const up = { '24h': m.uptime, '7d': Math.min(100, m.uptime + 0.4), '30d': Math.min(100, m.uptime + 0.9), '90d': Math.min(100, m.uptime + 1.2) }
  const word = paused ? 'paused' : m.state === 'error' ? 'down' : m.state === 'warn' ? 'slow' : 'up'
  const status = paused
    ? <StatusLine state="idle">Checks are paused. Nothing is recorded and no alert fires until you resume.</StatusLine>
    : m.state === 'error'
      ? <StatusLine state="error">Down for 30 minutes at 20:30, right after <Phrase onClick={() => go('api-gateway')}>dep_91a</Phrase>: connection refused from all 3 regions. Up again since dep_91b; 60 checks failed.</StatusLine>
      : m.state === 'warn'
        ? <StatusLine state="warn">Answering, but p95 is {(m.p95 / 1000).toFixed(1)}s over the last hour, above the 1s threshold. Slow is not down: the status page shows "degraded".</StatusLine>
        : <StatusLine state="ok">Up. Every check in the last {range} passed; p95 {m.p95} ms.</StatusLine>
  const facts: KV[] = [
    { k: `uptime ${range}`, v: `${up[range].toFixed(2)}%`, mono: true, state: up[range] < 99.9 ? 'warn' : undefined },
    { k: 'p50', v: `${m.p50} ms`, mono: true }, { k: 'p95', v: `${m.p95} ms`, mono: true, state: m.p95 > 1000 ? 'warn' : undefined },
    { k: 'last check', v: paused ? 'paused' : '12s ago · 200 · 96 ms', mono: true },
    { k: 'incidents 30d', v: String(INCIDENTS.length), mono: true, state: INCIDENTS.length ? 'warn' : undefined },
    { k: 'on the status page', v: 'API · public', mono: true },
  ]
  return (
    <Detail title={m.name} meta={`${m.type} · every ${m.every} · acme-storefront · production`} status={status}
      lede={<Lede state={paused ? 'idle' : m.state} word={word} facts={facts}><span className="font-mono">{m.url}</span> · checked from fra, iad, sin</Lede>}
      actions={<>
        <Segmented options={[['24h', '24h'], ['7d', '7d'], ['30d', '30d'], ['90d', '90d']] as const} value={range} onChange={setRange} className="h-7 [&>button]:h-7" />
        <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'checking now', `${m.url} · 200 · 94 ms from fra`)}>check now</Button>
        <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => setPaused((p) => !p)}>{paused ? 'resume' : 'pause'}</Button>
      </>}>
      <Columns>
        <div>
          <Section title="Checks" meta={`${range} · ${range === '24h' ? '30 min' : range === '7d' ? '3 h' : '1 day'} per segment`}>
            <div><StatusStrip buckets={m.buckets} height={24} /><p className="mt-1 font-mono text-[11px] text-muted-foreground">● up · ◐ slow · × down · ← → reads a segment</p></div>
          </Section>
          <Section title="Response time" meta="p50 thick · p95 thin · ┆ deploy · 1s threshold">
            <div className="border bg-background p-3">
              <TimeChart data={RT(m)} series={[{ key: 'p95', name: 'p95', width: 1 }, { key: 'p50', name: 'p50', width: 2 }]} unit="ms" height={170} xInterval={7} markers={[{ id: 'dep_91a', x: '20:30' }]} thresholds={[{ y: 1000, label: 'slow', state: 'warn' }]} readoutFormat={(p) => `${p.t} · p50 ${p.p50 ?? '—'} · p95 ${p.p95 ?? '—'} ms`} />
            </div>
          </Section>
          <Section title="Incidents" meta={`${INCIDENTS.length} in 30d · newest first`} action={<a href="/status?project=acme-storefront" target="_blank" rel="noreferrer" className="text-xs">as shown publicly</a>}>
            <Timeline items={INCIDENTS.map((i) => ({ t: i.t, label: i.label, note: i.note, state: i.state, icon: i.icon }))} />
          </Section>
        </div>
        <div>
          <Section title="Check" meta="how it is probed">
            <KeyValue compact rows={[{ k: 'method', v: 'GET', mono: true }, { k: 'expects', v: '200 within 10s', mono: true }, { k: 'from', v: 'fra · iad · sin', mono: true }, { k: 'down when', v: '2 of 3 regions fail twice', mono: true }]} />
          </Section>
          <Section title="Alerts" meta="who hears about it">
            <KeyValue compact rows={[{ k: 'on down', v: 'ops@acme.sh · #incidents', mono: true }, { k: 'on slow', v: 'p95 > 1s for 5 min · #incidents', mono: true }, { k: 'on recovery', v: 'same channels', mono: true }]} />
          </Section>
          <Section title="Status page" meta="public" action={<a href="/status?project=acme-storefront" target="_blank" rel="noreferrer" className="text-xs">open</a>}>
            <KeyValue compact rows={[{ k: 'shown as', v: 'API', mono: true }, { k: 'group', v: 'Platform', mono: true }, { k: 'slow shows as', v: 'degraded', mono: true }, { k: 'page', v: 'status.acme.sh', mono: true, copy: 'https://status.acme.sh' }]} />
          </Section>
          <Section title="Danger">
            <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs text-destructive">delete monitor</Button>} destructive title={`Delete ${m.name}`} description="History and incidents are removed from the status page too. Type the monitor name to confirm." confirmWord={m.name} steps={['stop checks', 'remove from status page', 'delete history']} onDone={() => go('uptime')} />
          </Section>
        </div>
      </Columns>
    </Detail>
  )
}
