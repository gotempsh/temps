import { Link } from 'react-router-dom'
import {
  ArrowRight,
  BarChart3,
  BookOpen,
  Boxes,
  Bug,
  Database,
  GitBranch,
  HardDrive,
  Sparkles,
  Terminal,
  Upload,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { ConnectionList } from '@/components/dashboard/ConnectionList'
import { InlineGitConnect } from '@/components/dashboard/InlineGitConnect'
import { cn } from '@/lib/utils'

interface FirstProjectOnboardingProps {
  /**
   * Whether a Git provider is already connected. When true the primary path
   * skips the "connect a provider" step and routes straight into the import
   * wizard, where the user picks a repo, attaches a database, and deploys.
   */
  gitConnected: boolean
}

// Deep-link into the project creation flow with the Observability Starter
// template pre-selected. The configurator reads `?source=templates&template=`
// (see GitImportClone), so this is a true one-click deploy: pick the template,
// Temps provisions the attached Postgres service, and the app deploys with
// analytics, error tracking, and tracing already wired. The slug must match the
// template registered in `temps-core/templates.yaml`.
const DEMO_TEMPLATE_SLUG = 'observability-starter'
const DEMO_TEMPLATE_HREF = `/projects/new?source=templates&template=${DEMO_TEMPLATE_SLUG}`

// What the demo app lights up, shown as inline pills so the value is legible at
// a glance without reading prose.
const DEMO_HIGHLIGHTS: ReadonlyArray<{
  label: string
  icon: React.ComponentType<{ className?: string }>
}> = [
  { label: 'Analytics', icon: BarChart3 },
  { label: 'Error tracking', icon: Bug },
  { label: 'Database', icon: Database },
] as const

// The two copy-paste commands for the local/CLI deploy path. `login` is the
// browser device-auth flow (no API key to mint or manage); `up` runs the setup
// wizard and deploys the current directory. The login command is pinned to THIS
// server's origin so the user authenticates against the instance they're
// actually looking at — not the CLI's localhost default. The CLI's `login`
// accepts a bare origin and appends `/api` itself, and `up` reuses the saved
// context, so it needs no URL.
function buildCliCommands(origin: string): readonly string[] {
  return [`bunx @temps-sdk/cli login ${origin}`, 'bunx @temps-sdk/cli up']
}

// The micro-steps shown under the CLI deploy path, so a new user can see the
// whole journey to a live URL at a glance rather than discovering it one screen
// at a time.
const CLI_STEPS = [
  'Authorize the CLI in your browser',
  'Temps detects your framework',
  'Build, push, and deploy from this folder',
] as const

// Databases / storage a project can provision. Slugs match `ServiceTypeRoute`
// in the generated API types, so each tile deep-links to the create screen with
// the engine pre-selected. MySQL is intentionally absent — Temps does not ship
// a MySQL engine, so advertising it would dead-end.
const DATABASES: ReadonlyArray<{
  slug: string
  label: string
  description: string
  icon: React.ComponentType<{ className?: string }>
}> = [
  {
    slug: 'postgres',
    label: 'PostgreSQL',
    description: 'Relational database',
    icon: Database,
  },
  {
    slug: 'redis',
    label: 'Redis',
    description: 'In-memory cache & queues',
    icon: Boxes,
  },
  {
    slug: 'mongodb',
    label: 'MongoDB',
    description: 'Document database',
    icon: Database,
  },
  {
    slug: 'rustfs',
    label: 'Object storage',
    description: 'S3-compatible buckets',
    icon: HardDrive,
  },
]

/**
 * First-run empty state for the project list. The goal is to reach the
 * activation point — a live deployment — in as few clicks as possible, while
 * making it obvious that a project can bring its own database (Postgres,
 * Redis, MongoDB, or object storage) along for the ride.
 *
 * Three sections, all wired to real flows:
 *   1. Deploy from Git — connect a provider (or, if one is linked, jump
 *      straight to the import wizard) to pick a repo + attach a database.
 *   2. Deploy from your machine (CLI) — `login` then `up` deploys the current
 *      directory.
 *   3. Add a database — engine tiles that deep-link into the storage create
 *      screen. The recommended way to get a database is inline during import,
 *      so the app and its database land together; this is the up-front path.
 */
export function FirstProjectOnboarding({
  gitConnected,
}: FirstProjectOnboardingProps) {
  // When git is connected the user has already done the hard part — send them
  // straight to the Git repository browser (`/projects/new?source=browse`),
  // NOT the import wizard (which is for Docker/Kubernetes workloads).

  // Pin the CLI login to this server's origin (protocol + host + port) so the
  // commands work for whatever URL the user opened the console at. Guard
  // `window` so it stays safe if ever server-rendered.
  const origin =
    typeof window !== 'undefined' ? window.location.origin : ''
  const cliCommands = buildCliCommands(origin)

  return (
    <div className="col-span-full min-w-0 rounded-lg border bg-card p-4 sm:p-8 lg:p-10 animate-in fade-in-50">
      {/* One-click "try Temps" path. This whole component only renders on an
          empty instance (the project list owns that branch), so the banner is
          guaranteed to show only when there are no deployed projects. It
          deploys a demo app that comes pre-wired with analytics, error
          tracking, tracing, and a Postgres database — so a new user reaches the
          activation moment (live URL + real telemetry) without first building a
          project of their own. */}
      <DemoAppBanner />

      <div className="mx-auto max-w-2xl text-center">
        <div className="mx-auto flex h-11 w-11 items-center justify-center rounded-xl bg-primary/10 sm:h-12 sm:w-12">
          <Upload className="h-5 w-5 text-primary sm:h-6 sm:w-6" />
        </div>
        <h2 className="mt-4 text-xl font-semibold tracking-tight sm:text-2xl">
          Deploy your first project
        </h2>
        <p className="mt-2 text-sm text-balance text-muted-foreground">
          Ship from Git or straight from your machine — a live URL in a couple of
          minutes. Need Postgres, Redis, or MongoDB? Add it as you go, and your
          app and its database deploy together.
        </p>
      </div>

      <div className="relative mx-auto mt-6 flex max-w-5xl flex-col gap-4 sm:mt-8 md:grid md:grid-cols-2 md:gap-8">
        {/* "or" divider between the two alternative deploy paths. On md+ it's an
            absolutely-positioned vertical rule in the gutter; below md it falls
            back to a normal flow element rendered between the stacked cards (see
            the inline "or" below), which is far more robust than positioning an
            absolute element at the grid's mid-point when the cards stack. */}
        <div
          aria-hidden
          className="pointer-events-none absolute inset-y-0 left-1/2 z-10 hidden -translate-x-1/2 md:block"
        >
          <div className="relative flex h-full w-px items-center justify-center">
            <span className="h-full w-px bg-border" />
            <span className="absolute flex h-7 min-w-7 items-center justify-center rounded-full border border-border bg-card px-2 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              or
            </span>
          </div>
        </div>

        {/* Path A — Deploy from Git (primary) */}
        <div className="flex flex-col rounded-xl border bg-background p-5 text-left sm:p-6">
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-primary/10">
              <GitBranch className="h-5 w-5 text-primary" />
            </div>
            <div className="min-w-0">
              <h3 className="text-base font-semibold">Deploy from Git</h3>
              <p className="text-xs text-muted-foreground">
                Git-push deploys with automatic builds
              </p>
            </div>
          </div>

          {gitConnected ? (
            // Provider already linked — list the connected accounts so the user
            // imports a repo from exactly the one they mean (each row deep-links
            // to that connection's repository browser).
            <ConnectionList />
          ) : (
            // No provider yet — connect one inline with a PAT (the happy path),
            // no detour to the full setup screen. On success it navigates
            // straight to repo selection for the new connection.
            <InlineGitConnect />
          )}
        </div>

        {/* Mobile-only "or" between the stacked cards (the md+ vertical divider
            above is hidden below md). */}
        <div
          aria-hidden
          className="relative flex items-center justify-center md:hidden"
        >
          <span className="h-px w-full bg-border" />
          <span className="absolute flex h-7 min-w-7 items-center justify-center rounded-full border border-border bg-card px-2 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            or
          </span>
        </div>

        {/* Path B — Deploy from your machine (CLI) */}
        <div className="flex flex-col rounded-xl border bg-background p-5 text-left sm:p-6">
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-muted">
              <Terminal className="h-5 w-5 text-muted-foreground" />
            </div>
            <div className="min-w-0">
              <h3 className="text-base font-semibold">Deploy from your machine</h3>
              <p className="text-xs text-muted-foreground">
                No Git provider required
              </p>
            </div>
          </div>

          <div className="mt-4 space-y-2">
            {cliCommands.map((cmd) => (
              <CliCommand key={cmd} command={cmd} />
            ))}
          </div>

          <ol className="mt-4 flex-1 space-y-2">
            {CLI_STEPS.map((step, i) => (
              <Step key={step} index={i + 1} label={step} />
            ))}
          </ol>
        </div>
      </div>

      {/* Add a database — engine tiles that deep-link into the create screen
          with the engine pre-selected. The primary path to a database is inline
          during import (above); this is the up-front option. */}
      <div className="mx-auto mt-6 max-w-5xl rounded-xl border bg-background p-5 sm:p-6">
        <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-muted">
              <Database className="h-5 w-5 text-muted-foreground" />
            </div>
            <div className="min-w-0">
              <h3 className="text-base font-semibold">Add a database</h3>
              <p className="text-xs text-muted-foreground">
                Provision a managed service to attach to a project
              </p>
            </div>
          </div>
        </div>

        <div className="mt-4 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {DATABASES.map((db) => (
            <DatabaseTile key={db.slug} {...db} />
          ))}
        </div>
      </div>

      {/* Footer — secondary, but keeps the panel feeling complete: docs for the
          undecided, and the import-existing path for users migrating in. */}
      <div className="mx-auto mt-6 flex max-w-5xl flex-col items-center justify-center gap-x-6 gap-y-2 border-t pt-5 text-sm sm:flex-row">
        <Button asChild variant="link" className="h-auto p-0 text-muted-foreground">
          <Link to="/projects/import-wizard" className="flex items-center gap-1.5">
            <Upload className="h-3.5 w-3.5" />
            Import an existing workload
          </Link>
        </Button>
        <Button asChild variant="link" className="h-auto p-0 text-muted-foreground">
          <a
            href="https://temps.sh/docs"
            target="_blank"
            rel="noreferrer"
            className="flex items-center gap-1.5"
          >
            <BookOpen className="h-3.5 w-3.5" />
            Read the deployment docs
          </a>
        </Button>
      </div>
    </div>
  )
}

function DemoAppBanner() {
  return (
    <Link
      to={DEMO_TEMPLATE_HREF}
      className={cn(
        'group mb-6 flex flex-col gap-4 rounded-xl border border-primary/30 bg-primary/5 p-5 text-left transition-colors sm:mb-8 sm:flex-row sm:items-center sm:justify-between sm:p-6',
        'hover:border-primary/50 hover:bg-primary/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
      )}
    >
      <div className="flex items-start gap-3 sm:items-center">
        <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-primary/15">
          <Sparkles className="h-5 w-5 text-primary" />
        </div>
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <h3 className="text-base font-semibold">Try the demo app</h3>
            <span className="rounded-full bg-primary/15 px-2 py-0.5 text-[11px] font-medium uppercase tracking-wide text-primary">
              No setup
            </span>
          </div>
          <p className="mt-0.5 text-sm text-muted-foreground">
            Deploy a sample app with a database in one click — see analytics,
            error tracking, and tracing light up live.
          </p>
          <div className="mt-2 flex flex-wrap gap-1.5">
            {DEMO_HIGHLIGHTS.map(({ label, icon: Icon }) => (
              <span
                key={label}
                className="inline-flex items-center gap-1 rounded-md border bg-background px-2 py-0.5 text-xs text-muted-foreground"
              >
                <Icon className="h-3 w-3" />
                {label}
              </span>
            ))}
          </div>
        </div>
      </div>
      <Button
        asChild
        className="w-full shrink-0 sm:w-auto"
        // The whole banner is a link; render the CTA as a non-interactive span
        // so it doesn't nest an anchor inside an anchor.
      >
        <span>
          Deploy demo
          <ArrowRight className="ml-1.5 h-4 w-4 transition-transform group-hover:translate-x-0.5" />
        </span>
      </Button>
    </Link>
  )
}

function Step({ index, label }: { index: number; label: string }) {
  return (
    <li className="flex items-center gap-2.5 text-sm text-muted-foreground">
      <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-muted text-[11px] font-medium text-foreground">
        {index}
      </span>
      {label}
    </li>
  )
}

function DatabaseTile({
  slug,
  label,
  description,
  icon: Icon,
}: {
  slug: string
  label: string
  description: string
  icon: React.ComponentType<{ className?: string }>
}) {
  return (
    <Link
      to={`/storage/create?type=${slug}`}
      className={cn(
        'group flex items-center gap-3 rounded-lg border bg-card p-3 text-left transition-colors',
        'hover:border-primary/50 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
      )}
    >
      <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-muted">
        <Icon className="h-4 w-4 text-muted-foreground" />
      </div>
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">{label}</p>
        <p className="truncate text-xs text-muted-foreground">{description}</p>
      </div>
      <ArrowRight className="h-4 w-4 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
    </Link>
  )
}

function CliCommand({ command }: { command: string }) {
  // Split on whitespace — these commands have no quoting, so keep it simple.
  // Only handles the `bunx @temps-sdk/cli <subcommand> [url]` shape.
  const tokens = command.split(' ')
  const packageIndex = tokens.findIndex((t) => t.startsWith('@'))

  const colorFor = (token: string, i: number): string => {
    if (/^https?:\/\//.test(token)) return 'text-amber-600 dark:text-amber-400' // URL arg
    if (i === 0) return 'text-emerald-600 dark:text-emerald-400' // runner (bunx)
    if (i === packageIndex) return 'text-foreground font-medium' // package
    if (packageIndex !== -1 && i === packageIndex + 1)
      return 'text-sky-600 dark:text-sky-400' // subcommand (login / up)
    return 'text-muted-foreground' // anything else (flags, extra args)
  }

  return (
    <div
      className={cn(
        'flex items-center gap-2 rounded-md border bg-muted/50 pl-3 pr-1 py-2',
        'font-mono text-xs sm:text-sm'
      )}
    >
      <span className="shrink-0 select-none text-muted-foreground">$</span>
      {/* Horizontal scroll instead of truncation so the whole command — URL
          and all — is readable. `min-w-0` lets the flex child actually shrink
          so the scroll container kicks in rather than overflowing the card. */}
      <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
        {tokens.map((token, i) => (
          <span key={i}>
            {i > 0 && ' '}
            <span className={colorFor(token, i)}>{token}</span>
          </span>
        ))}
      </code>
      <CopyButton
        value={command}
        minimal
        className="h-7 w-7 shrink-0 rounded-md text-muted-foreground"
        aria-label={`Copy "${command}"`}
      />
    </div>
  )
}
