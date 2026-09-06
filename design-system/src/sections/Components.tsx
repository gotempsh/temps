// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { ArrowUpFromLine, Box, Cpu, Database, ExternalLink, FileText, HardDrive, Loader2, MoreHorizontal, RotateCcw, Rocket, Rows3, Trash2, Waypoints } from 'lucide-react'
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from '@/components/ui/breadcrumb'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandShortcut,
} from '@/components/ui/command'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip'
import {
  EchoDialog,
  Field,
  Kbd,
  MOD,
  Num,
  PageState,
  PageTitle,
  Picker,
  Segmented,
  Status,
  type State,
} from '@/components/op'
import { Block, Demo, DocPage, Rule } from '@/components/op-doc'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   /components — the primitives (shadcn + plain elements) under the v1 ink
   skin. One block per primitive: the rule, the sizes and variants v1
   actually uses, and the states.

   The COMPOSED operator components (Status, Num/Metric, PageState, Kbd,
   EchoDialog, TimeChart, Ledger, Detail, Picker, Settings) are documented
   on /op-components. This page links there rather than repeating them.
   ──────────────────────────────────────────────────────────────────────── */

/** Portal content (dialog, popover, dropdown, select, palette) renders outside
 *  the `.operator` root, so it must carry the skin class or it renders
 *  unskinned. Same value EchoDialog defaults to. */
const SKIN = 'operator ink v1'

const TOC = [
  ['button', 'Button'],
  ['input', 'Input · Textarea'],
  ['picker', 'Picker vs Select'],
  ['toggle', 'Checkbox · Switch'],
  ['tabs', 'Tabs · Segmented'],
  ['rows', 'Rows and tables'],
  ['palette', 'Command palette'],
  ['overlay', 'Popover · Menu · Tooltip'],
  ['dialog', 'Dialog'],
  ['toast', 'Toast · notifications'],
  ['skeleton', 'Skeleton · pending'],
  ['kbd', 'Kbd'],
  ['breadcrumb', 'Breadcrumb · PageTitle'],
  ['identity', 'Identity'],
  ['banned', 'Banned'],
] as const

/* Brand §6 "an icon wherever it adds context": the palette is the one list that
   mixes every kind the console has, so every row leads with a fixed 16px slot in
   muted ink. Projects carry their kind (app · worker · static), pages carry the
   same icon the sidebar gives them, commands carry the icon of what they do. The
   state glyph keeps its own slot: kind and state never share one. */
const KIND = 'size-4 shrink-0 text-muted-foreground'
const PROJECT_KIND = { app: Box, worker: Cpu, static: FileText } as const
const PALETTE_PROJECTS = [
  ['billing-worker', 'error', 'worker'],
  ['api-gateway', 'warn', 'app'],
  ['docs', 'ok', 'static'],
] as const satisfies readonly (readonly [string, State, keyof typeof PROJECT_KIND])[]
const PALETTE_PAGES = [
  ['databases', Database, 'storage'],
  ['traces', Waypoints, 'observe'],
  ['backups', HardDrive, 'storage'],
] as const

/* The command palette rows and the picker rows share the ink treatment:
   selected is an ink fill, never a tint. */
const CMDK_HEADING =
  '[&_[cmdk-group-heading]]:py-1 [&_[cmdk-group-heading]]:text-[10px] [&_[cmdk-group-heading]]:uppercase [&_[cmdk-group-heading]]:tracking-[0.1em]'
const CMDK_ITEM = 'rounded-none data-[selected=true]:bg-foreground data-[selected=true]:text-background'

const BRANCHES = [
  { value: 'main', group: 'default', meta: 'e4d1f0a · 41m ago', keywords: 'master trunk' },
  { value: 'staging', group: 'recent', meta: '9bc61c0 · 2h ago' },
  { value: 'feat/checkout-address', group: 'recent', meta: 'b7c9d21 · 6h ago' },
  { value: 'fix/retry-stripe-webhooks', group: 'recent', meta: '7a11c3e · yesterday' },
  { value: 'feat/edge-cache', group: 'recent', meta: 'c0ffee1 · 2d ago' },
  { value: 'release/1.4', group: 'all', meta: '3 weeks ago' },
  { value: 'release/1.3', group: 'all', meta: '2 months ago' },
  { value: 'chore/deps-2026-08', group: 'all', meta: '2 months ago' },
  { value: 'spike/otel-metrics', group: 'all', meta: '4 months ago' },
]

/** Static toast body, same shape as the console's `notify(state, title, detail)`. */
function Toast({ level, title, detail, ts }: { level: 'ok' | 'warn' | 'err'; title: string; detail?: string; ts: string }) {
  return (
    <div className="w-full max-w-[360px] border bg-background px-3 py-2">
      <div className="flex items-start gap-2 font-mono text-xs">
        <span
          className={cn(
            'w-8 shrink-0',
            level === 'ok' && 'text-success',
            level === 'warn' && 'text-warning',
            level === 'err' && 'text-destructive',
          )}
        >
          {level}
        </span>
        <span className="min-w-0 flex-1">
          <span className="block">{title}</span>
          {detail && <span className="block truncate text-muted-foreground">{detail}</span>}
        </span>
        <span className="shrink-0 tabular-nums text-muted-foreground">{ts}</span>
      </div>
    </div>
  )
}

export function ComponentsPage() {
  const [pending, setPending] = useState(false)
  const [domain, setDomain] = useState('api gateway.acme.sh')
  const [note, setNote] = useState('Rolled back because /healthz returned 503 for 4 minutes after dep_91a.')
  const [branch, setBranch] = useState<string | null>('main')
  const [density, setDensity] = useState('comfortable')
  const [autoDeploy, setAutoDeploy] = useState(true)
  const [selected, setSelected] = useState<string[]>(['staging'])
  const [tab, setTab] = useState<'overview' | 'deploys' | 'logs'>('overview')
  const [range, setRange] = useState<'24h' | '7d' | '30d'>('24h')
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [connectOpen, setConnectOpen] = useState(false)
  const [deleted, setDeleted] = useState(false)

  const domainInvalid = /\s/.test(domain) || !domain.includes('.')
  const toggle = (id: string) =>
    setSelected((s) => (s.includes(id) ? s.filter((x) => x !== id) : [...s, id]))

  return (
    <DocPage
      eyebrow="components · primitives under ink"
      intro={
        <>
          The shadcn primitives and plain elements as v1 uses them: the sizes, the variants, the states. The composed
          operator components live on{' '}
          <Link to="/op-components" className="underline underline-offset-4">
            /op-components
          </Link>
          , assembled into a console on{' '}
          <Link to="/v1" className="underline underline-offset-4">
            /v1
          </Link>
          . Rules that are not obeyed here are not rules, so every block states the one it enforces. Anything that
          renders in a portal — dialog, popover, dropdown, select, palette — carries{' '}
          <span className="font-mono">operator ink v1</span> on its content element or it renders unskinned.
        </>
      }
      toc={TOC}
    >
      {/* ── Button ──────────────────────────────────────────────────── */}
      <Block
        id="button"
        title="Button"
        api={`<Button size="sm" className="op-primary h-8 text-xs">
  deploy <Kbd keys={['⌘', '⏎']} />
</Button>
<Button variant="outline" size="sm" className="h-7 text-xs">…</Button>  header
<Button variant="outline" size="sm" className="h-8 text-xs">…</Button>  page`}
        rule={
          <>
            <p>
              At most one <code>op-primary</code> per viewport. It is the ink-filled button with the 2px hard shadow
              that translates on press, and it marks the one thing the screen exists to do — deploy, save, promote.
              Everything else is an outline button.
            </p>
            <p>
              Two heights: <code>h-7</code> in the 44px header strip, <code>h-8</code> everywhere on the page. Labels
              are lowercase and name the outcome. Key badges sit inside the button, never replace it.
            </p>
            <p>
              A destructive outline (<code>border-destructive text-destructive</code>) is only ever an EchoDialog
              trigger. There is no filled red button in the console: red is a status colour, and a button that looks
              like a status reads as a fact rather than an action.
            </p>
          </>
        }
      >
        <Demo label="the one primary · h-8 page action">
          <div className="flex flex-wrap items-center gap-2">
            <Button size="sm" className="op-primary h-8 text-xs">
              deploy api-gateway <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" />
            </Button>
            <Button variant="outline" size="sm" className="h-8 text-xs">
              view build log
            </Button>
            <Button variant="outline" size="sm" className="h-8 text-xs">
              rollback…
            </Button>
          </div>
        </Demo>

        <Demo label="h-7 · header controls">
          <div className="flex flex-wrap items-center gap-2 border p-2">
            <Button variant="outline" size="sm" className="h-7 text-xs">
              find <Kbd keys={['⌘', 'K']} className="ml-1" />
            </Button>
            <Button variant="outline" size="icon" className="h-7 w-7" aria-label="Toggle density">
              <span aria-hidden className="font-mono text-[11px]">
                d
              </span>
            </Button>
            <Button variant="outline" size="icon" className="h-7 w-7 bg-foreground text-background" aria-pressed aria-label="Density is dense">
              <span aria-hidden className="font-mono text-[11px]">
                d
              </span>
            </Button>
          </div>
        </Demo>

        <Demo label="states · default, focus-visible (tab to it), disabled, pressed">
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="outline" size="sm" className="h-8 text-xs">
              default
            </Button>
            <Button
              variant="outline"
              size="sm"
              className="h-8 text-xs outline outline-2 outline-offset-2 outline-ring"
            >
              focus-visible
            </Button>
            <Button variant="outline" size="sm" className="h-8 text-xs" disabled>
              disabled · no changes
            </Button>
            <Button size="sm" className="op-primary op-pressed h-8 text-xs">
              save <Kbd keys={['⌘', 'S']} className="ml-1 opacity-70" />
            </Button>
          </div>
          <p className="mt-2 text-[11px] text-muted-foreground">
            <span className="font-mono">op-pressed</span> is the momentary look a keyboard shortcut leaves when it
            clicks the real button, so <Kbd keys={['⌘', 'S']} /> and a pointer press are honest about the same
            disabled state.
          </p>
        </Demo>

        <Demo label="destructive outline · only as an EchoDialog trigger">
          <EchoDialog
            trigger={
              <Button variant="outline" size="sm" className="h-8 border-destructive text-xs text-destructive">
                <Trash2 /> delete project…
              </Button>
            }
            echo="$ temps project delete billing-worker --yes"
            title="Delete billing-worker"
            description="Removes the project, its 3 environments, its routes and its certificate. Backups in S3 are kept."
            confirmWord="billing-worker"
            steps={['stop containers', 'remove proxy routes', 'revoke certificate', 'archive rows']}
            onDone={() => setDeleted(true)}
            destructive
          />
          {deleted && (
            <p className="mt-2 text-[11px] text-muted-foreground">billing-worker deleted (demo state).</p>
          )}
        </Demo>

        <Rule state="error">Two filled buttons on one screen: the reader has to decide which is the action.</Rule>
        <Rule state="ok">
          One <span className="font-mono">op-primary</span>, outline for the rest, destructive outline behind a typed
          confirmation.
        </Rule>
      </Block>

      {/* ── Input / Textarea ────────────────────────────────────────── */}
      <Block
        id="input"
        title="Input · Textarea"
        api={`<Field label="branch" help="the branch deploys build from">
  <Input className="h-8 font-mono text-xs" />
</Field>

<Input aria-invalid className="h-8 border-destructive font-mono text-xs" />`}
        rule={
          <>
            <p>
              One height: <code>h-8</code>, <code>text-xs</code>. The face is the tell — mono when the content is a
              value the operator will compare or paste (branch, domain, path, image tag, id), sans when it is prose
              they wrote (a rollback note, an alert description).
            </p>
            <p>
              <code>Field</code> lays out label · control · help and folds to one row through a container query, so it
              stacks when the section is narrow regardless of viewport width.
            </p>
            <p>
              An invalid field sets <code>aria-invalid</code>, takes a destructive border, and its help line names the
              fix. "Invalid input" is not a fix. There is no support channel: the message is the whole help the reader
              gets.
            </p>
          </>
        }
      >
        <Demo label="mono · a value">
          <div className="@container max-w-xl space-y-4 border p-4">
            <Field label="branch" help="deploys build from this branch on push">
              <Input defaultValue="main" className="h-8 font-mono text-xs" />
            </Field>
            <Field label="health path" help="polled every 10s after a container starts">
              <Input defaultValue="/healthz" className="h-8 font-mono text-xs" />
            </Field>
            <Field label="domain" help="a hostname you control, without a scheme or a path">
              <Input
                value={domain}
                onChange={(e) => setDomain(e.target.value)}
                aria-invalid={domainInvalid || undefined}
                className={cn('h-8 font-mono text-xs', domainInvalid && 'border-destructive')}
              />
            </Field>
            {domainInvalid && (
              <p className="flex items-start gap-2 text-[11px] text-destructive">
                <span aria-hidden className="w-3 shrink-0 text-center">
                  ×
                </span>
                <span className="min-w-0">
                  "{domain}" is not a hostname. Remove the space and include a dot — for example{' '}
                  <span className="font-mono">api.acme.sh</span>.
                </span>
              </p>
            )}
          </div>
        </Demo>

        <Demo label="sans · prose the operator writes">
          <div className="@container max-w-xl border p-4">
            <Field label="rollback note" help="shown on the deploy row and in the incident thread">
              <textarea
                value={note}
                onChange={(e) => setNote(e.target.value)}
                rows={3}
                className="op-prose flex w-full border border-input bg-background px-2 py-1.5 text-xs placeholder:text-muted-foreground focus-visible:border-ring focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-ring disabled:cursor-not-allowed disabled:opacity-50"
              />
            </Field>
          </div>
        </Demo>

        <Demo label="states · default, focus-visible, invalid, disabled, read-only value">
          <div className="grid max-w-xl gap-2">
            <Input defaultValue="staging" className="h-8 font-mono text-xs" />
            <Input
              defaultValue="staging"
              className="h-8 border-ring font-mono text-xs outline outline-2 -outline-offset-1 outline-ring"
            />
            <Input defaultValue="acme .sh" aria-invalid className="h-8 border-destructive font-mono text-xs" />
            <Input defaultValue="dep_91a" disabled className="h-8 font-mono text-xs" />
            <p className="op-inset border px-2 py-1.5 font-mono text-xs">
              dep_91a <span className="text-muted-foreground">· deployed 41m ago · e4d1f0a</span>
            </p>
          </div>
          <p className="mt-2 text-[11px] text-muted-foreground">
            A value the operator cannot change is not a disabled input — it is text in an inset pane. A disabled input
            invites a click that does nothing.
          </p>
        </Demo>
      </Block>

      {/* ── Picker vs Select ────────────────────────────────────────── */}
      <Block
        id="picker"
        title="Picker vs Select"
        api={`<Picker value={branch} onChange={setBranch} options={BRANCHES}
        allowCustom="use branch" />          // recognise, not recall

<Select>                                     // ≤7 fixed options
  <SelectTrigger className="h-8 rounded-none font-mono text-xs">…
  <SelectContent className="operator ink v1">…`}
        rule={
          <>
            <p>
              A plain <code>Select</code> is allowed only for a short, fixed list the operator already knows by heart:
              density, plan, sort order, log level. Seven options is the ceiling.
            </p>
            <p>
              Everything the operator has to <em>recognise</em> rather than recall — branches, images, regions,
              environments, providers, domains — is a{' '}
              <Link to="/op-components#picker" className="underline underline-offset-4">
                Picker
              </Link>
              : a filter box, grouped rows, a muted <code>meta</code> on the right, and loading/error as states inside
              the list instead of a spinner on the trigger.
            </p>
            <p>
              Both trigger the same shape: <code>h-8</code>, ink border, radius 0 (v1 flattens{' '}
              <code>select</code>), mono when the value is a value. Both need the skin class on their portal content.
            </p>
          </>
        }
      >
        <Demo label="Picker · 9 branches, grouped, with last commit and age">
          <div className="max-w-sm">
            <Picker value={branch} onChange={setBranch} options={BRANCHES} allowCustom="use branch" skin={SKIN} />
            <p className="mt-2 text-[11px] text-muted-foreground">
              Selected: <span className="font-mono">{branch ?? '–'}</span>. Type a branch that does not exist yet and
              it offers "use branch".
            </p>
          </div>
        </Demo>

        <Demo label="Select · 2 fixed options the operator knows (density)">
          <div className="max-w-sm">
            <Select value={density} onValueChange={setDensity}>
              <SelectTrigger className="h-8 rounded-none font-mono text-xs">
                <SelectValue />
              </SelectTrigger>
              <SelectContent className={cn(SKIN, 'rounded-none border shadow-none')}>
                <SelectItem value="comfortable" className="rounded-none font-mono text-xs">
                  comfortable
                </SelectItem>
                <SelectItem value="dense" className="rounded-none font-mono text-xs">
                  dense
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
        </Demo>

        <Demo label="native select · a short, fixed list of known values, styled by the skin">
          <label className="flex items-center gap-2 text-[11px] text-muted-foreground">
            plan
            <select
              defaultValue="team"
              aria-label="Plan"
              className="h-7 border bg-background px-1 font-mono text-[11px] text-foreground"
            >
              <option value="selfhost">self-hosted</option>
              <option value="starter">Starter</option>
              <option value="team">Team</option>
              <option value="business">Business</option>
            </select>
          </label>
        </Demo>

        <Rule state="error">
          A <span className="font-mono">&lt;select&gt;</span> of 40 branches. The operator cannot filter, cannot see
          which one is ahead of production, and cannot type one that does not exist yet.
        </Rule>
        <Rule state="ok">Picker for branches, images, regions, environments, providers. Select for density and plan.</Rule>
      </Block>

      {/* ── Checkbox / Switch ───────────────────────────────────────── */}
      <Block
        id="toggle"
        title="Checkbox · Switch"
        api={`<Checkbox className="h-3.5 w-3.5 rounded-none" />   part of a saved form
<Switch className="h-4 w-7 rounded-none [&>span]:h-3 [&>span]:w-3
                   [&>span]:rounded-none
                   data-[state=checked]:[&>span]:translate-x-3" />`}
        rule={
          <>
            <p>
              Both are ink, square, radius 0. Neither carries colour: a checked box is an ink fill, not a green one.
            </p>
            <p>
              <strong>Switch = it takes effect the moment you flip it</strong> (auto-deploy on push, alerting on a
              metric). There is no save button to press, so the flip must be followed by a toast that says what
              happened.
            </p>
            <p>
              <strong>Checkbox = part of a form that is saved</strong> (a settings section behind{' '}
              <Kbd keys={['⌘', 'S']} />) or a row selection that feeds a bulk bar. Nothing happens until save.
            </p>
          </>
        }
      >
        <Demo label="switch · immediate">
          <div className="@container max-w-xl space-y-3 border p-4">
            <Field label="auto-deploy" help="every push to main builds and deploys; takes effect immediately">
              <span className="flex items-center gap-2">
                <Switch
                  checked={autoDeploy}
                  onCheckedChange={setAutoDeploy}
                  className="h-4 w-7 rounded-none border data-[state=unchecked]:bg-background [&>span]:h-3 [&>span]:w-3 [&>span]:rounded-none [&>span]:bg-foreground [&>span]:shadow-none data-[state=checked]:[&>span]:translate-x-3 data-[state=checked]:[&>span]:bg-background"
                />
                <span className="font-mono text-xs">{autoDeploy ? 'on' : 'off'}</span>
              </span>
            </Field>
            <Field label="alerting" help="disabled while the project is paused">
              <span className="flex items-center gap-2">
                <Switch
                  disabled
                  className="h-4 w-7 rounded-none border data-[state=unchecked]:bg-background [&>span]:h-3 [&>span]:w-3 [&>span]:rounded-none [&>span]:bg-foreground [&>span]:shadow-none"
                />
                <span className="font-mono text-xs text-muted-foreground">off · project paused</span>
              </span>
            </Field>
          </div>
        </Demo>

        <Demo label="checkbox · selection that feeds a bulk bar">
          <div className="border">
            <div className="op-rows text-xs">
              {[
                ['production', 'dep_91a · main'],
                ['staging', 'dep_92c · staging'],
                ['preview-212', 'dep_92d · feat/checkout-address'],
              ].map(([env, meta]) => (
                <label key={env} className="op-row flex cursor-default items-center gap-3">
                  <Checkbox
                    checked={selected.includes(env)}
                    onCheckedChange={() => toggle(env)}
                    className="h-3.5 w-3.5 rounded-none border-foreground data-[state=checked]:bg-foreground data-[state=checked]:text-background"
                  />
                  <span className="min-w-0 flex-1 truncate font-mono">{env}</span>
                  <span className="shrink-0 font-mono text-[11px] text-muted-foreground">{meta}</span>
                </label>
              ))}
            </div>
            <div className="flex flex-wrap items-center gap-2 border-t px-3 py-2 text-[11px]">
              <span className="text-muted-foreground">
                <Num value={selected.length} /> selected · <Kbd keys="x" className="mx-1" /> toggle
                <Kbd keys="⇧A" className="mx-1" /> all
              </span>
              <div className="flex w-full flex-wrap gap-2 sm:ml-auto sm:w-auto">
                <Button variant="outline" size="sm" className="h-7 text-xs" disabled={selected.length === 0}>
                  add to production
                </Button>
                <Button variant="outline" size="sm" className="h-7 text-xs" disabled={selected.length === 0}>
                  remove
                </Button>
              </div>
            </div>
          </div>
        </Demo>

        <Demo label="states · unchecked, checked, disabled">
          <div className="flex flex-wrap items-center gap-6 text-xs">
            <label className="flex items-center gap-2">
              <Checkbox className="h-3.5 w-3.5 rounded-none border-foreground data-[state=checked]:bg-foreground data-[state=checked]:text-background" />
              unchecked
            </label>
            <label className="flex items-center gap-2">
              <Checkbox
                defaultChecked
                className="h-3.5 w-3.5 rounded-none border-foreground data-[state=checked]:bg-foreground data-[state=checked]:text-background"
              />
              checked
            </label>
            <label className="flex items-center gap-2 opacity-50">
              <Checkbox disabled className="h-3.5 w-3.5 rounded-none border-foreground" />
              disabled
            </label>
            <label className="flex items-center gap-2">
              <Checkbox className="h-3.5 w-3.5 rounded-none border-foreground outline outline-2 outline-offset-2 outline-ring" />
              focus-visible
            </label>
          </div>
        </Demo>
      </Block>

      {/* ── Tabs / Segmented ────────────────────────────────────────── */}
      <Block
        id="tabs"
        title="Tabs and Segmented"
        api={`// Detail tablist — the screen's sections, keys 1..n
<button role="tab" className={tab === t
  ? 'bg-foreground text-background' : 'hover:bg-muted'}>…

<Segmented options={[['24h','24h'],['7d','7d']]} value onChange />`}
        rule={
          <>
            <p>
              The <strong>Detail tablist</strong> switches what the screen is about — overview, deploys, environments,
              variables, logs, settings. Active is an ink fill (<code>bg-foreground text-background</code>), each tab
              carries its number key badge, and the strip scrolls horizontally on a phone rather than wrapping.
            </p>
            <p>
              <strong>Segmented</strong> changes a parameter of what is already on screen — range, compare, aggregate.
              Active is <code>bg-muted</code>, deliberately quieter, because it is not navigation and it must not
              compete with the tabs above it.
            </p>
            <p>
              There is no pill tab and no underline tab. Both read as decoration at this density and neither survives
              the ink border.
            </p>
          </>
        }
      >
        <Demo label="Detail tablist · navigation, ink fill active, number keys">
          <div role="tablist" className="op-scroll-x flex max-w-full border text-xs">
            {(['overview', 'deploys', 'logs'] as const).map((t, i) => (
              <button
                key={t}
                role="tab"
                aria-selected={tab === t}
                onClick={() => setTab(t)}
                className={cn(
                  'inline-flex h-8 shrink-0 items-center gap-1 whitespace-nowrap px-3 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring',
                  i > 0 && 'border-l',
                  tab === t ? 'bg-foreground text-background' : 'hover:bg-muted',
                )}
              >
                {t} <Kbd keys={String(i + 1)} className="ml-1 hidden opacity-60 sm:inline-flex" />
              </button>
            ))}
          </div>
        </Demo>

        <Demo label="Segmented · a parameter of the current view, muted active">
          <div className="flex flex-wrap items-center gap-3">
            <Segmented
              options={
                [
                  ['24h', '24h'],
                  ['7d', '7d'],
                  ['30d', '30d'],
                ] as const
              }
              value={range}
              onChange={setRange}
            />
            <span className="text-[11px] text-muted-foreground">
              requests / 30 min · window <span className="font-mono">{range}</span>
            </span>
          </div>
        </Demo>

        <Rule state="error">Pill tabs with a rounded active chip. A second radius and a second surface treatment.</Rule>
        <Rule state="ok">Ink fill for navigation, muted fill for a parameter, one border around both.</Rule>
      </Block>

      {/* ── Rows and tables ─────────────────────────────────────────── */}
      <Block
        id="rows"
        title="Rows and tables"
        api={`<div className="op-rows border">
  <div className="op-row op-cols hidden md:grid" style={{'--cols': grid}}>
    <span className="op-label">project</span> …
  </div>
  <div className="op-row op-cols grid grid-cols-[1fr_auto] md:grid"
       style={{'--cols': grid}}>…`}
        rule={
          <>
            <p>
              There is no <code>&lt;Table&gt;</code> in the system. A list is <code>.op-rows</code> (children separated
              by a soft 16% rule) wrapped in one ink border, and each row is <code>.op-row</code> — fixed height{' '}
              <code>--row-h</code> on desktop, growing with content on a phone.
            </p>
            <p>
              Columns come from <code>--cols</code> on <code>.op-cols</code>, applied from md up only. Below md every
              secondary cell is hidden and the row folds to <code>[1fr_auto]</code>: identity plus the state glyph,
              with what matters folded into a second line. The mobile rendering must carry the row's primary action —
              a phone user cannot reach a desktop-only cell.
            </p>
            <p>
              Header labels are <code>.op-label</code>. No zebra striping, no hover shadow, no border-radius on rows.
              Hover is <code>bg-muted</code> and a 2px ink bar on the left of the cursor row, which is the same
              affordance <Kbd keys="j" /> <Kbd keys="k" /> use.
            </p>
          </>
        }
      >
        <Demo label="ledger rows · resize below 768px to see the fold">
          <div className="op-rows border">
            <div
              className="op-row op-cols hidden items-center md:grid"
              style={{ '--cols': '1.4fr 1fr 110px 90px' } as React.CSSProperties}
            >
              {['project', 'state', 'requests 24h', 'error rate'].map((h) => (
                <span key={h} className="op-label min-w-0 truncate">
                  {h}
                </span>
              ))}
            </div>
            {[
              { id: 'billing-worker', state: 'error' as const, note: 'failing health checks', req: null, err: null },
              { id: 'api-gateway', state: 'warn' as const, note: 'error rate above 0.5%', req: 30800, err: '0.61' },
              { id: 'docs', state: 'ok' as const, note: 'production · dep_88f', req: 2210, err: '0.00' },
              { id: 'acme-web', state: 'idle' as const, note: 'not deployed', req: null, err: null },
            ].map((r, i) => (
              <div
                key={r.id}
                className={cn(
                  'op-row op-cols relative grid w-full grid-cols-[1fr_auto] items-center gap-x-3 text-left text-xs md:grid',
                  i === 1 && 'bg-muted',
                )}
                style={{ '--cols': '1.4fr 1fr 110px 90px' } as React.CSSProperties}
              >
                {i === 1 && <span aria-hidden className="absolute left-0 top-0 h-full w-0.5 bg-foreground" />}
                <span className="min-w-0 md:hidden">
                  <span className="block truncate font-medium">{r.id}</span>
                  <span className="block truncate text-[11px] text-muted-foreground">{r.note}</span>
                </span>
                <span className="md:hidden">
                  <Status state={r.state} label="" />
                </span>
                <span className="hidden min-w-0 truncate font-medium md:block">{r.id}</span>
                <span className="hidden min-w-0 truncate md:block">
                  <Status state={r.state} label={r.note} />
                </span>
                <span className="hidden min-w-0 truncate md:block">
                  <Num value={r.req} />
                </span>
                <span className="hidden min-w-0 truncate md:block">
                  <Num value={r.err} unit="%" className={r.err === '0.61' ? 'text-destructive' : undefined} />
                </span>
              </div>
            ))}
            <div className="op-row flex flex-wrap items-center gap-y-1 text-[11px] text-muted-foreground">
              4 of 6 · <Kbd keys="j" className="mx-1" />
              <Kbd keys="k" className="mr-1" /> move · <Kbd keys="⏎" className="mx-1" /> open ·{' '}
              <Kbd keys="/" className="mx-1" /> filter
            </div>
          </div>
          <p className="mt-2 text-[11px] text-muted-foreground">
            The full template, with filter, sort-by-attention and PageState swapping the rows, is{' '}
            <Link to="/op-components#ledger" className="underline underline-offset-4">
              Ledger
            </Link>
            .
          </p>
        </Demo>

        <Rule state="error">
          <span className="font-mono">&lt;Table&gt;</span> with zebra rows and a hover shadow: three surface
          treatments to say "this is a list".
        </Rule>
        <Rule state="ok">
          <span className="font-mono">.op-rows</span> / <span className="font-mono">.op-row</span> with{' '}
          <span className="font-mono">--cols</span>, one ink frame, soft rules inside.
        </Rule>
      </Block>

      {/* ── Command palette ─────────────────────────────────────────── */}
      <Block
        id="palette"
        title="Command palette"
        api={`<CommandDialog contentClassName={cn(SKIN, 'border shadow-none sm:rounded')}>
  <CommandInput prompt=">" className="font-mono text-xs" />
  <CommandItem className="rounded-none
    data-[selected=true]:bg-foreground
    data-[selected=true]:text-background" />`}
        rule={
          <>
            <p>
              <Kbd keys={['⌘', 'K']} /> everywhere, and always with a visible <em>find</em> button in the header beside
              it — the shortcut is the accelerator, never the only entry point.
            </p>
            <p>
              The magnifier is replaced by a <code>&gt;</code> prompt, the whole dialog is mono, group headings are{' '}
              <code>.op-label</code>-style uppercase, and the selected row is an ink fill, matching the Picker. No
              shadow: the ink border is the elevation.
            </p>
            <p>
              Rows carry the same status glyphs as the page they jump to, so a failing project looks failing in the
              palette too.
            </p>
            <p>
              Every row also leads with its <em>kind</em> in a fixed 16px slot of muted ink: a project&rsquo;s shape
              (app, worker, static), the icon the sidebar gives a page, the icon of what a command does. The palette
              is the one list that mixes every kind the console has, so bare words leave the reader nothing but the
              word to tell a page from a project. Kind and state never share the slot — the glyph keeps its own, and
              an icon is never tinted.
            </p>
          </>
        }
      >
        <Demo label="live · opens as a portal with the skin class">
          <Button variant="outline" size="sm" className="h-8 text-xs" onClick={() => setPaletteOpen(true)}>
            open palette <Kbd keys={['⌘', 'K']} className="ml-1" />
          </Button>
          <CommandDialog
            open={paletteOpen}
            onOpenChange={setPaletteOpen}
            contentClassName={cn(SKIN, 'border shadow-none sm:rounded')}
          >
            <CommandInput prompt=">" placeholder="jump to a project, or run a command…" className="font-mono text-xs" />
            <CommandList className="font-mono text-xs">
              <CommandEmpty>no matches</CommandEmpty>
              <CommandGroup heading="projects" className={CMDK_HEADING}>
                {PALETTE_PROJECTS.map(([name, state, kind]) => {
                  const Kind = PROJECT_KIND[kind]
                  return (
                    <CommandItem key={name} className={CMDK_ITEM} onSelect={() => setPaletteOpen(false)}>
                      <Status state={state} label="" />
                      <Kind aria-hidden className={KIND} />
                      <span>{name}</span>
                      <CommandShortcut className="text-inherit opacity-60">{kind} · production</CommandShortcut>
                    </CommandItem>
                  )
                })}
              </CommandGroup>
              <CommandGroup heading="pages" className={CMDK_HEADING}>
                {PALETTE_PAGES.map(([name, Icon, group]) => (
                  <CommandItem key={name} className={CMDK_ITEM} onSelect={() => setPaletteOpen(false)}>
                    <Icon aria-hidden className={KIND} />
                    <span>{name}</span>
                    <CommandShortcut className="text-inherit opacity-60">{group}</CommandShortcut>
                  </CommandItem>
                ))}
              </CommandGroup>
              <CommandGroup heading="commands" className={CMDK_HEADING}>
                <CommandItem className={CMDK_ITEM} onSelect={() => setPaletteOpen(false)}>
                  <Rocket aria-hidden className={KIND} />
                  <span>deploy api-gateway</span>
                  <CommandShortcut className="text-inherit opacity-60">{MOD} ⏎</CommandShortcut>
                </CommandItem>
                <CommandItem className={CMDK_ITEM} onSelect={() => setPaletteOpen(false)}>
                  <Rows3 aria-hidden className={KIND} />
                  <span>toggle density</span>
                  <CommandShortcut className="text-inherit opacity-60">d</CommandShortcut>
                </CommandItem>
              </CommandGroup>
            </CommandList>
          </CommandDialog>
        </Demo>

        <Demo label="inline · the same list, so the selected fill is visible on the page">
          <Command className={cn('rounded-none border bg-popover font-mono text-xs')}>
            <CommandInput prompt=">" placeholder="jump to a project, or run a command…" className="font-mono text-xs" />
            <CommandList className="max-h-56 font-mono text-xs">
              <CommandEmpty>no matches</CommandEmpty>
              <CommandGroup heading="projects" className={CMDK_HEADING}>
                <CommandItem className={CMDK_ITEM}>
                  <Status state="error" label="" />
                  <Cpu aria-hidden className={KIND} />
                  <span>billing-worker</span>
                  <CommandShortcut className="text-inherit opacity-60">worker · production</CommandShortcut>
                </CommandItem>
                <CommandItem className={CMDK_ITEM}>
                  <Status state="ok" label="" />
                  <FileText aria-hidden className={KIND} />
                  <span>docs</span>
                  <CommandShortcut className="text-inherit opacity-60">static · production</CommandShortcut>
                </CommandItem>
              </CommandGroup>
              <CommandGroup heading="pages" className={CMDK_HEADING}>
                <CommandItem className={CMDK_ITEM}>
                  <Database aria-hidden className={KIND} />
                  <span>databases</span>
                  <CommandShortcut className="text-inherit opacity-60">storage</CommandShortcut>
                </CommandItem>
                <CommandItem className={CMDK_ITEM}>
                  <Waypoints aria-hidden className={KIND} />
                  <span>traces</span>
                  <CommandShortcut className="text-inherit opacity-60">observe</CommandShortcut>
                </CommandItem>
              </CommandGroup>
            </CommandList>
          </Command>
        </Demo>
      </Block>

      {/* ── Popover / DropdownMenu / Tooltip ────────────────────────── */}
      <Block
        id="overlay"
        title="Popover · DropdownMenu · Tooltip"
        api={`<PopoverContent className={cn(SKIN, 'border p-0 shadow-none sm:rounded')} />
<DropdownMenuContent className={cn(SKIN, 'rounded-none border shadow-none')} />
<TooltipContent className={cn(SKIN, 'border bg-background text-foreground shadow-none')} />`}
        rule={
          <>
            <p>
              All three are ink-bordered, shadow-free (<code>shadow-none</code> — the stock <code>shadow-md</code> is a
              blur, and the system has exactly one shadow: the 3px hard <code>.op-raise</code>), radius from the token,
              and mono whenever they list values.
            </p>
            <p>
              All three render in a portal outside the <code>.operator</code> root, so all three take{' '}
              <code>operator ink v1</code> on their content element. Forget it and the menu renders in the app's
              default theme with a 0.5rem radius.
            </p>
            <p>
              A dropdown is the pattern for a row's overflow actions. The row's <em>primary</em> action is never in
              there: promote and roll back are visible in the row, because a hidden action is an action the operator
              does not know exists.
            </p>
          </>
        }
      >
        <Demo label="row actions · overflow menu, destructive item opens EchoDialog">
          <div className="op-rows border">
            <div className="op-row flex items-center gap-3 text-xs">
              <span className="min-w-0 flex-1 truncate font-mono">dep_92c</span>
              <span className="hidden truncate font-mono text-[11px] text-muted-foreground sm:block">
                staging · 9bc61c0 · 2h ago
              </span>
              <Button variant="outline" size="sm" className="h-7 text-xs">
                <ArrowUpFromLine /> promote
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="outline" size="icon" className="h-7 w-7" aria-label="More actions for dep_92c">
                    <MoreHorizontal />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className={cn(SKIN, 'rounded-none border font-mono text-xs shadow-none')}>
                  <DropdownMenuLabel className="op-label px-2 py-1.5">dep_92c</DropdownMenuLabel>
                  <DropdownMenuSeparator className="bg-border" />
                  <DropdownMenuItem className="rounded-none text-xs">
                    <ExternalLink /> open build log
                  </DropdownMenuItem>
                  <DropdownMenuItem className="rounded-none text-xs">
                    <ArrowUpFromLine /> promote to production
                  </DropdownMenuItem>
                  <DropdownMenuItem className="rounded-none text-xs">
                    <RotateCcw /> roll back to dep_90e
                  </DropdownMenuItem>
                  <DropdownMenuSeparator className="bg-border" />
                  <EchoDialog
                    trigger={
                      <DropdownMenuItem
                        onSelect={(e) => e.preventDefault()}
                        className="rounded-none text-xs text-destructive"
                      >
                        <Trash2 /> delete deploy…
                      </DropdownMenuItem>
                    }
                    echo="$ temps deploy delete dep_92c --yes"
                    title="Delete dep_92c"
                    description="Removes the image and the build log. Nothing is serving it: staging moved to dep_92d 40m ago."
                    confirmWord="dep_92c"
                    steps={['detach image', 'remove build log', 'archive row']}
                    onDone={() => undefined}
                    destructive
                  />
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>
          <p className="mt-2 text-[11px] text-muted-foreground">
            promote is in the row; the menu holds only what is secondary. Delete goes through{' '}
            <Link to="/op-components#echo" className="underline underline-offset-4">
              EchoDialog
            </Link>
            .
          </p>
        </Demo>

        <Demo label="popover · a value pane, mono, no shadow">
          <Popover>
            <PopoverTrigger asChild>
              <Button variant="outline" size="sm" className="h-8 font-mono text-xs">
                dep_91a
              </Button>
            </PopoverTrigger>
            <PopoverContent align="start" className={cn(SKIN, 'w-72 border p-0 font-mono text-xs shadow-none sm:rounded')}>
              <div className="op-inset border-b px-3 py-2">dep_91a</div>
              <dl className="op-rows">
                {[
                  ['branch', 'main'],
                  ['commit', 'e4d1f0a'],
                  ['image', 'temps/api-gateway:e4d1f0a'],
                  ['deployed', '41m ago'],
                ].map(([k, v]) => (
                  <div key={k} className="flex items-baseline gap-3 px-3 py-1.5">
                    <dt className="op-label w-16 shrink-0">{k}</dt>
                    <dd className="min-w-0 flex-1 truncate">{v}</dd>
                  </div>
                ))}
              </dl>
            </PopoverContent>
          </Popover>
        </Demo>

        <Demo label="tooltip · a name, never a sentence the reader needs">
          <TooltipProvider delayDuration={150}>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="outline" size="icon" className="h-7 w-7" aria-label="Roll back">
                  <RotateCcw />
                </Button>
              </TooltipTrigger>
              <TooltipContent
                className={cn(SKIN, 'rounded-none border bg-background font-mono text-[11px] text-foreground shadow-none')}
              >
                roll back to dep_90e
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
          <p className="mt-2 text-[11px] text-muted-foreground">
            A tooltip is a label for an icon-only control, and every icon-only control needs one plus an{' '}
            <span className="font-mono">aria-label</span>. Anything the operator must read to decide belongs on the
            page.
          </p>
        </Demo>
      </Block>

      {/* ── Dialog ──────────────────────────────────────────────────── */}
      <Block
        id="dialog"
        title="Dialog"
        api={`<DialogContent className={cn(SKIN, 'gap-0 p-0 shadow-none sm:rounded')}>
  <div className="op-inset border-b px-3 py-2 font-mono text-xs">…</div>
  <div className="space-y-4 p-4">…</div>`}
        rule={
          <>
            <p>
              There is exactly one dialog pattern for anything destructive or irreversible:{' '}
              <Link to="/op-components#echo" className="underline underline-offset-4">
                EchoDialog
              </Link>
              . Typed confirmation with a copyable resource badge, step progress from the backend. Delete, destroy, roll back, revoke
              and rotate all use it, and there is no other "are you sure?" in the console.
            </p>
            <p>
              Non-destructive dialogs — connect a repository, create a database, invite a user — are not
              EchoDialog, but they wear the same chrome: ink border, no shadow, an inset header strip carrying the
              command or the identity, then the body at <code>p-4</code>, then <code>h-8</code> buttons with one{' '}
              <code>op-primary</code>.
            </p>
            <p>
              Every dialog is a portal: the skin class goes on <code>DialogContent</code>.
            </p>
          </>
        }
      >
        <Demo label="non-destructive · connect repository">
          <Dialog open={connectOpen} onOpenChange={setConnectOpen}>
            <DialogTrigger asChild>
              <Button variant="outline" size="sm" className="h-8 text-xs">
                connect repository…
              </Button>
            </DialogTrigger>
            <DialogContent className={cn(SKIN, 'gap-0 p-0 shadow-none sm:max-w-md sm:rounded')}>
              <div className="op-inset border-b px-3 py-2 font-mono text-xs">
                $ temps project connect api-gateway --repo acme/api-gateway --branch main
              </div>
              <div className="@container space-y-4 p-4">
                <div className="space-y-1">
                  <DialogTitle className="text-sm font-semibold">Connect a repository</DialogTitle>
                  <DialogDescription className="op-prose text-xs">
                    api-gateway will build from this repository on every push to the chosen branch.
                  </DialogDescription>
                </div>
                <Field label="repository" help="repositories the temps GitHub app can read">
                  <Input defaultValue="acme/api-gateway" className="h-8 font-mono text-xs" />
                </Field>
                <Field label="branch" help="9 branches · main is 1 deploy ahead of production">
                  <Picker
                    value={branch}
                    onChange={setBranch}
                    options={BRANCHES}
                    allowCustom="use branch"
                    skin={SKIN}
                  />
                </Field>
                <div className="flex flex-wrap justify-end gap-2">
                  <Button variant="outline" size="sm" className="h-8 text-xs" onClick={() => setConnectOpen(false)}>
                    cancel <Kbd keys="esc" className="ml-1 opacity-70" />
                  </Button>
                  <Button size="sm" className="op-primary h-8 text-xs" onClick={() => setConnectOpen(false)}>
                    connect <Kbd keys="⏎" className="ml-1 opacity-70" />
                  </Button>
                </div>
              </div>
            </DialogContent>
          </Dialog>
        </Demo>

        <Demo label="destructive · the only confirm dialog in the system">
          <EchoDialog
            trigger={
              <Button variant="outline" size="sm" className="h-8 border-destructive text-xs text-destructive">
                roll back to dep_90e…
              </Button>
            }
            echo="$ temps deploy rollback api-gateway --to dep_90e"
            title="Roll back api-gateway"
            description="Production returns to dep_90e (main · 7a11c3e). dep_91a stays available to redeploy."
            confirmWord="api-gateway"
            steps={['pull dep_90e image', 'start containers', 'health check', 'swap proxy routes']}
            onDone={() => undefined}
          />
        </Demo>

        <Rule state="error">
          A stock <span className="font-mono">AlertDialog</span> asking "Are you sure? This cannot be undone." It names
          nothing, echoes nothing, and a mis-click deletes production.
        </Rule>
        <Rule state="ok">
          EchoDialog: the command it will run, the resource name typed by hand, the backend's own steps ticking.
        </Rule>
      </Block>

      {/* ── Toast / notifications ───────────────────────────────────── */}
      <Block
        id="toast"
        title="Toast · notifications"
        api={`notify('ok',   'deployed dep_91a', 'api-gateway · production · 41s')
notify('warn', '30d is beyond this plan\\'s retention', 'Team keeps 90d')
notify('err',  'build failed', 'billing-worker · missing Dockerfile')

toast.custom(() => <div className={cn(SKIN, 'border bg-background …')}>…`}
        rule={
          <>
            <p>
              One shape: <code>notify(state, title, detail)</code>. A glyph column carrying the state word, one
              sentence naming what happened to which resource, and an optional mono detail line. Timestamp on the
              right, mono and tabular.
            </p>
            <p>
              Every toast is also pushed to the notifications drawer in the header, newest first, so an operator who
              looked away has not lost the deploy result. A toast is never the only record of anything.
            </p>
            <p>
              A toast reports something that already happened. It is not a page state: a surface that failed to load
              renders <code>PageState error</code> with a retry, not a toast over an empty page. Toasts render in a
              portal, so the custom body carries the skin class.
            </p>
          </>
        }
      >
        <Demo label="the three levels">
          <div className="space-y-2">
            <Toast level="ok" title="deployed dep_91a" detail="api-gateway · production · 41s" ts="14:02:11" />
            <Toast
              level="warn"
              title="30d is beyond this plan's retention"
              detail="Team keeps 90d · Business keeps 13 months"
              ts="14:02:44"
            />
            <Toast level="err" title="build failed" detail="billing-worker · no Dockerfile at repository root" ts="14:03:09" />
          </div>
        </Demo>

        <Demo label="notifications drawer · the same rows, shared history">
          <div className="w-full max-w-[360px] border bg-background">
            <div className="flex items-center justify-between border-b px-3 py-2">
              <span className="op-label">notifications</span>
              <span className="text-[11px] text-muted-foreground">3 · newest first</span>
            </div>
            <div className="op-rows">
              <div className="px-3 py-2">
                <Toast level="err" title="build failed" detail="billing-worker · no Dockerfile" ts="14:03:09" />
              </div>
              <div className="px-3 py-2">
                <Toast level="warn" title="30d is beyond this plan's retention" detail="Team keeps 90d" ts="14:02:44" />
              </div>
              <div className="px-3 py-2">
                <Toast level="ok" title="deployed dep_91a" detail="api-gateway · production" ts="14:02:11" />
              </div>
            </div>
          </div>
          <p className="mt-2 text-[11px] text-muted-foreground">
            The rows above are nested inside the drawer for this demo; in the console both call the same{' '}
            <span className="font-mono">NoteRow</span>.
          </p>
        </Demo>
      </Block>

      {/* ── Skeleton / pending ──────────────────────────────────────── */}
      <Block
        id="skeleton"
        title="Skeleton · pending"
        api={`<PageState state="loading" rows={4} />        // the only loading UI

<Button className="op-primary h-8 text-xs" disabled>
  <Loader2 className="animate-spin" /> deploying…
</Button>`}
        rule={
          <>
            <p>
              Content that is loading renders{' '}
              <Link to="/op-components#page-state" className="underline underline-offset-4">
                PageState loading
              </Link>{' '}
              — skeleton rows the shape of the rows that are coming, so the page does not collapse and then expand
              when the data lands. Skeletons are square: <code>rounded-none</code>, <code>bg-muted</code>.
            </p>
            <p>
              A spinner is legal in exactly one place: inside a button the operator just pressed, where it means "your
              action is running", not "content is loading". It disappears with the action, and the button stays
              disabled until it does.
            </p>
            <p>
              Never a centred spinner as a page state. It says nothing about what is loading, from where, or what to do
              if it never arrives.
            </p>
          </>
        }
      >
        <Demo label="loading · skeleton rows shaped like the real rows">
          <PageState state="loading" rows={4} />
        </Demo>

        <Demo label="pending · the only legal spinner">
          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              className="op-primary h-8 text-xs"
              disabled={pending}
              onClick={() => {
                setPending(true)
                window.setTimeout(() => setPending(false), 2400)
              }}
            >
              {pending ? (
                <>
                  <Loader2 className="animate-spin" /> deploying…
                </>
              ) : (
                <>
                  deploy api-gateway <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" />
                </>
              )}
            </Button>
            {pending && <span className="font-mono text-[11px] text-muted-foreground">step 2 of 4 · build image</span>}
          </div>
        </Demo>

        <Rule state="error">A centred spinner where the page should be. It names nothing and never fails.</Rule>
        <Rule state="ok">
          Skeleton rows for content, a button spinner for an action, <span className="font-mono">PageState error</span>{' '}
          with a retry when it does not arrive.
        </Rule>
      </Block>

      {/* ── Kbd ─────────────────────────────────────────────────────── */}
      <Block
        id="kbd"
        title="Kbd"
        api={`<Kbd keys={['⌘', '⏎']} />   ⌘⏎ on macOS, Ctrl⏎ elsewhere
<Kbd keys="/" />           filter
import { MOD } from '@/components/op'   // '⌘' | 'Ctrl'`}
        rule={
          <>
            <p>
              A key badge is an accelerator sitting on a control that already exists. It is never the only way to reach
              an action — full reference on{' '}
              <Link to="/op-components#kbd" className="underline underline-offset-4">
                /op-components#kbd
              </Link>
              .
            </p>
            <p>
              Platform-aware: pass <code>'⌘'</code> and it renders <code>{MOD}</code> here. Inside an ink-filled button
              the badge inverts automatically, so it stays readable on the fill.
            </p>
          </>
        }
      >
        <Demo label={`platform · this browser resolves ⌘ to ${MOD}`}>
          <div className="flex flex-wrap items-center gap-4 text-xs">
            <span className="flex items-center gap-2">
              <Kbd keys={['⌘', 'K']} /> find
            </span>
            <span className="flex items-center gap-2">
              <Kbd keys={['⌘', 'S']} /> save
            </span>
            <span className="flex items-center gap-2">
              <Kbd keys={['⌘', '⏎']} /> deploy
            </span>
            <span className="flex items-center gap-2">
              <Kbd keys="/" /> filter
            </span>
            <span className="flex items-center gap-2">
              <Kbd keys="j" />
              <Kbd keys="k" /> move
            </span>
            <span className="flex items-center gap-2">
              <Kbd keys="esc" /> close
            </span>
          </div>
        </Demo>

        <Demo label="on a fill · the badge inverts">
          <Button size="sm" className="op-primary h-8 text-xs">
            deploy <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" />
          </Button>
        </Demo>
      </Block>

      {/* ── Breadcrumb / PageTitle ──────────────────────────────────── */}
      <Block
        id="breadcrumb"
        title="Breadcrumb and PageTitle"
        api={`// shell header — navigation
<nav className="flex items-center gap-1 text-xs text-muted-foreground">
  <a>projects</a><span>/</span><span className="text-foreground">api-gateway</span>

// page — identity
<PageTitle title="api-gateway" meta="production · dep_91a · main" />`}
        rule={
          <>
            <p>
              They are two different jobs and both are needed. The breadcrumb lives in the 44px header and answers
              "where am I in the app": <code>text-xs</code>, muted, plain <code>/</code> separators, only the last
              segment in ink. It never grows a chevron icon or a dropdown.
            </p>
            <p>
              <code>PageTitle</code> lives on the page and answers "what am I looking at": the screen's name in{' '}
              <code>.op-title</code> — the one 700-weight line on a console screen — plus one or two mono facts that
              place it (<code>production · dep_91a · main</code>).
            </p>
          </>
        }
      >
        <Demo label="breadcrumb · header strip">
          <div className="flex h-11 items-center gap-2 border px-3 text-xs">
            <Breadcrumb>
              <BreadcrumbList className="gap-1 text-xs text-muted-foreground sm:gap-1">
                <BreadcrumbItem>
                  <BreadcrumbLink href="#">projects</BreadcrumbLink>
                </BreadcrumbItem>
                <BreadcrumbSeparator className="[&>svg]:hidden">/</BreadcrumbSeparator>
                <BreadcrumbItem>
                  <BreadcrumbPage className="text-foreground">api-gateway</BreadcrumbPage>
                </BreadcrumbItem>
              </BreadcrumbList>
            </Breadcrumb>
          </div>
        </Demo>

        <Demo label="PageTitle · identity">
          <PageTitle title="api-gateway" meta="production · dep_91a · main" />
        </Demo>

        <Demo label="both, as the console assembles them">
          <div className="border">
            <div className="flex h-11 items-center gap-2 border-b px-3 text-xs">
              <nav aria-label="Breadcrumb" className="flex min-w-0 items-center gap-1 truncate text-muted-foreground">
                <a href="#" className="hover:text-foreground">
                  projects
                </a>
                <span>/</span>
                <span className="text-foreground">api-gateway</span>
              </nav>
            </div>
            <div className="p-4">
              <PageTitle title="api-gateway" meta="production · dep_91a · main" />
            </div>
          </div>
        </Demo>
      </Block>

      {/* ── Identity ────────────────────────────────────────────────── */}
      <Block
        id="identity"
        title="Identity"
        api={`<span className="flex h-5 w-5 items-center justify-center
                 border font-mono text-[10px]">D</span>`}
        rule={
          <>
            <p>
              A person or a machine identity is initials in a bordered square, mono, sized to the row it sits in (h-5
              in the header, h-6 in a list). No circle, no gradient, no generated colour: colour means status, and an
              avatar has no status.
            </p>
            <p>
              The initials are never the only identification — the name sits beside them wherever there is room, and
              is the accessible label where there is not.
            </p>
          </>
        }
      >
        <Demo label="header · identity block">
          <div className="flex h-11 items-center gap-2 border px-3 text-xs">
            <span className="ml-auto flex items-center gap-2 border-l pl-3">
              <span aria-hidden className="flex h-5 w-5 items-center justify-center border font-mono text-[10px]">
                M
              </span>
              <span className="text-muted-foreground">maya</span>
            </span>
          </div>
        </Demo>

        <Demo label="a list of members and machine identities">
          <div className="op-rows border text-xs">
            {[
              ['MA', 'maya', 'owner · last active 4m ago'],
              ['RK', 'r.kowalski', 'admin · last active yesterday'],
              ['CI', 'ci-deploy-bot', 'api key · 2,140 calls in 24h'],
            ].map(([initials, name, meta]) => (
              <div key={name} className="op-row flex items-center gap-3">
                <span
                  aria-hidden
                  className="flex h-6 w-6 shrink-0 items-center justify-center border font-mono text-[10px]"
                >
                  {initials}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono">{name}</span>
                <span className="hidden shrink-0 truncate text-[11px] text-muted-foreground sm:block">{meta}</span>
              </div>
            ))}
          </div>
        </Demo>
      </Block>

      {/* ── Banned ──────────────────────────────────────────────────── */}
      <Block
        id="banned"
        title="Banned"
        rule={
          <>
            <p>
              These are the primitives v1 replaced, and what replaced them. They are not style preferences: each one
              adds a surface treatment, a radius or a hue the system does not have, and the console's drift is what
              2,265 palette literals across 189 files looks like.
            </p>
            <p>
              Also banned everywhere: Tailwind palette literals and hex in tsx, a second hue, titles at weight 500, and
              hiding a feature because it is not configured or not on the plan.
            </p>
          </>
        }
      >
        <Demo label="replaced primitives">
          <div className="op-rows border">
            {[
              [
                <>
                  <span className="font-mono">&lt;Card&gt;</span> as layout — a rounded, shadowed box around every
                  group (214 files).
                </>,
                <>
                  A grid with ink borders. One <span className="font-mono">.op-raise</span> per screen, on the thing
                  the reader is meant to act on.
                </>,
              ],
              [
                <>
                  <span className="font-mono">&lt;Badge&gt;</span> for status — a coloured pill that reads as
                  decoration and carries no word.
                </>,
                <>
                  <Link to="/op-components#status" className="underline underline-offset-4">
                    Status
                  </Link>
                  : glyph + word, five states, colour only through it.
                </>,
              ],
              [
                <>Pill tabs and underline tabs — a second radius and a second active treatment.</>,
                <>
                  The Detail tablist (<span className="font-mono">bg-foreground text-background</span>) for navigation,{' '}
                  <span className="font-mono">Segmented</span> (<span className="font-mono">bg-muted</span>) for a
                  parameter.
                </>,
              ],
              [
                <>
                  <span className="font-mono">Loader2</span> as page state — a centred spinner that names nothing and
                  never fails (134 files).
                </>,
                <>
                  <span className="font-mono">PageState loading</span> skeletons;{' '}
                  <span className="font-mono">PageState error</span> with the resource and a retry. A spinner only
                  inside a pressed button.
                </>,
              ],
              [
                <>
                  <span className="font-mono">EmptyPlaceholder</span> and two other empty-state components — three
                  implementations, blank in the unconfigured case.
                </>,
                <>
                  One <span className="font-mono">PageState</span> with four states, including{' '}
                  <span className="font-mono">unconfigured</span>: what is missing, an example, a link to the settings
                  page.
                </>,
              ],
              [
                <>
                  Stock <span className="font-mono">&lt;AlertDialog&gt;</span> — "Are you sure? This cannot be undone."
                </>,
                <>
                  <Link to="/op-components#echo" className="underline underline-offset-4">
                    EchoDialog
                  </Link>
                  : typed resource name with a copy badge, backend steps. The only confirm dialog.
                </>,
              ],
              [
                <>
                  <span className="font-mono">&lt;Table&gt;</span> with zebra rows and hover shadows (52 files).
                </>,
                <>
                  <span className="font-mono">.op-rows</span> / <span className="font-mono">.op-row</span> with{' '}
                  <span className="font-mono">--cols</span>, and the{' '}
                  <Link to="/op-components#ledger" className="underline underline-offset-4">
                    Ledger
                  </Link>{' '}
                  template above them.
                </>,
              ],
              [
                <>
                  A plain <span className="font-mono">&lt;select&gt;</span> for branches, images, regions or
                  environments.
                </>,
                <>
                  <Link to="/op-components#picker" className="underline underline-offset-4">
                    Picker
                  </Link>
                  , with filter, groups, meta, and loading/error as states in the list.
                </>,
              ],
            ].map(([bad, good], i) => (
              <div key={i} className="grid gap-2 p-3 md:grid-cols-2 md:gap-6">
                <div className="min-w-0">
                  <Rule state="error">{bad}</Rule>
                </div>
                <div className="min-w-0">
                  <Rule state="ok">{good}</Rule>
                </div>
              </div>
            ))}
          </div>
        </Demo>
      </Block>
    </DocPage>
  )
}
