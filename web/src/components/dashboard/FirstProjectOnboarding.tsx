import { Link, useNavigate } from 'react-router'
import {
  Activity,
  ArrowRight,
  BarChart3,
  BookOpen,
  Bug,
  Database,
  FileArchive,
  FolderOpen,
  GitBranch,
  Mail,
  Network,
  Play,
  ScrollText,
  Terminal,
  UploadCloud,
} from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { ConnectionList } from '@/components/dashboard/ConnectionList'
import { InlineGitConnect } from '@/components/dashboard/InlineGitConnect'
import { cn } from '@/lib/utils'
import { filesFromDrop, filesFromInput } from '@/lib/drop-files'
import { handOffDropFiles } from '@/lib/drop-handoff'

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
    name: 'Analytics',
    blurb:
      'Visitors, pages, funnels, and a live globe — no third-party scripts.',
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
    name: 'Databases',
    blurb: 'Postgres, Redis, MongoDB, and S3-compatible storage.',
    href: 'https://temps.sh/docs/databases',
  },
  {
    icon: Mail,
    name: 'Email',
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
      {/* Hero. Deliberately one band, not a screen: the demo CTA and the list
          of what lights up carry the pitch, and the ways to get an app on
          Temps sit directly below without scrolling. Flat bg-card, same as
          every other panel on the page — no gradient wash. */}
      <div className="rounded-2xl border border-primary/20 bg-card p-5 sm:p-6">
        {/* Title and CTA share one line; the capability chips wrap onto the
            next. The badge, the sub-paragraph and the payoff glyph are gone —
            each was another 40-130px of height for something the headline and
            the chips already say. */}
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <h2 className="text-base font-semibold tracking-tight text-balance sm:text-lg">
            Deploy one app. Watch your whole stack light up.
          </h2>
          <Button asChild size="sm" className="group shrink-0">
            <Link to={DEMO_TEMPLATE_HREF}>
              Deploy the demo app
              <ArrowRight className="ml-1.5 h-4 w-4 transition-transform group-hover:translate-x-0.5" />
            </Link>
          </Button>
        </div>

        <div className="mt-3 flex flex-wrap items-center gap-2">
          <span className="mr-0.5 text-xs text-muted-foreground">
            Pre-wired with
          </span>
          {SHOWCASE.map((f) => (
            <ShowcaseChip key={f.name} {...f} />
          ))}
        </div>
      </div>

      {/* Deploy your own — Git, CLI, or browser upload. */}
      <section
        id="deploy-your-own"
        className="rounded-2xl border bg-card p-5 sm:p-6"
      >
        <div className="mb-4 flex flex-col gap-1 sm:flex-row sm:items-baseline sm:justify-between">
          <h3 className="text-lg font-semibold tracking-tight">
            Get your own app on Temps
          </h3>
          <a
            href="https://temps.sh/docs"
            target="_blank"
            rel="noreferrer"
            className="flex shrink-0 items-center gap-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground"
          >
            <BookOpen className="h-3.5 w-3.5" />
            Read the deployment docs
          </a>
        </div>

        {/* Three peer paths. Migrating from another platform has
            its own entry point in the page header above, so it isn't repeated
            here as a third card. */}
        <div className="grid gap-5 lg:grid-cols-3">
          {/* Path A — Deploy from Git */}
          <div className="flex flex-col rounded-xl border bg-background p-4 text-left sm:p-5">
            <div className="flex items-center gap-2.5">
              <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-primary/10">
                <GitBranch className="h-4 w-4 text-primary" />
              </div>
              <div className="min-w-0">
                <h4 className="text-sm font-semibold">Deploy from Git</h4>
                <p className="text-xs text-muted-foreground">
                  Git-push deploys with automatic builds
                </p>
              </div>
            </div>
            {gitConnected ? <ConnectionList /> : <InlineGitConnect />}
          </div>

          {/* Path B — Deploy from your machine (CLI) */}
          <div className="flex flex-col rounded-xl border bg-background p-4 text-left sm:p-5">
            <div className="flex items-center gap-2.5">
              <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-muted">
                <Terminal className="h-4 w-4 text-muted-foreground" />
              </div>
              <div className="min-w-0">
                <h4 className="text-sm font-semibold">
                  Deploy from your machine
                </h4>
                <p className="text-xs text-muted-foreground">
                  No Git provider required
                </p>
              </div>
            </div>

            <div className="mt-3 space-y-2">
              {cliCommands.map((cmd) => (
                <CliCommand key={cmd} command={cmd} />
              ))}
            </div>

            <ol className="mt-3 flex-1 space-y-2">
              {CLI_STEPS.map((step, i) => (
                <Step key={step} index={i + 1} label={step} />
              ))}
            </ol>
          </div>

          <EmptyStateDropCard />
        </div>
      </section>
    </div>
  )
}

function EmptyStateDropCard() {
  const navigate = useNavigate()
  const folderInputRef = useRef<HTMLInputElement>(null)
  const zipInputRef = useRef<HTMLInputElement>(null)
  const [isDragging, setIsDragging] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    folderInputRef.current?.setAttribute('webkitdirectory', '')
  }, [])

  const continueToDrop = (files: ReturnType<typeof filesFromInput>) => {
    if (files.length === 0) return
    handOffDropFiles(files)
    navigate('/drop')
  }

  return (
    <div
      className={cn(
        'group flex flex-col rounded-xl border bg-background p-4 text-left transition-colors sm:p-5',
        isDragging && 'border-primary bg-primary/5'
      )}
      onDragEnter={(event) => {
        event.preventDefault()
        setIsDragging(true)
      }}
      onDragOver={(event) => event.preventDefault()}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node)) {
          setIsDragging(false)
        }
      }}
      onDrop={async (event) => {
        event.preventDefault()
        setIsDragging(false)
        setError(null)
        try {
          const files = await filesFromDrop(event)
          if (files.length === 0)
            throw new Error('Choose a project folder or ZIP')
          handOffDropFiles(files)
          navigate('/drop')
        } catch (caught) {
          setError(
            caught instanceof Error
              ? caught.message
              : 'Those project files could not be read'
          )
        }
      }}
    >
      <div className="flex items-center gap-2.5">
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-muted transition-colors group-hover:bg-primary/10">
          <UploadCloud className="h-4 w-4 text-muted-foreground group-hover:text-primary" />
        </div>
        <div className="min-w-0">
          <h4 className="text-sm font-semibold">Drop project files</h4>
          <p className="text-xs text-muted-foreground">
            Package locally, then detect on this Temps instance
          </p>
        </div>
      </div>

      <div className="mt-4 flex flex-1 flex-col justify-center rounded-lg border border-dashed border-border/80 bg-muted/20 p-4 text-center transition-colors group-hover:border-muted-foreground/50">
        <p className="text-sm font-medium">
          {isDragging
            ? 'Release to inspect your project'
            : 'Drop a folder or ZIP here'}
        </p>
        <p className="mt-1 text-xs text-muted-foreground">
          Common secret files are excluded before the archive is uploaded for
          preset detection.
        </p>
        <div className="mt-4 grid gap-2 sm:grid-cols-2">
          <Button
            type="button"
            size="sm"
            onClick={() => folderInputRef.current?.click()}
          >
            <FolderOpen className="size-4" /> Open folder
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => zipInputRef.current?.click()}
          >
            <FileArchive className="size-4" /> Upload ZIP
          </Button>
        </div>
      </div>

      {error && <p className="mt-3 text-xs text-destructive">{error}</p>}
      <Button asChild variant="ghost" size="sm" className="mt-2">
        <Link to="/drop">Open Drop without files</Link>
      </Button>

      <input
        ref={folderInputRef}
        type="file"
        multiple
        className="hidden"
        onChange={(event) => continueToDrop(filesFromInput(event.target.files))}
      />
      <input
        ref={zipInputRef}
        type="file"
        accept=".zip,application/zip"
        className="hidden"
        onChange={(event) => continueToDrop(filesFromInput(event.target.files))}
      />
    </div>
  )
}

/**
 * One capability, as a compact chip. Replaces the previous card-with-blurb:
 * eight of those cost a full screen to communicate "you get all of this",
 * which the names alone already do. The link still goes to the docs page that
 * shows how to add it to your own app.
 */
function ShowcaseChip({
  icon: Icon,
  name,
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
        'inline-flex items-center gap-1.5 rounded-full border bg-background/70 px-2.5 py-1 text-xs font-medium transition-colors',
        'hover:border-primary/40 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
      )}
    >
      <Icon className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
      {name}
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
