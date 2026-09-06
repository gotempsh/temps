// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Tokens, motion and iconography, rendered from the data that defines them.
//
// The token table below is built from `web/packages/op/tokens.json` at build
// time — not from a copy of it — so a token that exists here exists in the
// stylesheet too (`node web/packages/op/scripts/tokens.mjs check` proves the
// third side of that triangle). Adding a token means editing one file.
//
// Docs: design-system/docs/motion.md, design-system/docs/icons.md.

import { useState, type ComponentType, type ReactNode } from 'react'
import {
  Activity, Archive, ArrowLeftRight, ArrowRight, ArrowUp, ArrowUpCircle, ArrowUpFromLine,
  ArrowUpRight, BarChart3, Bell, BellOff, Bot, Box, Brain, Bug, Camera, Check, ChevronDown,
  ChevronRight, ChevronsUpDown, Cloud, Code, Cog, Columns3, Compass, Container, Copy, Cpu,
  Database, Download, ExternalLink, Eye, EyeOff, FilePen, FileText, Filter, FolderOpen, Gauge,
  GitBranch, Globe, Hammer, HardDrive, HeartPulse, HelpCircle, Hourglass, Inbox, Key, Layers,
  Link, ListChecks, Loader2, Mail, MailCheck, MailOpen, MailX, Maximize2, Megaphone, Menu,
  Minimize2, Minus, Monitor, Moon, MoreHorizontal, MousePointerClick, Network, Paperclip,
  Pencil, Play, Plus, Puzzle, RefreshCw, Rocket, RotateCcw, Route, Rss, ScrollText, Search,
  Send, Server, Settings, Share2, ShieldCheck, Smartphone, Square, Star, Sun, Tablet, Tag,
  Terminal, ThumbsUp, Timer, Trash2, TriangleAlert, Upload, User, Users, Video, Waypoints, X,
  Zap, type LucideIcon,
} from 'lucide-react'
import tokens from '../../../../web/packages/op/tokens.json'

/* ── local Block / Demo, in the shape OpComponents.tsx uses ─────────────── */

function Block({ id, title, rule, api, children }: { id: string; title: string; rule: ReactNode; api: string; children: ReactNode }) {
  return (
    <section id={id} className="scroll-mt-16 border-t pt-8">
      <div className="grid gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
        <div className="min-w-0">
          <h2 className="op-h2">{title}</h2>
          <div className="op-prose mt-2 space-y-2 text-sm text-muted-foreground">{rule}</div>
          <pre tabIndex={0} className="op-inset mt-4 overflow-auto border p-3 font-mono text-[11px] leading-5 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">{api}</pre>
        </div>
        <div className="min-w-0 space-y-4">{children}</div>
      </div>
    </section>
  )
}

function Demo({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col">
      <p className="op-label mb-2">{label}</p>
      <div className="flex-1">{children}</div>
    </div>
  )
}

/* ── tokens.json → rows ─────────────────────────────────────────────────── */

type Token = { $type?: string; $value: unknown; $description?: string }
type Group = { [k: string]: Token | Group | unknown }

const doc = tokens as unknown as { base: Group; semantic: Record<'light' | 'dark', Group> }

const isToken = (v: unknown): v is Token => !!v && typeof v === 'object' && !Array.isArray(v) && '$value' in (v as object)

/** Every token under a group, as `{ path, type, value, description }`. */
function flatten(node: Group, path: string[] = [], inheritedType?: string): { path: string; type: string; value: string; description: string }[] {
  const type = (node.$type as string | undefined) ?? inheritedType
  const out: { path: string; type: string; value: string; description: string }[] = []
  for (const [k, v] of Object.entries(node)) {
    if (k.startsWith('$')) continue
    if (isToken(v)) {
      out.push({
        path: [...path, k].join('.'),
        type: v.$type ?? type ?? '',
        value: Array.isArray(v.$value) ? `cubic-bezier(${v.$value.join(', ')})` : String(v.$value),
        description: v.$description ?? '',
      })
    } else if (v && typeof v === 'object') {
      out.push(...flatten(v as Group, [...path, k], type))
    }
  }
  return out
}

/** Follow `{a.b.c}` to a primitive, so the table shows what the browser sees. */
function resolve(value: string): string {
  const m = /^\{([^}]+)\}$/.exec(value.trim())
  if (!m) return value
  const target = m[1].split('.').reduce<unknown>((n, k) => (n && typeof n === 'object' ? (n as Record<string, unknown>)[k] : undefined), doc as unknown)
  return isToken(target) ? resolve(String(target.$value)) : value
}

const BASE = flatten(doc.base, ['base'])
const LIGHT = flatten(doc.semantic.light)
const DARK = flatten(doc.semantic.dark)

/** A colour value gets a swatch; everything else shows the literal. */
function Swatch({ value, type }: { value: string; type: string }) {
  if (type !== 'color') return null
  return <span aria-hidden className="inline-block size-4 shrink-0 border align-[-3px]" style={{ background: value }} />
}

function TokenTable({ rows, alias }: { rows: { path: string; type: string; value: string; description: string }[]; alias?: boolean }) {
  return (
    <div data-allow-overflow tabIndex={0} className="op-kv overflow-x-auto focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">
      <table className="w-full min-w-[42rem] border-collapse text-left">
        <thead>
          <tr className="border-b">
            <th scope="col" className="op-label px-3 py-2">token</th>
            <th scope="col" className="op-label px-3 py-2">{alias ? 'alias · resolved' : 'value'}</th>
            <th scope="col" className="op-label px-3 py-2">what it is for</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => {
            const resolved = resolve(r.value)
            return (
              <tr key={r.path} className="align-top">
                <th scope="row" className="whitespace-nowrap px-3 py-1.5 text-left font-mono text-[11px] font-normal">
                  {alias ? `--${r.path}` : r.path}
                </th>
                <td className="whitespace-nowrap px-3 py-1.5 font-mono text-[11px] text-muted-foreground">
                  <span className="inline-flex items-center gap-1.5">
                    <Swatch value={resolved} type={r.type} />
                    {alias && r.value !== resolved ? <span className="text-foreground">{r.value}</span> : null}
                    <span>{resolved}</span>
                  </span>
                </td>
                <td className="px-3 py-1.5 text-[11px] text-muted-foreground">{r.description}</td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

/* ── motion ─────────────────────────────────────────────────────────────── */

/**
 * The tier is chosen with a class, never with a literal or an inline style:
 * `op.css`'s blanket rule carries `!important`, so an inline
 * `transitionDuration` never reaches the element — which is the point. One
 * stylesheet decides how long anything takes.
 */
const TIERS = [
  ['fast', 'op-motion-fast', '80ms', 'hover acknowledgement'],
  ['base', '', '100ms', 'every state change of a control'],
  ['slow', 'op-motion-slow', '200ms', 'something arriving on top of the page'],
] as const

function MotionDemo() {
  const [on, setOn] = useState<string | null>('base')
  return (
    <div className="space-y-3">
      <div className="op-kv">
        {TIERS.map(([name, tier, value, use]) => (
          <div key={name} className="flex flex-wrap items-center gap-3 px-3 py-2">
            <button
              type="button"
              aria-pressed={on === name}
              onClick={() => setOn(on === name ? null : name)}
              className={`op-motion ${tier} inline-flex h-7 min-w-24 items-center justify-center gap-2 border px-3 text-xs data-[hot=true]:bg-foreground data-[hot=true]:text-background`}
              data-hot={on === name}
            >
              {name}
            </button>
            <span className="font-mono text-[11px] text-muted-foreground">{value}</span>
            <span className="text-[11px] text-muted-foreground">{use}</span>
          </div>
        ))}
      </div>
      <p className="op-prose text-[11px] text-muted-foreground">
        The same control, three durations. Only the fill and the ink move — nothing changes size,
        nothing reflows, and the end state is identical in all three. Under{' '}
        <span className="font-mono">prefers-reduced-motion: reduce</span> all three tokens are{' '}
        <span className="font-mono">0s</span> and every button above snaps, which is why they must
        already be readable at the end state alone. See <span className="font-mono">docs/motion.md</span>.
      </p>
    </div>
  )
}

/* ── icon vocabulary ────────────────────────────────────────────────────── */

type Concept = [concept: string, icon: LucideIcon | ComponentType<{ className?: string }>, where: string]

const KINDS: Concept[] = [
  ['project', Box, 'ledgers · palette'],
  ['service / container', Container, 'agent tools'],
  ['deployment', Rocket, 'deploy ledger'],
  ['database', Database, 'database pages'],
  ['storage / volume', HardDrive, 'nodes · database'],
  ['node / compute', Cpu, 'nodes ledger'],
  ['server / instance', Server, 'nodes · observe'],
  ['network / topology', Network, 'settings routing'],
  ['mesh / peers', Waypoints, 'system map'],
  ['domain / region', Globe, 'domains · geography'],
  ['route', Route, 'proxy routes'],
  ['environment / branch', GitBranch, 'env pickers'],
  ['file', FileText, 'agent file rows'],
  ['file being edited', FilePen, 'agent write tool'],
  ['log / terminal', Terminal, 'log panes'],
  ['log stream', ScrollText, 'console nav'],
  ['code / stack frame', Code, 'error detail'],
  ['agent', Bot, 'agent chat'],
  ['reasoning step', Brain, 'agent chat'],
  ['task list', ListChecks, 'agent plan'],
  ['user', User, 'error detail'],
  ['team', Users, 'team settings'],
  ['api key / secret', Key, 'settings keys'],
  ['tag / label', Tag, 'analytics dimensions'],
  ['plugin', Puzzle, 'settings plugins'],
  ['layer / stack', Layers, 'console nav'],
  ['folder', FolderOpen, 'console nav'],
  ['archive', Archive, 'system map'],
  ['cloud', Cloud, 'system map'],
  ['session replay', Video, 'replay'],
  ['screenshot', Camera, 'email preview'],
  ['error / crash', Bug, 'error tracking'],
  ['build', Hammer, 'build settings'],
  ['schedule / cron', Timer, 'scheduled jobs'],
  ['retention window', Hourglass, 'settings'],
  ['health check', HeartPulse, 'system map'],
  ['metric / gauge', Gauge, 'monitoring'],
  ['chart', BarChart3, 'analytics nav'],
  ['activity', Activity, 'console overview'],
  ['performance', Zap, 'web vitals'],
  ['security', ShieldCheck, 'settings · permissions'],
  ['notification', Bell, 'alerts'],
  ['notification muted', BellOff, 'error mute'],
  ['announcement', Megaphone, 'campaigns'],
  ['feed', Rss, 'status page'],
  ['email message', Mail, 'email events'],
  ['email opened', MailOpen, 'email events'],
  ['email delivered', MailCheck, 'email events'],
  ['email bounced', MailX, 'email events'],
  ['inbox / queue', Inbox, 'email · observe'],
  ['click', MousePointerClick, 'email + error events'],
  ['device: desktop', Monitor, 'device breakdown'],
  ['device: tablet', Tablet, 'device breakdown'],
  ['device: phone', Smartphone, 'device breakdown'],
  ['browser', Compass, 'analytics browsers'],
  ['link', Link, 'linked resources'],
  ['settings', Settings, 'settings · PageState'],
  ['console settings', Cog, 'console nav'],
  ['help', HelpCircle, 'agent chat'],
]

const ACTIONS: Concept[] = [
  ['create / add', Plus, 'every "new"'],
  ['edit', Pencil, 'inline edit'],
  ['delete', Trash2, 'inline destructive'],
  ['remove from a list', Minus, 'env var rows'],
  ['copy', Copy, 'CopyButton'],
  ['retry / rerun', RotateCcw, 'retry actions'],
  ['refresh / reload', RefreshCw, 'manual refresh · PageState'],
  ['run / play', Play, 'admin jobs'],
  ['stop', Square, 'agent stop'],
  ['send', Send, 'composer'],
  ['upload / import', Upload, 'env import'],
  ['import from a file', ArrowUpFromLine, 'env import'],
  ['download / export', Download, 'exports'],
  ['share', Share2, 'analytics share'],
  ['search / filter', Search, 'ledger filter · palette'],
  ['narrow a list', Filter, 'system map'],
  ['open externally', ExternalLink, 'anything leaving the console'],
  ['navigate / go', ArrowRight, 'links out of a section'],
  ['open in a new place', ArrowUpRight, 'cross-page links'],
  ['swap / compare', ArrowLeftRight, 'comparison'],
  ['promote / upgrade', ArrowUpCircle, 'plan upgrade'],
  ['submit', ArrowUp, 'chat composer'],
  ['expand', Maximize2, 'full-screen panes'],
  ['collapse', Minimize2, 'full-screen panes'],
  ['approve', Check, 'confirmations'],
  ['dismiss / close', X, 'dialogs · chips'],
  ['more', MoreHorizontal, 'overflow menus'],
  ['disclose', ChevronRight, 'trees'],
  ['open a drop', ChevronDown, 'selects'],
  ['pick from many', ChevronsUpDown, 'Picker'],
  ['columns', Columns3, 'ledger columns'],
  ['menu', Menu, 'mobile nav'],
  ['theme: light', Sun, 'theme toggle'],
  ['theme: dark', Moon, 'theme toggle'],
  ['show a secret', Eye, 'SecretValue'],
  ['hide a secret', EyeOff, 'SecretValue'],
  ['feedback', ThumbsUp, 'agent turn'],
  ['favourite', Star, 'landing'],
  ['warning callout', TriangleAlert, 'Callout'],
  ['attachment', Paperclip, 'agent composer'],
  ['working', Loader2, 'inside a button, never a page'],
]

function IconTable({ title, rows }: { title: string; rows: Concept[] }) {
  return (
    <div className="min-w-0">
      <p className="op-label mb-2">{title} · {rows.length}</p>
      <div data-allow-overflow tabIndex={0} className="op-kv overflow-x-auto focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">
        <table className="w-full min-w-[26rem] border-collapse text-left">
          <thead>
            <tr className="border-b">
              <th scope="col" className="op-label px-3 py-2">icon</th>
              <th scope="col" className="op-label px-3 py-2">concept</th>
              <th scope="col" className="op-label px-3 py-2">where used</th>
            </tr>
          </thead>
          <tbody>
            {rows.map(([concept, Icon, where]) => (
              <tr key={concept}>
                <td className="px-3 py-1.5"><Icon className="size-4 shrink-0 text-muted-foreground" /></td>
                <th scope="row" className="whitespace-nowrap px-3 py-1.5 text-left text-xs font-normal">{concept}</th>
                <td className="px-3 py-1.5 text-[11px] text-muted-foreground">{where}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

/* ── the section ────────────────────────────────────────────────────────── */

/** The token table alone: what `/guide#tokens` mounts under the handoff's §4. */
export function TokensBlock() {
  return (
    <div className="space-y-12">
      <Block
        id="tokens-table"
        title="Tokens"
        api={`// the same file the stylesheet is checked against
import tokens from '@temps-sdk/op/tokens.json'

// enforcement, in \`bun run lint\`:
node web/packages/op/scripts/tokens.mjs check
node web/packages/op/scripts/tokens.mjs build  // prints, does not write`}
        rule={
          <>
            <p>
              Two layers. <span className="font-mono">base</span> is the raw material — the paper/ink
              pair, the five state hues, the faces, the scale. <span className="font-mono">semantic</span>{' '}
              is what a component may name, and it is exactly the custom properties{' '}
              <span className="font-mono">.operator.ink</span> declares, light and dark.
            </p>
            <p>
              Nothing below is typed by hand: the table is built from{' '}
              <span className="font-mono">tokens.json</span>, and{' '}
              <span className="font-mono">tokens.mjs check</span> fails the build if that file and{' '}
              <span className="font-mono">op.css</span> disagree about a single value.
            </p>
            <p>
              Light and dark are asymmetric on purpose: dark redeclares only what changes. Radius,
              faces, the focus ring and motion cascade from light.
            </p>
          </>
        }
      >
        <Demo label={`base · ${BASE.length} tokens`}><TokenTable rows={BASE} /></Demo>
        <Demo label={`semantic · light · ${LIGHT.length} custom properties`}><TokenTable rows={LIGHT} alias /></Demo>
        <Demo label={`semantic · dark · ${DARK.length} custom properties`}><TokenTable rows={DARK} alias /></Demo>
      </Block>

    </div>
  )
}

/** Motion alone: `/guide#motion` and the gallery both mount this. */
export function MotionBlock() {
  return (
    <div className="space-y-12">
      <Block
        id="motion-tiers"
        title="Motion"
        api={`--op-duration-fast   80ms
--op-duration       100ms
--op-duration-slow  200ms
--op-ease           cubic-bezier(0.2, 0, 0, 1)

.op-motion .op-motion-fast .op-motion-slow`}
        rule={
          <>
            <p>
              Motion tells the reader they caused something, or that a value they are watching
              changed. It never introduces, never decorates, never asks to be waited for.
            </p>
            <p>
              May move: a control changing state, a drop opening, a row entering focus, a live value
              updating. Never moves: layout, page transitions, charts drawing themselves, skeletons
              shimmering, anything decorative.
            </p>
            <p>
              Two exceptions exist today and are written down in{' '}
              <span className="font-mono">docs/motion.md</span>: the permanent hard 3px{' '}
              <span className="font-mono">.op-raise</span> offset, which never lifts, and the two
              animations the blanket rule excludes by selector —{' '}
              <span className="font-mono">animate-pulse</span> (skeletons) and{' '}
              <span className="font-mono">animate-spin</span> (the retry button).
            </p>
          </>
        }
      >
        <Demo label="three durations, one control"><MotionDemo /></Demo>
      </Block>

    </div>
  )
}

/** The icon vocabulary alone: `/guide#icons` and the gallery both mount this. */
export function IconsBlock() {
  return (
    <div className="space-y-12">
      <Block
        id="icons-vocabulary"
        title="Icons"
        api={`lucide-react only · stroke 1.75 (set on .operator.ink svg.lucide)
16px  size-4      kind slot before a name
14px  size-3.5    inside a label, button or badge

<LedgerRow icon={Rocket} … />`}
        rule={
          <>
            <p>
              One family, monochrome ink, two sizes. An icon says what a thing <em>is</em> or what a
              control <em>does</em>; a glyph says what state it is in. They never share a slot.
            </p>
            <p>
              The table is the whole allowed vocabulary: one concept, one icon. If a concept is not
              here it does not have an icon yet — adding one is a PR with one table row and a call
              site. Two icons for one concept is the failure this table exists to prevent.
            </p>
            <p>
              Banned: brand-coloured logos (except <span className="font-mono">GitProviderLogo</span>{' '}
              and <span className="font-mono">ProjectMark</span>, which are identity marks), filled
              icons, emoji, a coloured icon, and icons used as bullets. Full list in{' '}
              <span className="font-mono">docs/icons.md</span>.
            </p>
          </>
        }
      >
        <div className="grid gap-6 lg:grid-cols-2">
          <IconTable title="kinds · what a thing is" rows={KINDS} />
          <IconTable title="actions · what a control does" rows={ACTIONS} />
        </div>
      </Block>
    </div>
  )
}

/** All three, in order: what `/op-components` mounts. */
export function TokenBlocks() {
  return (
    <div className="space-y-12">
      <TokensBlock />
      <MotionBlock />
      <IconsBlock />
    </div>
  )
}
