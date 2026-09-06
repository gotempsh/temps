// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { Camera, ExternalLink, Inbox, MailCheck, MailOpen, MailX, MousePointerClick, Plus, RefreshCw, Send, TriangleAlert } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { CopyButton } from '@/components/ui/copy-button'
import {
  ChartFooter, Detail, EchoDialog, Field, Kbd, Ledger, Metric, MetricGrid, Num, PageState, Phrase, Picker, Section, SectionTitle, Segmented, KeyValue, Timeline, Settings, Status, StatusLine, TimeChart,
  type KV, type LedgerRow, type Page, type Series, type State, type StatusItem, type TimePoint, type TimeRange,
  Columns, Lede, type TimelineItem,
} from '@/components/op'
import { Toggle } from './ConsoleV5Admin'
import { useFresh } from './console-fresh'

/* ────────────────────────────────────────────────────────────────────────
   Email on v5, from the real console's shapes: EmailProviderResponse
   (provider_type ses | scaleway | smtp, region, is_active),
   EmailDomainResponse (+ DnsRecordResponse: record_type, name, value,
   status unknown | verified | pending | failed), EmailResponse (status
   queued | sent | failed | captured, from/to, subject, provider_message_id,
   error_message, track_opens/track_clicks), EmailStatsResponse (total,
   sent, failed, queued, captured) and EmailTrackingSetupResponse
   (webhook_url, topic_arn, subscription_requested, event_destination_attached).

   What changes against temps/web today (Email.tsx: five equal tabs, cards):
   - Three tabs, not five. "mail" is the work: metrics, the chart, and the
     sent ledger on one page, because they are one question ("what went
     out and what went wrong"). "sending" is the setup: domains and the
     providers behind them, together, because a domain is nothing without
     its provider. "settings" is tracking. The SDK lives in the docs.
   - The verdict comes first: a domain whose DKIM failed, a bounce rate
     above threshold, or "no provider: mail is captured, not sent".
   - The chart is the time filter: drag across it and the ledger under it
     narrows to that window (sent · bounced per hour with deploy markers,
     because a bounce spike after a deploy is the question).
   - Domains lead with the record that is wrong. Each DNS record is a row
     with a copyable value and its own state; "verify now" is the one action.
   - Providers: type, region, domains served, active, default. When none is
     configured the tab onboards ("mail is captured; add a provider to
     send") instead of showing an empty table.
   - SDK docs leave the console for the docs site; a link remains.
   ──────────────────────────────────────────────────────────────────────── */

type Notify = (level: 'ok' | 'warn' | 'err', msg: string, detail?: string) => void

type Provider = { id: number; name: string; provider_type: 'ses' | 'scaleway' | 'smtp'; region: string; is_active: boolean; is_default: boolean; domains: number; created_at: string }
const PROVIDERS: Provider[] = [
  { id: 1, name: 'ses-eu', provider_type: 'ses', region: 'eu-west-1', is_active: true, is_default: true, domains: 2, created_at: '2025-11-02' },
  { id: 2, name: 'smtp-fallback', provider_type: 'smtp', region: 'smtp.resend.com:587', is_active: false, is_default: false, domains: 1, created_at: '2026-03-14' },
]
const PROVIDER_LABEL = { ses: 'Amazon SES', scaleway: 'Scaleway TEM', smtp: 'SMTP' } as const

type DnsRecord = { record_type: 'TXT' | 'CNAME' | 'MX'; name: string; value: string; priority?: number; status: 'verified' | 'pending' | 'failed' | 'unknown'; purpose: string }
type Domain = { id: number; domain: string; provider_id: number; status: 'verified' | 'pending' | 'failed'; last_verified_at: string | null; verification_error: string | null; sent_30d: number; records: DnsRecord[] }
const DOMAINS: Domain[] = [
  {
    id: 1, domain: 'acme.sh', provider_id: 1, status: 'verified', last_verified_at: '12m ago', verification_error: null, sent_30d: 1102,
    records: [
      { record_type: 'TXT', name: 'acme.sh', value: 'v=spf1 include:amazonses.com ~all', status: 'verified', purpose: 'SPF' },
      { record_type: 'CNAME', name: 'k7d2._domainkey.acme.sh', value: 'k7d2.dkim.amazonses.com', status: 'verified', purpose: 'DKIM 1/3' },
      { record_type: 'CNAME', name: 'p1xq._domainkey.acme.sh', value: 'p1xq.dkim.amazonses.com', status: 'verified', purpose: 'DKIM 2/3' },
      { record_type: 'CNAME', name: 'zz8m._domainkey.acme.sh', value: 'zz8m.dkim.amazonses.com', status: 'verified', purpose: 'DKIM 3/3' },
      { record_type: 'TXT', name: '_dmarc.acme.sh', value: 'v=DMARC1; p=quarantine; rua=mailto:dmarc@acme.sh', status: 'verified', purpose: 'DMARC' },
      { record_type: 'MX', name: 'bounce.acme.sh', value: 'feedback-smtp.eu-west-1.amazonses.com', priority: 10, status: 'verified', purpose: 'bounce MX' },
    ],
  },
  {
    id: 2, domain: 'notify.acme.sh', provider_id: 1, status: 'pending', last_verified_at: '3m ago', verification_error: null, sent_30d: 168,
    records: [
      { record_type: 'TXT', name: 'notify.acme.sh', value: 'v=spf1 include:amazonses.com ~all', status: 'verified', purpose: 'SPF' },
      { record_type: 'CNAME', name: 'a1b2._domainkey.notify.acme.sh', value: 'a1b2.dkim.amazonses.com', status: 'verified', purpose: 'DKIM 1/3' },
      { record_type: 'CNAME', name: 'c3d4._domainkey.notify.acme.sh', value: 'c3d4.dkim.amazonses.com', status: 'verified', purpose: 'DKIM 2/3' },
      { record_type: 'CNAME', name: 'e5f6._domainkey.notify.acme.sh', value: 'e5f6.dkim.amazonses.com', status: 'pending', purpose: 'DKIM 3/3' },
      { record_type: 'TXT', name: '_dmarc.notify.acme.sh', value: 'v=DMARC1; p=none', status: 'unknown', purpose: 'DMARC' },
    ],
  },
  {
    id: 3, domain: 'mail.acme-storefront.com', provider_id: 2, status: 'failed', last_verified_at: '41m ago', verification_error: 'SPF record found but does not include the provider: v=spf1 include:_spf.google.com ~all', sent_30d: 14,
    records: [
      { record_type: 'TXT', name: 'mail.acme-storefront.com', value: 'v=spf1 include:_spf.google.com include:spf.resend.com ~all', status: 'failed', purpose: 'SPF' },
      { record_type: 'CNAME', name: 'resend._domainkey.mail.acme-storefront.com', value: 'resend._domainkey.resend.com', status: 'verified', purpose: 'DKIM' },
      { record_type: 'TXT', name: '_dmarc.mail.acme-storefront.com', value: 'v=DMARC1; p=none', status: 'unknown', purpose: 'DMARC' },
    ],
  },
]
const DOMAIN_STATE: Record<Domain['status'] | DnsRecord['status'], State> = { verified: 'ok', pending: 'warn', failed: 'error', unknown: 'idle' }

type Mail = { id: string; to: string; subject: string; from: string; status: 'queued' | 'sent' | 'delivered' | 'opened' | 'bounced' | 'failed' | 'captured'; project: string; env: string; at: string; atNum: number; error?: string; provider_message_id?: string; tags?: string[] }
const MAILS: Mail[] = [
  { id: 'em_9f31', to: 'jules@example.com', subject: 'Your order #48211 has shipped', from: 'orders@acme.sh', status: 'opened', project: 'acme-storefront', env: 'production', at: '2m ago', atNum: 2, provider_message_id: '0102018e-9f31-4c1a', tags: ['order', 'shipping'] },
  { id: 'em_9f2c', to: 'mira@northwind.io', subject: 'Reset your password', from: 'no-reply@acme.sh', status: 'delivered', project: 'api-gateway', env: 'production', at: '6m ago', atNum: 6, provider_message_id: '0102018e-9f2c-88a0', tags: ['auth'] },
  { id: 'em_9f1a', to: 'ops@oldmail.example', subject: 'Invoice INV-2026-0912', from: 'billing@acme.sh', status: 'bounced', project: 'billing-worker', env: 'production', at: '14m ago', atNum: 14, error: 'Hard bounce · 550 5.1.1 mailbox does not exist', provider_message_id: '0102018e-9f1a-1b77', tags: ['invoice'] },
  { id: 'em_9f0d', to: 'team@acme.sh', subject: 'Weekly digest · 41 deploys, 2 incidents', from: 'digest@notify.acme.sh', status: 'delivered', project: 'docs', env: 'production', at: '1h ago', atNum: 60, provider_message_id: '0102018e-9f0d-02de', tags: ['digest'] },
  { id: 'em_9efc', to: 'sam@example.com', subject: 'Verify your email address', from: 'no-reply@mail.acme-storefront.com', status: 'failed', project: 'acme-storefront', env: 'production', at: '1h ago', atNum: 63, error: 'Provider rejected: 554 Message rejected: Email address is not verified (domain mail.acme-storefront.com)', tags: ['auth'] },
  { id: 'em_9ef0', to: 'dev@acme.sh', subject: 'Verify your email address', from: 'no-reply@acme.sh', status: 'captured', project: 'acme-storefront', env: 'pr-212', at: '2h ago', atNum: 120, tags: ['auth'] },
  { id: 'em_9ee2', to: 'ana@example.org', subject: 'Your order #48190 has shipped', from: 'orders@acme.sh', status: 'delivered', project: 'acme-storefront', env: 'production', at: '3h ago', atNum: 180, provider_message_id: '0102018e-9ee2-7c41', tags: ['order', 'shipping'] },
  { id: 'em_9ed9', to: 'finance@contoso.example', subject: 'Invoice INV-2026-0911', from: 'billing@acme.sh', status: 'delivered', project: 'billing-worker', env: 'production', at: '5h ago', atNum: 300, provider_message_id: '0102018e-9ed9-5d10', tags: ['invoice'] },
  { id: 'em_9ec4', to: 'alerts@acme.sh', subject: '[temps] billing-worker failing health checks', from: 'alerts@notify.acme.sh', status: 'queued', project: 'temps', env: '—', at: 'now', atNum: 0, tags: ['alert'] },
  { id: 'em_9eb0', to: 'lee@example.net', subject: 'Welcome to Acme', from: 'hello@acme.sh', status: 'delivered', project: 'acme-web', env: 'production', at: '9h ago', atNum: 540, provider_message_id: '0102018e-9eb0-e3a2', tags: ['onboarding'] },
]
// The ten above are the interesting ones; the rest of the 30 days is routine mail, generated so the ledger has something to page through.
const ROUTINE_SUBJECTS = ['Your order #{n} has shipped', 'Reset your password', 'Welcome to Acme', 'Invoice INV-2026-{n}', 'Your receipt for order #{n}', 'Verify your email address', 'Weekly digest']
const ROUTINE_FROM = ['orders@acme.sh', 'no-reply@acme.sh', 'hello@acme.sh', 'billing@acme.sh', 'orders@acme.sh', 'no-reply@acme.sh', 'digest@notify.acme.sh']
const ROUTINE_PROJECT = ['acme-storefront', 'api-gateway', 'acme-web', 'billing-worker', 'acme-storefront', 'acme-storefront', 'docs']
for (let i = 0; i < 118; i++) {
  const k = i % ROUTINE_SUBJECTS.length
  const n = 48180 - i * 3
  const hoursAgo = 10 + Math.floor(i * 7.2)
  MAILS.push({ id: `em_${(0x9ea0 - i).toString(16)}`, to: `${['sam', 'ana', 'lee', 'kim', 'noor', 'ivo', 'tess', 'raj'][i % 8]}${i}@example.${['com', 'org', 'net'][i % 3]}`, subject: ROUTINE_SUBJECTS[k].replace('{n}', String(n)), from: ROUTINE_FROM[k], status: i % 23 === 11 ? 'bounced' : i % 9 === 4 ? 'opened' : 'delivered', project: ROUTINE_PROJECT[k], env: 'production', at: hoursAgo < 24 ? `${hoursAgo}h ago` : `${Math.floor(hoursAgo / 24)}d ago`, atNum: hoursAgo * 60, error: i % 23 === 11 ? 'Hard bounce · 550 5.1.1 mailbox does not exist' : undefined, provider_message_id: `0102018e-${(0x9ea0 - i).toString(16)}-0000`, tags: [k === 3 ? 'invoice' : 'order'] })
}
const MAIL_STATE: Record<Mail['status'], State> = { queued: 'idle', sent: 'ok', delivered: 'ok', opened: 'ok', bounced: 'error', failed: 'error', captured: 'sampled' }
/** One icon per email event, everywhere it is drawn. The icon says what happened; colour only marks failure. */
type MailEvent = 'queued' | 'sent' | 'delivered' | 'opened' | 'clicked' | 'bounced' | 'failed' | 'captured'
const MAIL_EVENT_ICONS: Record<MailEvent, ReactNode> = {
  queued: <Inbox />, sent: <Send />, delivered: <MailCheck />, opened: <MailOpen />, clicked: <MousePointerClick />,
  bounced: <MailX />, failed: <TriangleAlert />, captured: <Camera />,
}
const MAIL_EVENT_STATE: Partial<Record<MailEvent, State>> = { bounced: 'error', failed: 'error', captured: 'sampled', queued: 'idle' }
const MAIL_WORD: Record<Mail['status'], string> = { queued: 'queued', sent: 'sent', delivered: 'delivered', opened: 'opened', bounced: 'bounced', failed: 'failed', captured: 'captured · not sent' }

const STATS = { total: 1284, delivered: 1259, bounced: 15, complained: 2, failed: 8, queued: 1, captured: 37, opened: 512, clicked: 141 }
const NOW_H = 19
const HOURS = Array.from({ length: 10 }, (_, i) => `${String(NOW_H - 9 + i).padStart(2, '0')}:00`)
const SERIES: TimePoint[] = HOURS.map((t, i) => ({ t, sent: [38, 41, 44, 39, 47, 52, 49, 58, 61, 55][i], bounced: [0, 1, 0, 0, 1, 0, 0, 6, 4, 3][i] }))
/** Axis label of the hour an email was sent in, from minutes ago. */
const hourOf = (minutesAgo: number) => `${String(Math.max(NOW_H - 9, NOW_H - Math.floor(minutesAgo / 60))).padStart(2, '0')}:00`
const inRange = (m: Mail, r: TimeRange | null) => !r || (HOURS.indexOf(hourOf(m.atNum)) >= HOURS.indexOf(r.from) && HOURS.indexOf(hourOf(m.atNum)) <= HOURS.indexOf(r.to))

/** The two lines on the mail chart, declared once so the chart and its legend cannot drift apart. */
const MAIL_SERIES: Series[] = [{ key: 'sent', name: 'sent' }, { key: 'bounced', name: 'bounced' }]
/**
 * A chart legend is drawn from the series it describes, never typed by hand: same order, same
 * name, same stroke and width TimeChart uses (series[0] is chart-1 at 1.5, the rest chart-2 at 1).
 */
function SeriesLegend({ series }: { series: Series[] }) {
  return (
    <>
      {series.map((s, i) => (
        <span key={s.key} className="inline-flex items-center gap-1.5">
          <span
            aria-hidden
            className="inline-block w-4 shrink-0"
            style={{ borderTopStyle: 'solid', borderTopWidth: s.width ?? (i === 0 ? 1.5 : 1), borderTopColor: s.stroke ?? (i === 0 ? 'var(--chart-1)' : 'var(--chart-2)') }}
          />
          {s.name}
        </span>
      ))}
    </>
  )
}

const TABS = ['mail', 'domains', 'providers', 'settings'] as const
type Tab = (typeof TABS)[number]

export function EmailScreen({ dense, notify, go, initialTab = 'mail' }: { dense: boolean; notify: Notify; go: (v: string) => void; initialTab?: Tab }) {
  const [tab, setTab] = useState<Tab>(initialTab)
  const [q, setQ] = useState('')
  const [statusFilter, setStatusFilter] = useState<string>('all')
  const [range, setRange] = useState<TimeRange | null>(null)
  const [pageNo, setPageNo] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  // Fresh install (shell toggle, `?fresh=1`): no provider, no domain, nothing sent. Every tab must onboard rather than go blank.
  const [fresh] = useFresh()
  const mails: Mail[] = fresh ? [] : MAILS
  const domains: Domain[] = fresh ? [] : DOMAINS
  const [providers, setProviders] = useState(fresh ? [] : PROVIDERS)
  const [tracking, setTracking] = useState({ opens: true, clicks: true })
  const [savedTracking, setSavedTracking] = useState(tracking)

  const failedDomain = domains.find((d) => d.status === 'failed')
  const pendingDomain = domains.find((d) => d.status === 'pending')
  const bounceRate = (STATS.bounced / STATS.total) * 100
  const noProvider = providers.filter((p) => p.is_active).length === 0
  const items: StatusItem[] = []
  if (bounceRate > 1) items.push({ state: 'warn', children: <>Bounce rate <Num value={bounceRate.toFixed(1)} unit="%" /> over 30d, above the 1% threshold, most since dep_91a.</> })
  if (pendingDomain) items.push({ state: 'warn', children: <><Phrase onClick={() => go(`domain:${pendingDomain.id}`)}>{pendingDomain.domain}</Phrase> is waiting on one DKIM record.</> })
  if (STATS.captured > 0) items.push({ state: 'sampled', children: <>{STATS.captured} preview emails were captured, not sent.</> })
  const status = noProvider
    ? (fresh
      ? <StatusLine state="idle">Nothing has been sent yet. Mail from any project is captured here even before a provider exists.</StatusLine>
      : <StatusLine state="warn">No active provider: mail is captured, not sent. <Phrase onClick={() => setTab('providers')}>Add a provider</Phrase>.</StatusLine>)
    : failedDomain
      ? <StatusLine state="error" more={items.length ? { label: `+${items.length} warning${items.length > 1 ? 's' : ''}`, items } : undefined}><Phrase onClick={() => go(`domain:${failedDomain.id}`)}>{failedDomain.domain}</Phrase> failed verification: SPF does not include the provider.</StatusLine>
      : <StatusLine state={items.length ? 'warn' : 'ok'} more={items.length > 1 ? { label: `+${items.length - 1} more`, items: items.slice(1) } : undefined}>{items[0]?.children ?? 'All domains verified, deliveries normal.'}</StatusLine>

  const filtered = mails.filter((m) => inRange(m, range) && (statusFilter === 'all' || (statusFilter === 'problems' ? m.status === 'bounced' || m.status === 'failed' : m.status === statusFilter)) && (!q || `${m.to} ${m.subject} ${m.from} ${m.project}`.toLowerCase().includes(q.toLowerCase())))
  // Server-side in the real console (page, page_size on ListEmailsQuery); here the filtered set is sliced the same way.
  const pageOf = <T,>(xs: T[]) => xs.slice((pageNo - 1) * pageSize, pageNo * pageSize)
  const mailPage: Page = { page: pageNo, pageSize, total: filtered.length, onPage: setPageNo, onPageSize: (n) => { setPageSize(n); setPageNo(1) } }
  const mailRows: LedgerRow[] = pageOf(filtered).map((m) => ({
    id: m.id,
    state: MAIL_STATE[m.status],
    onOpen: () => go(`email:${m.id}`),
    sort: { to: m.to, subject: m.subject, status: MAIL_WORD[m.status], from: m.from, at: m.atNum },
    cells: [
      <span key="to" className="truncate font-mono">{m.to}</span>,
      <span key="subj" className="truncate">{m.subject}</span>,
      <span key="st" className="min-w-0 truncate"><Status state={MAIL_STATE[m.status]} label={MAIL_WORD[m.status]} />{m.error && <span className="ml-2 text-muted-foreground">{m.error.split(' · ')[0]}</span>}</span>,
      <span key="from" className="truncate font-mono text-muted-foreground">{m.from}</span>,
      <span key="proj" className="truncate text-muted-foreground">{m.project} <span className="opacity-70">{m.env}</span></span>,
      <span key="at" className="text-muted-foreground">{m.at}</span>,
    ],
    mobile: <><span className="font-mono">{m.to}</span> · {m.subject}<span className="block text-muted-foreground"><Status state={MAIL_STATE[m.status]} label={MAIL_WORD[m.status]} /> · {m.at}</span></>,
  }))

  const domainRows: LedgerRow[] = domains.map((d) => {
    const bad = d.records.filter((r) => r.status !== 'verified')
    const prov = providers.find((p) => p.id === d.provider_id)
    return {
      id: String(d.id), state: DOMAIN_STATE[d.status], onOpen: () => go(`domain:${d.id}`),
      sort: { domain: d.domain, status: d.status, sent: d.sent_30d },
      cells: [
        <span key="d" className="truncate font-mono font-medium">{d.domain}</span>,
        <span key="s" className="min-w-0 truncate"><Status state={DOMAIN_STATE[d.status]} label={d.status} />{d.status !== 'verified' && bad[0] && <span className="ml-2 text-muted-foreground">{bad.length === 1 ? `${bad[0].purpose} ${bad[0].status}` : `${bad.length} records not verified`}</span>}</span>,
        <span key="r" className="flex gap-1 font-mono text-[11px]">{d.records.map((r) => <span key={r.name} title={`${r.purpose} · ${r.status}`} className={DOMAIN_STATE[r.status] === 'ok' ? 'text-success' : DOMAIN_STATE[r.status] === 'error' ? 'text-destructive' : DOMAIN_STATE[r.status] === 'warn' ? 'text-warning' : 'text-muted-foreground'}>{r.status === 'verified' ? '●' : r.status === 'failed' ? '×' : r.status === 'pending' ? '◐' : '○'}</span>)}</span>,
        <span key="p" className="truncate text-muted-foreground">{prov?.name ?? '—'}</span>,
        <Num key="n" value={d.sent_30d} />,
        <span key="v" className="text-muted-foreground">{d.last_verified_at ?? 'never'}</span>,
      ],
      mobile: <><span className="font-mono">{d.domain}</span><span className="block text-muted-foreground"><Status state={DOMAIN_STATE[d.status]} label={d.status} />{bad[0] && d.status !== 'verified' && ` · ${bad[0].purpose} ${bad[0].status}`}</span></>,
    }
  })

  const setActive = (p: Provider, on: boolean) => { setProviders((ps) => ps.map((x) => x.id === p.id ? { ...x, is_active: on } : x)); notify('ok', `${p.name} ${on ? 'activated' : 'deactivated'}`) }
  // Deactivating the only active provider stops all outbound mail: that transition is confirmed, the others are one click.
  const toggleActive = (p: Provider) => {
    const last = p.is_active && providers.filter((x) => x.is_active).length === 1
    const cls = 'text-muted-foreground underline underline-offset-4 hover:text-foreground'
    if (!last) return <button type="button" className={cls} onClick={(e) => { e.stopPropagation(); setActive(p, !p.is_active) }}>{p.is_active ? 'deactivate' : 'activate'}</button>
    return <EchoDialog trigger={<button type="button" className={cls} onClick={(e) => e.stopPropagation()}>deactivate</button>} echo={`$ temps email provider deactivate ${p.name}`} title="Deactivate the last active provider" description="No provider will be left to send. Every message from every project is captured in the console and never leaves this server until a provider is active again." confirmWord={p.name} steps={['deactivate provider', 'switch sending to capture']} onDone={() => setActive(p, false)} />
  }
  const providerRows: LedgerRow[] = providers.map((p) => ({
    id: String(p.id), state: p.is_active ? 'ok' : 'idle',
    sort: { name: p.name, type: p.provider_type, domains: p.domains },
    cells: [
      <span key="n" className="truncate font-mono font-medium">{p.name}{p.is_default && <span className="ml-2 border px-1 text-[10px] font-normal text-muted-foreground">default</span>}</span>,
      <span key="t" className="truncate">{PROVIDER_LABEL[p.provider_type]}</span>,
      <span key="r" className="truncate font-mono text-muted-foreground">{p.region}</span>,
      <Num key="d" value={p.domains} />,
      <span key="a"><Status state={p.is_active ? 'ok' : 'idle'} label={p.is_active ? 'active' : 'inactive'} /></span>,
      <span key="x" className="flex justify-end gap-2 text-[11px]">
        <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={() => notify('ok', 'test email sent', `${p.name} → maya@acme.sh · accepted by ${PROVIDER_LABEL[p.provider_type]}`)}>send test</button>
        {toggleActive(p)}
      </span>,
    ],
    mobile: <><span className="font-mono">{p.name}</span> · {PROVIDER_LABEL[p.provider_type]}<span className="block text-muted-foreground">{p.region} · {p.is_active ? 'active' : 'inactive'}</span><span className="mt-1 flex gap-3 text-[11px]"><button type="button" className="underline underline-offset-4" onClick={(e) => { e.stopPropagation(); notify('ok', 'test email sent', `${p.name} → maya@acme.sh`) }}>send test</button>{toggleActive(p)}</span></>,
  }))

  const deliveredPct = ((STATS.delivered / STATS.total) * 100).toFixed(1)
  const openPct = ((STATS.opened / STATS.delivered) * 100).toFixed(0)
  const trackingDirty = JSON.stringify(tracking) !== JSON.stringify(savedTracking)

  return (
    <Detail title="Email" meta={`${providers.filter((p) => p.is_active).length} active provider${providers.filter((p) => p.is_active).length === 1 ? '' : 's'} · ${domains.length} domains · ${(fresh ? 0 : STATS.total).toLocaleString()} sent · 30d`} status={status} tabs={TABS} tab={tab} onTab={(t) => { setTab(t); setQ('') }}
      actions={<a href="https://temps.sh/docs/email" target="_blank" rel="noreferrer" className="inline-flex h-7 items-center gap-1 text-xs text-muted-foreground hover:text-foreground">SDK docs <ExternalLink className="h-3 w-3" /></a>}>
      {tab === 'mail' && mails.length === 0 && !q && statusFilter === 'all' && !range && (
        <PageState state="unconfigured" title="Nothing has been sent yet"
          missing="No project has called the email API. Send one from a project with the SDK or the REST endpoint; it is captured here even before a provider is configured, so you can read it and check the rendering."
          example={/* a grid, not a stack: the <pre> is then a grid item, so its automatic minimum size is 0 and the sample scrolls inside its own frame instead of widening the page on a phone */
            <div className="grid min-w-0 grid-cols-1 gap-2 font-mono text-[11px]"><p className="min-w-0 break-words">● orders@acme.sh → jules@example.com · Your order #48180 has shipped · delivered · 1.2s via ses-eu</p><pre className="op-inset max-w-full min-w-0 overflow-x-auto border px-3 py-2">{`curl -X POST $TEMPS_URL/api/email/send \\\n  -H "Authorization: Bearer $TEMPS_TOKEN" \\\n  -d '{"to":"jules@example.com","subject":"Hello","text":"It works."}'`}</pre><p className="text-muted-foreground">then: <a href="#" onClick={(e) => { e.preventDefault(); setTab('providers') }}>add a provider</a> so it leaves the server, and <a href="#" onClick={(e) => { e.preventDefault(); setTab('domains') }}>verify a domain</a> so it lands in the inbox</p></div>}
          settingsHref="/docs/email" settingsLabel="send your first email" />
      )}
      {tab === 'mail' && !(mails.length === 0 && !q && statusFilter === 'all' && !range) && (
        <div className="space-y-4">
          <MetricGrid cols={4}>
            <Metric label="sent · 30d" value={STATS.total.toLocaleString()} delta="+18%" baseline="vs previous 30d" />
            <Metric label="delivered" value={deliveredPct} unit="%" baseline={`${STATS.delivered.toLocaleString()} of ${STATS.total.toLocaleString()}`} />
            <Metric label="bounced" value={STATS.bounced} delta={`${bounceRate.toFixed(1)}%`} baseline="threshold 1% · 11 since dep_91a" state="warn" />
            <Metric label="opened" value={openPct} unit="%" baseline={`${STATS.clicked} clicked · tracking on`} />
          </MetricGrid>
          <div className="border p-3">
            <TimeChart data={SERIES} series={MAIL_SERIES} markers={[{ id: 'dep_90e', x: HOURS[2] }, { id: 'dep_91a', x: HOURS[7], note: 'billing-worker · invoice template' }]} unit="/h" height={160} onOpen={(id) => go(`deploy:${id}`)} selection={range} onSelect={(r) => { setRange(r); setPageNo(1) }} />
            <ChartFooter><span>last 10h · hourly</span><SeriesLegend series={MAIL_SERIES} /><span>┆ deploy</span><span className="ml-auto">drag to narrow the list below</span></ChartFooter>
          </div>
        <Ledger status={null} dense={dense}
          columns={[{ label: 'to', key: 'to' }, { label: 'subject', key: 'subject' }, { label: 'status', key: 'status' }, { label: 'from', key: 'from' }, 'project', { label: 'when', key: 'at', numeric: true }]}
          grid="minmax(8rem,1.2fr) minmax(10rem,2fr) minmax(9rem,1.6fr) minmax(8rem,1.2fr) minmax(90px,max-content) minmax(60px,max-content)"
          rows={mailRows} total={mails.length} filter={q} onFilter={(v) => { setQ(v); setPageNo(1) }} page={mailPage} placeholder="filter by recipient, subject, sender or project" hint={range ? `${filtered.length} in ${range.from} → ${range.to} · clear the selection on the chart to see all` : `${STATS.queued} queued · ${STATS.captured} captured from previews`}
          action={
            <Picker skin="operator ink v4 v5" value={statusFilter} onChange={(v) => { setStatusFilter(v); setPageNo(1) }} placeholder="status" options={[{ value: 'all', meta: `${mails.length}` }, { value: 'problems', meta: `${mails.filter((m) => m.status === 'bounced' || m.status === 'failed').length}`, state: 'error' }, { value: 'delivered' }, { value: 'opened' }, { value: 'bounced', state: 'error' }, { value: 'failed', state: 'error' }, { value: 'queued', state: 'idle' }, { value: 'captured', state: 'sampled' }]} />
          }
          state={filtered.length === 0 ? <PageState state="empty" title="No emails match" reason={`Nothing ${statusFilter === 'all' ? '' : statusFilter + ' '}matches “${q}”.`} next={<button type="button" className="underline underline-offset-4" onClick={() => { setQ(''); setStatusFilter('all') }}>clear filters</button>} /> : undefined} />
        </div>
      )}

      {/* One Ledger per screen (brand §6 Taste). Domains and providers are two resources, so they are two facets, not two tables on one tab. */}
      {tab === 'domains' && domains.length === 0 && (
        <PageState state="unconfigured" title="No sending domain"
          missing="Mail sent from an unverified domain is marked as spam or rejected. Add the domain you send from; the console tells you the exact DNS records (SPF, DKIM, DMARC) to create and checks them for you."
          example={<div className="font-mono text-[11px]"><p>● acme.sh · verified · SPF DKIM DMARC · ses-eu · 1,102 sent · 30d</p><p className="text-muted-foreground">◐ notify.acme.sh · pending · 2 records not verified · we check every 10 minutes</p></div>}
          settingsHref="/email/domains/new" settingsLabel="add a domain" />
      )}
      {tab === 'domains' && domains.length > 0 && (
        <Ledger status={null} dense={dense}
          columns={[{ label: 'domain', key: 'domain' }, { label: 'status', key: 'status' }, 'records', 'provider', { label: 'sent · 30d', key: 'sent', numeric: true }, 'verified']}
          grid="minmax(10rem,1.5fr) minmax(10rem,2fr) minmax(70px,max-content) minmax(80px,max-content) minmax(70px,max-content) minmax(70px,max-content)"
          rows={domainRows} total={domains.length} filter={q} onFilter={setQ} placeholder="filter domains" hint="● verified · ◐ pending · × failed · ○ not checked"
          action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'add domain', 'would open the add-domain flow: domain, provider, then the records to create')}><Plus /> add domain</Button>} />
      )}

      {tab === 'providers' && (providers.length === 0
        ? <PageState state="unconfigured" title="No email provider" missing="Mail is captured in the console and never leaves this server. Add Amazon SES, Scaleway TEM or an SMTP server to send." example={<span className="font-mono">orders@acme.sh → jules@example.com · delivered in 1.2s via Amazon SES</span>} settingsHref="/settings/email" settingsLabel="add a provider" />
        : <Ledger status={null} dense={dense}
            columns={[{ label: 'provider', key: 'name' }, { label: 'type', key: 'type' }, 'region · host', { label: 'domains', key: 'domains', numeric: true }, 'state', '']}
            grid="minmax(9rem,1.4fr) minmax(90px,max-content) minmax(10rem,1.6fr) minmax(60px,max-content) minmax(70px,max-content) minmax(140px,max-content)"
            rows={providerRows} total={providers.length} filter={q} onFilter={setQ} placeholder="filter providers" hint="the default provider sends for domains that name no provider"
            action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'add provider', 'SES · Scaleway TEM · SMTP')}><Plus /> add provider</Button>} />
      )}

      {tab === 'settings' && (
        <Settings status={null} dirty={trackingDirty} onSave={() => { setSavedTracking(tracking); notify('ok', 'tracking settings saved', 'applies to emails sent from now on') }}
          sections={[
            { title: 'tracking', body: <>
              <Field label="open tracking" help="a 1×1 pixel per email; opens are approximate (image blocking, prefetch)"><Toggle checked={tracking.opens} onChange={(v) => setTracking({ ...tracking, opens: v })} /></Field>
              <Field label="click tracking" help="links are rewritten through this server and redirect; unsubscribe links are never rewritten"><Toggle checked={tracking.clicks} onChange={(v) => setTracking({ ...tracking, clicks: v })} /></Field>
            </> },
            { title: 'delivery events · ses-eu', body: <>
              <Field label="webhook" help="the provider posts delivery, bounce and complaint events here"><span className="flex min-w-0 items-center gap-2"><Input readOnly value="https://temps.acme.sh/api/email/events/ses" className="h-8 min-w-0 flex-1 font-mono text-xs" /><CopyButton value="https://temps.acme.sh/api/email/events/ses" minimal label="Copy webhook URL" className="h-8 w-8 border" /></span></Field>
              <Field label="SNS topic" help="created by Temps in eu-west-1; the subscription was confirmed 2025-11-02"><span className="font-mono text-xs">arn:aws:sns:eu-west-1:••••:temps-email-events</span></Field>
              <Field label="event destination" help="attached to the SES configuration set; without it bounces never arrive"><Status state="ok" label="attached · last event 14m ago" /></Field>
            </> },
          ]}
          danger={
            <EchoDialog destructive trigger={<Button size="sm" variant="outline" className="h-7 border-destructive text-xs text-destructive">disable tracking and delete events</Button>} title="Delete tracking data" description="Deletes 512 open and 141 click events from the last 30 days and turns both trackers off. Sent emails are kept." confirmWord="delete tracking" steps={['turn trackers off', 'delete events', 'rewrite nothing']} onDone={() => { setTracking({ opens: false, clicks: false }); setSavedTracking({ opens: false, clicks: false }); notify('warn', 'tracking disabled', '653 events deleted') }} />
          } />
      )}
    </Detail>
  )
}

/* ── One email: its events, content, headers ───────────────────────── */

export function EmailDetailScreen({ id, notify, go }: { id: string; dense: boolean; notify: Notify; go: (v: string) => void }) {
  const m = MAILS.find((x) => x.id === id) ?? MAILS[0]
  const events = ((): TimelineItem[] => {
    const ev = (kind: MailEvent, note: string, t = m.at): TimelineItem => ({ t, label: kind, icon: MAIL_EVENT_ICONS[kind], state: MAIL_EVENT_STATE[kind], note })
    const base = [ev('queued', `by ${m.project} · ${m.env}`)]
    if (m.status === 'captured') return [...base, ev('captured', 'preview environment: not handed to a provider; readable below')]
    if (m.status === 'queued') return base
    // The provider's words are quoted once, in the Lede; here the timeline says which event carried them.
    if (m.status === 'failed') return [...base, ev('failed', 'the provider rejected the message; its words are above')]
    const sent = [...base, ev('sent', `accepted by ses-eu · ${m.provider_message_id}`)]
    if (m.status === 'bounced') return [...sent, ev('bounced', 'bounce notification from the provider; its words are above')]
    const delivered = [...sent, ev('delivered', 'provider delivery event')]
    if (m.status === 'opened') return [...delivered, ev('opened', 'Apple Mail · Lisbon · first of 2', '1m ago')]
    return delivered
  })()
  // The provider's words are quoted once, in the Lede. The verdict never repeats them: it says what to do about them.
  const status = m.status === 'bounced'
    ? <StatusLine state="error">Nothing retries a hard bounce. Correct the address where it is stored, then send again; until then every message to it is dropped.</StatusLine>
    : m.status === 'failed'
      ? <StatusLine state="error"><Phrase onClick={() => go('domain:3')}>Verify the domain</Phrase>, then resend. No other message from it will leave until the records pass.</StatusLine>
      : m.status === 'captured'
        ? <StatusLine state="sampled">Nothing to do: preview environments never hand mail to a provider. Read the rendering below to check it.</StatusLine>
        : m.status === 'queued'
          ? <StatusLine state="idle">Nothing to do yet: mail usually leaves within seconds. If it is still here in a few minutes, check the provider.</StatusLine>
          : <StatusLine state="ok">Nothing to do: nothing further happens to a delivered message.</StatusLine>
  // Reference only: what is in the meta (id, project, env) or the lede (to, from, provider, timing) is not repeated here.
  const headers: KV[] = [
    { k: 'provider message id', v: m.provider_message_id ?? '—', copy: m.provider_message_id },
    { k: 'reply-to', v: 'support@acme.sh' },
    { k: 'tags', v: m.tags?.join(', ') ?? '—', mono: false },
    { k: 'size', v: '4.1 KB · text/html + text/plain' },
    { k: 'tracking', v: `opens ${m.status === 'captured' ? 'off' : 'on'} · clicks ${m.status === 'captured' ? 'off' : 'on'}`, mono: false, state: m.status === 'captured' ? 'idle' : 'ok' },
  ]

  // One record, one page, no tabs (handoff §7 "record page"): sections in reading order, each a title plus one body,
  // ink rules between sections, soft rules between rows. Left: what happened, then the facts. Right: what was sent.
  const last = events[events.length - 1]
  const [view, setView] = useState<'html' | 'text'>('html')
  const sentLike = m.status !== 'captured' && m.status !== 'queued' && m.status !== 'failed'
  const facts: KV[] = [
    { k: 'to', v: m.to, mono: true, copy: m.to },
    { k: 'from', v: m.from, mono: true },
    { k: 'provider', v: sentLike ? 'ses-eu' : m.status === 'captured' ? 'none · captured' : 'none yet', mono: true, state: m.status === 'captured' ? 'idle' : undefined },
    { k: m.status === 'bounced' ? 'bounced' : m.status === 'failed' ? 'failed' : 'delivered', v: m.status === 'queued' ? 'not yet' : last.t, mono: true, state: m.status === 'bounced' || m.status === 'failed' ? 'error' : undefined },
    { k: 'took', v: sentLike ? '1.2s from queue to delivery' : '—', mono: true },
    { k: 'opens', v: m.status === 'opened' ? '2 · Apple Mail · Lisbon' : m.status === 'captured' ? 'not tracked' : '0', mono: true },
  ]
  const lede = m.status === 'bounced' || m.status === 'failed'
    ? <Lede state="error" word={m.status} facts={facts}>{m.error}{m.status === 'failed' && <> <Phrase onClick={() => go('domain:3')}>Verify the domain</Phrase>.</>}</Lede>
    : m.status === 'captured'
      ? <Lede state="sampled" word="captured" facts={facts}>from {m.env} · never sent; previews do not send mail</Lede>
      : m.status === 'queued'
        ? <Lede state="idle" word="queued" facts={facts}>{m.at} · waiting for ses-eu to accept it</Lede>
        : <Lede state="ok" word={m.status} facts={facts}>{last.t}{m.status === 'opened' ? ' · Apple Mail · Lisbon' : ''} · via ses-eu</Lede>
  return (
    <Detail title={m.subject} meta={`${m.id} · ${m.project} · ${m.env}`} status={status} lede={lede}
      actions={<>
        <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'resent', `${m.subject} → ${m.to}`)}><Send /> resend</Button>
      </>}>
      <Columns>
        <div>
          <Section title="Content" action={<Segmented options={[['html', 'html'], ['text', 'text']] as const} value={view} onChange={setView} className="h-7 [&>button]:h-7" />}>
            {view === 'html'
              ? <div className="border bg-background p-6 text-sm"><p className="font-semibold">{m.subject}</p><p className="mt-2 text-muted-foreground">Track it at <a href="#" onClick={(e) => e.preventDefault()}>acme.sh/orders/48211</a>.</p><p className="mt-4 text-xs text-muted-foreground">— Acme · <a href="#" onClick={(e) => e.preventDefault()}>unsubscribe</a></p></div>
              : <pre className="op-inset whitespace-pre-wrap border px-4 py-3 font-mono text-[11px] leading-5">{`Hi,\n\n${m.subject}.\n\nTrack it at https://acme.sh/orders/48211\n\n— Acme`}</pre>}
          </Section>
          <Section title="Events" meta={`${events.length} · last ${events[events.length - 1].t}`}>
            <Timeline items={events} />
            {m.status === 'bounced' && <p className="mt-3 text-xs text-muted-foreground">Hard bounces suppress the address: further sends to <span className="font-mono">{m.to}</span> are dropped as <span className="font-mono">suppressed</span> until you <a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'address unsuppressed', m.to) }}>remove it from the suppression list</a>.</p>}
          </Section>
        </div>
        <div>
          <Section title="Headers" meta={`${headers.length}`}>
            <KeyValue rows={headers} compact />
          </Section>
        </div>
      </Columns>
    </Detail>
  )
}

/* ── One domain: the records, each with its own state ──────────────── */

/**
 * One record, one page, no tabs (handoff §7 "record page"): the DNS records are the page — they are
 * what the reader came for — and the two settings that govern them are a short form under the ledger.
 * An earlier draft split them into records | settings tabs, which is a facet row over a single record.
 */
export function EmailDomainScreen({ id, dense, notify, go }: { id: string; dense: boolean; notify: Notify; go: (v: string) => void }) {
  const [verifying, setVerifying] = useState(false)
  const [recQ, setRecQ] = useState('')
  const d = DOMAINS.find((x) => String(x.id) === id) ?? DOMAINS[0]
  const prov = PROVIDERS.find((p) => p.id === d.provider_id)
  const bad = d.records.filter((r) => r.status === 'failed' || r.status === 'pending')
  const status = d.status === 'verified'
    ? <StatusLine state="ok">All {d.records.length} records verified {d.last_verified_at}.</StatusLine>
    : d.status === 'failed'
      ? <StatusLine state="error">{d.verification_error}</StatusLine>
      : <StatusLine state="warn">{bad.length} record{bad.length > 1 ? 's' : ''} still pending: {bad.map((r) => r.purpose).join(', ')}. DNS can take up to 48h.</StatusLine>
  const verify = () => { setVerifying(true); setTimeout(() => { setVerifying(false); notify(d.status === 'verified' ? 'ok' : 'warn', `${d.domain} checked`, d.status === 'verified' ? 'all records verified' : `${bad.length} record${bad.length > 1 ? 's' : ''} still ${bad[0]?.status}`) }, 900) }
  const matches = (r: DnsRecord) => { const n = recQ.trim().toLowerCase(); return !n || [r.purpose, r.record_type, r.name, r.value, r.status].some((f) => f.toLowerCase().includes(n)) }
  const shown = d.records.filter(matches)
  const rows: LedgerRow[] = shown.map((r) => ({
    id: r.name, state: DOMAIN_STATE[r.status],
    cells: [
      <span key="p" className="truncate">{r.purpose}</span>,
      <span key="t" className="font-mono">{r.record_type}</span>,
      <span key="n" className="flex min-w-0 items-center gap-1 font-mono"><span className="truncate">{r.name}</span><CopyButton value={r.name} minimal label="Copy name" className="h-5 w-5 shrink-0 text-muted-foreground" /></span>,
      <span key="v" className="flex min-w-0 items-center gap-1 font-mono"><span className="truncate">{r.value}</span><CopyButton value={r.value} minimal label="Copy value" className="h-5 w-5 shrink-0 text-muted-foreground" /></span>,
      <span key="pr" className="font-mono text-muted-foreground">{r.priority ?? '—'}</span>,
      <span key="s"><Status state={DOMAIN_STATE[r.status]} label={r.status === 'unknown' ? 'not checked' : r.status} /></span>,
    ],
    mobile: <><span className="font-mono">{r.purpose} · {r.record_type}</span><span className="block truncate font-mono text-muted-foreground">{r.name}</span><span className="block"><Status state={DOMAIN_STATE[r.status]} label={r.status} /></span></>,
  }))
  const [form, setForm] = useState({ provider: prov?.name ?? 'ses-eu', dmarc: 'quarantine' })
  const [saved, setSaved] = useState(form)
  const dirty = JSON.stringify(form) !== JSON.stringify(saved)
  return (
    <Detail title={d.domain} meta={`${prov ? PROVIDER_LABEL[prov.provider_type] + ' · ' + prov.name : 'no provider'} · ${d.sent_30d.toLocaleString()} sent · 30d`} status={status}
      actions={<Button size="sm" className="op-primary h-7 text-xs" onClick={verify} disabled={verifying}><RefreshCw className={verifying ? 'animate-spin' : ''} /> {verifying ? 'checking DNS…' : 'verify now'}</Button>}>
      <div className="space-y-6">
        {d.status === 'failed' && (
          <div className="op-raise border p-3 text-xs">
            <SectionTitle title="What to change" meta="SPF" />
            <p className="mt-1">Your SPF record lists Google only. Add the provider's include so both can send: one TXT record per domain, both includes in it.</p>
            <div className="mt-2 flex items-center gap-2"><code className="op-inset min-w-0 flex-1 truncate px-2 py-1 font-mono text-[11px]">{d.records[0].value}</code><CopyButton value={d.records[0].value} minimal label="Copy SPF value" className="h-7 w-7 border" /></div>
            <p className="mt-2 text-muted-foreground">Then <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={verify}>verify now</button>. Until it passes, mail from this domain is rejected by the provider (see <Phrase onClick={() => go('email:em_9efc')}>em_9efc</Phrase>).</p>
          </div>
        )}
        <Ledger status={null} dense={dense}
          columns={['record', 'type', 'name', 'value', { label: 'prio', numeric: true }, 'status']}
          grid="minmax(70px,max-content) minmax(50px,max-content) minmax(10rem,1.4fr) minmax(12rem,2fr) minmax(40px,max-content) minmax(90px,max-content)"
          rows={rows} total={d.records.length} filter={recQ} onFilter={setRecQ} placeholder="filter records"
          hint={`checked ${d.last_verified_at ?? 'never'} · ${verifying ? 'checking…' : 'records are re-checked every 15 minutes while any is pending'}`}
          state={shown.length === 0 ? <PageState state="empty" title="No records match" reason={`Nothing in this domain's DNS records matches “${recQ}”.`} next={<button type="button" className="underline underline-offset-4" onClick={() => setRecQ('')}>clear the filter</button>} /> : undefined} />
        <Section title="Sending" meta="who sends for this domain, and what receivers do with failures">
          <div className="@container space-y-4 border bg-background p-4">
            <Field label="provider" help="which provider sends for this domain; changing it needs new DKIM records"><Picker skin="operator ink v4 v5" value={form.provider} onChange={(v) => setForm({ ...form, provider: v })} options={PROVIDERS.map((p) => ({ value: p.name, meta: PROVIDER_LABEL[p.provider_type], state: p.is_active ? 'ok' : 'idle' }))} /></Field>
            <Field label="DMARC policy" help="what receivers do with mail that fails SPF and DKIM; start at none, move to quarantine once reports are clean"><Picker skin="operator ink v4 v5" value={form.dmarc} onChange={(v) => setForm({ ...form, dmarc: v })} options={[{ value: 'none', meta: 'monitor only' }, { value: 'quarantine', meta: 'to spam' }, { value: 'reject', meta: 'drop' }]} /></Field>
            <div className="flex items-center gap-3 border-t pt-3 text-xs">
              <span className={dirty ? undefined : 'text-muted-foreground'}>{dirty ? 'unsaved changes · takes effect on the next send' : 'no changes'}</span>
              <Button size="sm" disabled={!dirty} onClick={() => { setSaved(form); notify('ok', 'domain settings saved', d.domain) }} className="op-primary ml-auto h-7 text-xs">save</Button>
            </div>
          </div>
        </Section>
        <Section title="Danger">
          <EchoDialog destructive trigger={<Button size="sm" variant="outline" className="h-7 border-destructive text-xs text-destructive">remove domain</Button>} title={`Remove ${d.domain}`} description={`Mail from ${d.domain} stops sending immediately; ${d.sent_30d} emails were sent from it in the last 30 days. DNS records are yours and stay where they are.`} confirmWord={d.domain} steps={['remove identity from provider', 'delete domain', 'reject new sends']} onDone={() => { notify('warn', `${d.domain} removed`); go('email') }} />
        </Section>
      </div>
    </Detail>
  )
}

export const EMAIL_KEYS = <><Kbd keys="j" /> down · <Kbd keys="k" /> up</>
