import { Link } from 'react-router'
import {
  Activity,
  ArrowRight,
  BarChart3,
  BookOpen,
  Bug,
  Database,
  GitBranch,
  Globe,
  Mail,
  Network,
  Play,
  ScrollText,
  Sparkles,
  Terminal,
  Upload,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { ConnectionList } from '@/components/dashboard/ConnectionList'
import { InlineGitConnect } from '@/components/dashboard/InlineGitConnect'
import { SourceLogo } from '@/components/imports/SourceLogo'
import {
  TOP_MIGRATION_SOURCES,
  importHref,
} from '@/components/imports/migration-sources'
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

// The full platform, shown as a grid so a new user sees the breadth at a glance
// — every one of these lights up with real data when they deploy the demo, and
// each links to the docs that show how to add the same to their OWN project.
const SHOWCASE: ReadonlyArray<{
  icon: React.ComponentType<{ className?: string }>
  name: string
  blurb: string
  href: string
}> = [
  {
    icon: BarChart3,
    name: 'Web analytics',
    blurb: 'Visitors, pages, funnels, and a live globe — no third-party scripts.',
    href: 'https://temps.sh/docs/analytics',
  },
  {
    icon: Bug,
    name: 'Error tracking',
    blurb: 'Sentry-compatible exceptions with stack traces and AI autofix.',
    href: 'https://temps.sh/docs/error-tracking',
  },
  {
    icon: Network,
    name: 'Tracing',
    blurb: 'OpenTelemetry span waterfalls across your services.',
    href: 'https://temps.sh/docs/opentelemetry',
  },
  {
    icon: ScrollText,
    name: 'Request logs',
    blurb: 'Every request: method, status, latency, and geo.',
    href: 'https://temps.sh/docs/logs',
  },
  {
    icon: Play,
    name: 'Session replay',
    blurb: 'Watch real sessions to see what a user actually did.',
    href: 'https://temps.sh/docs/session-replay',
  },
  {
    icon: Activity,
    name: 'Uptime monitoring',
    blurb: 'Health checks with instant Slack, email, or webhook alerts.',
    href: 'https://temps.sh/docs/monitoring',
  },
  {
    icon: Database,
    name: 'Managed databases',
    blurb: 'Postgres, Redis, MongoDB, and S3-compatible storage.',
    href: 'https://temps.sh/docs/databases',
  },
  {
    icon: Mail,
    name: 'Transactional email',
    blurb: 'Send via SES, Scaleway, or SMTP with DKIM signing.',
    href: 'https://temps.sh/docs/email',
  },
]

// The two copy-paste commands for the local/CLI deploy path. `login` is the
// browser device-auth flow (no API key to mint or manage); `up` runs the setup
// wizard and deploys the current directory. The login command is pinned to THIS
// server's origin so the user authenticates against the instance they're
// actually looking at — not the CLI's localhost default.
function buildCliCommands(origin: string): readonly string[] {
  return [`bunx @temps-sdk/cli login ${origin}`, 'bunx @temps-sdk/cli up']
}

const CLI_STEPS = [
  'Authorize the CLI in your browser',
  'Temps detects your framework',
  'Build, push, and deploy from this folder',
] as const

/**
 * First-run empty state for the project list. Structured as a showcase-first
 * pitch: lead with the one-click demo (the fastest way to see the whole
 * platform light up with real data), show everything you get, then the two
 * ways to deploy your own app. Only renders on an empty instance.
 */
export function FirstProjectOnboarding({
  gitConnected,
}: FirstProjectOnboardingProps) {
  const origin = typeof window !== 'undefined' ? window.location.origin : ''
  const cliCommands = buildCliCommands(origin)

  return (
    <div className="col-span-full min-w-0 space-y-6 animate-in fade-in-50">
      {/* Hero — the demo is the wow entry point. */}
      <div className="relative overflow-hidden rounded-2xl border border-primary/20 bg-card">
        <div
          className="p-6 sm:p-8 lg:p-10"
          style={{
            backgroundImage:
              'radial-gradient(120% 120% at 15% 0%, color-mix(in oklch, var(--primary) 10%, transparent) 0%, transparent 55%)',
          }}
        >
          <div className="flex flex-col gap-6 lg:flex-row lg:items-center lg:justify-between">
            <div className="max-w-2xl">
              <span className="inline-flex items-center gap-1.5 rounded-full border border-primary/30 bg-primary/10 px-2.5 py-1 text-xs font-medium text-primary">
                <Sparkles className="h-3.5 w-3.5" />
                No setup required
              </span>
              <h2 className="mt-4 text-2xl font-semibold tracking-tight text-balance sm:text-3xl">
                Deploy one app. Watch your whole stack light up.
              </h2>
              <p className="mt-3 text-sm text-muted-foreground text-balance sm:text-base">
                One click ships a sample app with a database — pre-wired with
                analytics, error tracking, logs, and tracing. Deploy it to see
                exactly what Temps gives you, then add the same to your own
                projects.
              </p>
              <div className="mt-5 flex flex-col gap-3 sm:flex-row sm:items-center">
                <Button asChild size="lg" className="group">
                  <Link to={DEMO_TEMPLATE_HREF}>
                    Deploy the demo app
                    <ArrowRight className="ml-1.5 h-4 w-4 transition-transform group-hover:translate-x-0.5" />
                  </Link>
                </Button>
                <a
                  href="#deploy-your-own"
                  className="text-sm font-medium text-muted-foreground hover:text-foreground"
                >
                  or deploy your own app ↓
                </a>
              </div>
            </div>

            {/* Payoff glyph — the live globe is the single most striking
                Temps feature, so it anchors the hero. */}
            <div className="hidden shrink-0 lg:block">
              <div className="flex h-32 w-32 items-center justify-center rounded-full border border-primary/20 bg-primary/5">
                <Globe className="h-16 w-16 text-primary/70" />
              </div>
            </div>
          </div>
        </div>
      </div>

      {/* Showcase — the whole platform, and how to add each to your project. */}
      <section className="rounded-2xl border bg-card p-6 sm:p-8">
        <div className="mb-5 sm:mb-6">
          <h3 className="text-lg font-semibold tracking-tight">
            Everything you get, in one platform
          </h3>
          <p className="mt-1 text-sm text-muted-foreground">
            Deploy the demo to see these populate with live data — or click any
            to learn how to add it to your own app.
          </p>
        </div>
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {SHOWCASE.map((f) => (
            <ShowcaseCard key={f.name} {...f} />
          ))}
        </div>
      </section>

      {/* Deploy your own — Git / CLI. */}
      <section
        id="deploy-your-own"
        className="rounded-2xl border bg-card p-6 sm:p-8"
      >
        <div className="mx-auto mb-6 max-w-2xl text-center">
          <div className="mx-auto flex h-11 w-11 items-center justify-center rounded-xl bg-muted">
            <Upload className="h-5 w-5 text-muted-foreground" />
          </div>
          <h3 className="mt-4 text-xl font-semibold tracking-tight">
            Deploy your own app
          </h3>
          <p className="mt-2 text-sm text-balance text-muted-foreground">
            Ship from Git or straight from your machine — a live URL in a couple
            of minutes. Attach a Postgres, Redis, or MongoDB as you go.
          </p>
        </div>

        <div className="relative mx-auto flex max-w-5xl flex-col gap-4 md:grid md:grid-cols-2 md:gap-8">
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

          {/* Path A — Deploy from Git */}
          <div className="flex flex-col rounded-xl border bg-background p-5 text-left sm:p-6">
            <div className="flex items-center gap-3">
              <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-primary/10">
                <GitBranch className="h-5 w-5 text-primary" />
              </div>
              <div className="min-w-0">
                <h4 className="text-base font-semibold">Deploy from Git</h4>
                <p className="text-xs text-muted-foreground">
                  Git-push deploys with automatic builds
                </p>
              </div>
            </div>
            {gitConnected ? <ConnectionList /> : <InlineGitConnect />}
          </div>

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
                <h4 className="text-base font-semibold">
                  Deploy from your machine
                </h4>
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

        {/* Migrate from another platform — one-click tiles for the platforms
            people most often arrive from; each opens the import wizard with the
            source preselected. The wizard migrates the whole setup: apps,
            databases (with data), domains, and env vars. */}
        <div className="mx-auto mt-6 max-w-5xl rounded-xl border bg-background p-5 sm:p-6">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-center gap-3">
              <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-muted">
                <Upload className="h-5 w-5 text-muted-foreground" />
              </div>
              <div className="min-w-0">
                <h3 className="text-base font-semibold">
                  Already running somewhere else?
                </h3>
                <p className="text-xs text-muted-foreground">
                  Bring your apps, databases, domains, and env vars over in one
                  guided import
                </p>
              </div>
            </div>
            <Button asChild variant="outline" size="sm" className="shrink-0">
              <Link
                to="/projects/import-wizard"
                className="flex items-center gap-1.5"
              >
                See all platforms
                <ArrowRight className="h-3.5 w-3.5" />
              </Link>
            </Button>
          </div>

          <div className="mt-4 grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
            {TOP_MIGRATION_SOURCES.map((platform) => (
              <MigrationSourceTile key={platform.source} {...platform} />
            ))}
          </div>
        </div>

        <div className="mx-auto mt-6 flex max-w-5xl flex-col items-center justify-center gap-x-6 gap-y-2 border-t pt-5 text-sm sm:flex-row">
          <Button
            asChild
            variant="link"
            className="h-auto p-0 text-muted-foreground"
          >
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
      </section>
    </div>
  )
}

function MigrationSourceTile({
  source,
  label,
}: {
  source: string
  label: string
}) {
  return (
    <Link
      to={importHref(source)}
      className={cn(
        'group flex items-center gap-2.5 rounded-lg border bg-card p-3 text-left transition-colors',
        'hover:border-primary/50 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
      )}
    >
      <SourceLogo source={source} className="h-5 w-5 shrink-0" />
      <span className="min-w-0 flex-1 truncate text-sm font-medium">
        {label}
      </span>
      <ArrowRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
    </Link>
  )
}

function ShowcaseCard({
  icon: Icon,
  name,
  blurb,
  href,
}: {
  icon: React.ComponentType<{ className?: string }>
  name: string
  blurb: string
  href: string
}) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className={cn(
        'group flex flex-col rounded-xl border bg-background p-4 text-left transition-colors',
        'hover:border-primary/40 hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
      )}
    >
      <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <Icon className="h-4 w-4" />
      </div>
      <p className="mt-3 text-sm font-semibold">{name}</p>
      <p className="mt-1 flex-1 text-xs leading-relaxed text-muted-foreground">
        {blurb}
      </p>
      <span className="mt-3 inline-flex items-center gap-0.5 text-xs font-medium text-primary opacity-0 transition-opacity group-hover:opacity-100">
        Learn how
        <ArrowRight className="h-3 w-3" />
      </span>
    </a>
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

function CliCommand({ command }: { command: string }) {
  const tokens = command.split(' ')
  const packageIndex = tokens.findIndex((t) => t.startsWith('@'))

  const colorFor = (token: string, i: number): string => {
    if (/^https?:\/\//.test(token)) return 'text-amber-600 dark:text-amber-400'
    if (i === 0) return 'text-emerald-600 dark:text-emerald-400'
    if (i === packageIndex) return 'text-foreground font-medium'
    if (packageIndex !== -1 && i === packageIndex + 1)
      return 'text-sky-600 dark:text-sky-400'
    return 'text-muted-foreground'
  }

  return (
    <div
      className={cn(
        'flex items-center gap-2 rounded-md border bg-muted/50 pl-3 pr-1 py-2',
        'font-mono text-xs sm:text-sm'
      )}
    >
      <span className="shrink-0 select-none text-muted-foreground">$</span>
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
