import { useEffect, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  ArrowRight,
  Bot,
  Check,
  Circle,
  Database,
  ExternalLink,
  FolderPlus,
  KeyRound,
  Loader2,
  ShieldAlert,
  Sparkles,
  Terminal,
  Wand2,
  type LucideIcon,
} from 'lucide-react'
import {
  createApiKeyMutation,
  listApiKeysOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { HighlightedCode } from '@/components/ui/code-block'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  buildAiHarnessCommands,
  buildAiHarnessKeyRequest,
  findAiHarnessKey,
  getAiHarnessStatus,
} from '@/lib/ai-onboarding'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'
import { cn } from '@/lib/utils'
import { toast } from 'sonner'

const STARTER_PROMPTS = [
  {
    icon: Bot,
    title: 'Verify access',
    prompt:
      'Use the Temps skill to verify my connection and list the projects on this instance. Do not make any changes.',
  },
  {
    icon: FolderPlus,
    title: 'Create a project',
    prompt:
      'Use the Temps skill to create a manual project named api-playground. Explain the target and ask for confirmation before creating it, then verify it appears in the project list.',
  },
  {
    icon: Database,
    title: 'Create PostgreSQL',
    prompt:
      'Use the Temps skill to create a PostgreSQL service named app-db. Explain the target and ask for confirmation before creating it, then verify the service is running.',
  },
] as const

export function AiOnboarding() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [createdKeySecret, setCreatedKeySecret] = useState<string | null>(null)
  const origin = typeof window === 'undefined' ? '' : window.location.origin
  const commands = buildAiHarnessCommands(origin)

  const {
    data: apiKeysData,
    isLoading: apiKeysLoading,
    refetch: refetchApiKeys,
  } = useQuery({
    ...listApiKeysOptions({ query: { page: 1, page_size: 100 } }),
    retry: false,
    refetchInterval: (query) =>
      getAiHarnessStatus(query.state.data?.api_keys) === 'waiting'
        ? 5_000
        : false,
  })
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const createHarnessKey = useMutation({
    ...createApiKeyMutation(),
    meta: {
      errorTitle: 'Failed to create AI harness key',
    },
    onSuccess: async (response) => {
      setCreatedKeySecret(response.api_key)
      await refetchApiKeys()
      toast.success('AI harness key created')
    },
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () =>
          createHarnessKey.mutate(variables)
        )
      ) {
        return
      }
      toast.error(
        sensitiveActionErrorMessage(
          error,
          'Failed to create the AI harness key'
        )
      )
    },
  })

  usePageTitle('Connect AI harness')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Finish setting up', href: '/setup' },
      { label: 'Connect AI harness' },
    ])
  }, [setBreadcrumbs])

  const harnessKey = findAiHarnessKey(apiKeysData?.api_keys)
  const harnessStatus = getAiHarnessStatus(apiKeysData?.api_keys)
  const effectiveHarnessStatus =
    createdKeySecret && harnessStatus === 'missing' ? 'waiting' : harnessStatus

  const handleCreateHarnessKey = () => {
    createHarnessKey.mutate({ body: buildAiHarnessKeyRequest() })
  }

  return (
    <div className="w-full space-y-6 p-4 sm:p-8">
      <div className="flex items-center gap-3">
        <Button asChild variant="ghost" size="icon" className="shrink-0">
          <Link to="/setup" aria-label="Back to setup">
            <ArrowLeft className="size-4" />
          </Link>
        </Button>
        <div>
          <p className="text-xs font-medium uppercase tracking-[0.18em] text-primary">
            AI harness quickstart
          </p>
          <h1 className="mt-1 text-2xl font-semibold tracking-tight sm:text-3xl">
            Let your AI operate Temps
          </h1>
          <p className="mt-1 max-w-3xl text-sm text-muted-foreground sm:text-base">
            Give Codex, Claude Code, Cursor, or another compatible harness the
            Temps skill and a dedicated credential for this instance.
          </p>
        </div>
      </div>

      <Card className="overflow-hidden border-primary/20">
        <CardContent className="relative p-0">
          <div
            className="absolute inset-0 opacity-80"
            aria-hidden="true"
            style={{
              backgroundImage:
                'radial-gradient(80% 120% at 0% 0%, color-mix(in oklch, var(--primary) 12%, transparent) 0%, transparent 62%)',
            }}
          />
          <div className="relative grid gap-6 p-5 sm:p-7 lg:grid-cols-[1fr_auto] lg:items-center">
            <div className="flex items-start gap-4">
              <div className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-primary text-primary-foreground shadow-sm">
                <Sparkles className="size-5" />
              </div>
              <div>
                <h2 className="text-lg font-semibold tracking-tight">
                  One connection, every platform workflow
                </h2>
                <p className="mt-1 max-w-2xl text-sm leading-relaxed text-muted-foreground">
                  Your harness can inspect projects, create services, deploy
                  applications, and verify the result using the same pinned
                  Temps CLI workflow.
                </p>
              </div>
            </div>
            {apiKeysLoading ? (
              <Skeleton className="h-10 w-44" />
            ) : effectiveHarnessStatus === 'connected' ? (
              <div className="inline-flex items-center gap-2 rounded-lg border border-emerald-500/30 bg-emerald-500/5 px-4 py-2.5 text-sm font-medium text-emerald-600 dark:text-emerald-400">
                <Check className="size-4" />
                Harness connected
              </div>
            ) : effectiveHarnessStatus === 'waiting' ? (
              <div className="inline-flex items-center gap-2 rounded-lg border border-amber-500/30 bg-amber-500/5 px-4 py-2.5 text-sm font-medium text-amber-700 dark:text-amber-300">
                <Circle className="size-3 fill-current" />
                Waiting for first request
              </div>
            ) : (
              <Button
                size="lg"
                onClick={handleCreateHarnessKey}
                disabled={createHarnessKey.isPending}
              >
                {createHarnessKey.isPending ? (
                  <Loader2 className="mr-1.5 size-4 animate-spin" />
                ) : null}
                {createHarnessKey.isPending
                  ? 'Creating harness key…'
                  : 'Create harness API key'}
                {!createHarnessKey.isPending && (
                  <ArrowRight className="ml-1.5 size-4" />
                )}
              </Button>
            )}
          </div>
        </CardContent>
      </Card>

      {effectiveHarnessStatus === 'waiting' && !createdKeySecret && (
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <Terminal className="size-4 text-amber-700 dark:text-amber-300" />
          <AlertTitle>API key created — finish the connection</AlertTitle>
          <AlertDescription>
            Install the skill, authenticate the CLI, and run either read-only
            verification command below. This page checks for the key&apos;s
            first authenticated request automatically.
          </AlertDescription>
        </Alert>
      )}

      <div className="grid gap-4">
        <OnboardingStep
          number="01"
          icon={KeyRound}
          title="Create a dedicated admin key"
          description="The harness needs platform access to create and verify projects, databases, deployments, domains, and other resources."
          ready={!!harnessKey || !!createdKeySecret}
          loading={apiKeysLoading}
          status={harnessKey || createdKeySecret ? 'Key ready' : 'Required'}
        >
          <div className="space-y-3">
            <Alert className="border-amber-500/25 bg-amber-500/5">
              <ShieldAlert className="size-4 text-amber-600 dark:text-amber-400" />
              <AlertTitle>Full platform access</AlertTitle>
              <AlertDescription>
                This admin key can change the entire instance. Use a dedicated
                key, keep it only in your harness credential store, and revoke
                it when you stop using the integration.
              </AlertDescription>
            </Alert>
            {createdKeySecret ? (
              <OneTimeKeySecret value={createdKeySecret} />
            ) : harnessKey ? (
              <Button asChild variant="outline">
                <Link to={`/settings/keys/${harnessKey.id}`}>
                  Manage harness key
                  <ArrowRight className="ml-1.5 size-3.5" />
                </Link>
              </Button>
            ) : (
              <Button
                onClick={handleCreateHarnessKey}
                disabled={createHarnessKey.isPending}
              >
                {createHarnessKey.isPending ? (
                  <Loader2 className="mr-1.5 size-3.5 animate-spin" />
                ) : null}
                {createHarnessKey.isPending
                  ? 'Creating admin key…'
                  : 'Create admin key'}
                {!createHarnessKey.isPending && (
                  <ArrowRight className="ml-1.5 size-3.5" />
                )}
              </Button>
            )}
          </div>
        </OnboardingStep>

        <OnboardingStep
          number="02"
          icon={Wand2}
          title="Add Temps to your harness"
          description="Run the skill installer inside the repository where you use your AI agent, then authenticate the pinned CLI with the key from step one."
          ready={effectiveHarnessStatus === 'connected'}
          status={
            effectiveHarnessStatus === 'connected'
              ? 'Authenticated'
              : 'Two commands'
          }
        >
          <div className="space-y-3">
            <Command
              label="Install the Temps skill"
              value={commands.installSkill}
            />
            <Command
              label="Store this instance as a CLI context"
              value={commands.connectCli}
              multiline
            />
            <p className="text-xs leading-relaxed text-muted-foreground">
              The key is entered through a hidden terminal prompt and removed
              from the shell environment after login.
            </p>
          </div>
        </OnboardingStep>

        <OnboardingStep
          number="03"
          icon={Terminal}
          title="Verify before changing anything"
          description="Confirm the harness is targeting this instance and can read its projects. Neither verification command changes platform state."
          ready={effectiveHarnessStatus === 'connected'}
          status={
            effectiveHarnessStatus === 'connected' ? 'Verified' : 'Read-only'
          }
        >
          <div className="space-y-3">
            <Command
              label="Verify identity and context"
              value={commands.verifyIdentity}
            />
            <Command
              label="Verify project access"
              value={commands.verifyProjects}
            />
          </div>
        </OnboardingStep>
      </div>

      <section className="space-y-3">
        <div>
          <h2 className="text-base font-semibold tracking-tight">
            Try a real task in your harness
          </h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Copy a prompt after verification. Write operations explicitly ask
            for confirmation and finish with read-only proof.
          </p>
        </div>
        <div className="grid gap-3 md:grid-cols-3">
          {STARTER_PROMPTS.map((example) => (
            <Card key={example.title} className="group h-full">
              <CardContent className="flex h-full flex-col p-4">
                <div className="flex items-center justify-between gap-3">
                  <div className="flex size-8 items-center justify-center rounded-lg bg-muted text-muted-foreground">
                    <example.icon className="size-4" />
                  </div>
                  <CopyButton
                    value={example.prompt}
                    minimal
                    className="size-8 rounded-md text-muted-foreground"
                    label={`Copy ${example.title} prompt`}
                  />
                </div>
                <h3 className="mt-4 text-sm font-semibold">{example.title}</h3>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {example.prompt}
                </p>
              </CardContent>
            </Card>
          ))}
        </div>
      </section>

      <div className="grid gap-3 sm:grid-cols-2">
        <Link
          to="/ai-gateway"
          className="group flex items-center justify-between gap-4 rounded-xl border bg-card p-4 transition-colors hover:border-primary/30 hover:bg-muted/30"
        >
          <div>
            <p className="text-sm font-medium">Enable Temps built-in AI</p>
            <p className="mt-0.5 text-xs text-muted-foreground">
              Separately configure a provider for console chat, autofix, and AI
              summaries.
            </p>
          </div>
          <ArrowRight className="size-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5" />
        </Link>
        <a
          href="https://temps.sh/docs/features/skills"
          target="_blank"
          rel="noreferrer"
          className="group flex items-center justify-between gap-4 rounded-xl border bg-card p-4 transition-colors hover:border-primary/30 hover:bg-muted/30"
        >
          <div>
            <p className="text-sm font-medium">
              Browse the Temps skill catalog
            </p>
            <p className="mt-0.5 text-xs text-muted-foreground">
              Add focused skills for analytics, error tracking, SDKs, and
              platform setup.
            </p>
          </div>
          <ExternalLink className="size-4 shrink-0 text-muted-foreground" />
        </a>
      </div>

      <div className="flex flex-col gap-3 rounded-xl border bg-muted/30 p-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <p className="text-sm font-medium">
            Need the rest of platform setup?
          </p>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Domains, notifications, backups, and team access remain in the
            complete setup checklist.
          </p>
        </div>
        <Button asChild variant="outline">
          <Link to="/setup">Back to all setup steps</Link>
        </Button>
      </div>
      {verificationDialog}
    </div>
  )
}

function OnboardingStep({
  number,
  icon: Icon,
  title,
  description,
  ready = false,
  loading = false,
  status,
  children,
}: {
  number: string
  icon: LucideIcon
  title: string
  description: string
  ready?: boolean
  loading?: boolean
  status: string
  children: ReactNode
}) {
  return (
    <Card className="w-full overflow-hidden">
      <CardContent className="grid gap-6 p-5 sm:p-6 lg:grid-cols-[minmax(18rem,0.8fr)_minmax(0,1.2fr)] lg:gap-10">
        <div>
          <div className="flex items-start justify-between gap-3">
            <div className="flex items-center gap-3">
              <div
                className={cn(
                  'flex size-9 items-center justify-center rounded-lg border',
                  ready
                    ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-500'
                    : 'bg-muted text-muted-foreground'
                )}
              >
                {ready ? (
                  <Check className="size-4" />
                ) : (
                  <Icon className="size-4" />
                )}
              </div>
              <span className="font-mono text-[11px] text-muted-foreground">
                {number}
              </span>
            </div>
            {loading ? (
              <Skeleton className="h-5 w-20" />
            ) : (
              <span
                className={cn(
                  'inline-flex items-center gap-1.5 rounded-full border px-2 py-1 text-[10px] font-medium uppercase tracking-wide',
                  ready
                    ? 'border-emerald-500/30 bg-emerald-500/5 text-emerald-600 dark:text-emerald-400'
                    : 'text-muted-foreground'
                )}
              >
                {ready ? (
                  <Check className="size-3" />
                ) : (
                  <Circle className="size-2.5" />
                )}
                {status}
              </span>
            )}
          </div>
          <h2 className="mt-5 text-base font-semibold tracking-tight">
            {title}
          </h2>
          <p className="mt-1 max-w-xl text-sm leading-relaxed text-muted-foreground">
            {description}
          </p>
        </div>
        <div className="min-w-0 lg:border-l lg:pl-10">{children}</div>
      </CardContent>
    </Card>
  )
}

function Command({
  label,
  value,
  multiline = false,
}: {
  label: string
  value: string
  multiline?: boolean
}) {
  return (
    <div className="space-y-1.5">
      <p className="text-[11px] font-medium text-muted-foreground">{label}</p>
      <div className="flex min-w-0 items-start gap-2 rounded-lg border bg-background px-3 py-2.5">
        <pre
          className={cn(
            'm-0 min-w-0 flex-1 overflow-x-auto font-mono text-xs',
            multiline ? 'whitespace-pre leading-relaxed' : 'whitespace-nowrap'
          )}
        >
          <HighlightedCode code={value} language="bash" />
        </pre>
        <CopyButton
          value={value}
          minimal
          className="size-7 shrink-0 rounded-md"
          label={`Copy ${label.toLowerCase()}`}
        />
      </div>
    </div>
  )
}

function OneTimeKeySecret({ value }: { value: string }) {
  return (
    <div className="space-y-3 rounded-xl border border-emerald-500/30 bg-emerald-500/5 p-4">
      <div className="flex items-start gap-3">
        <Check className="mt-0.5 size-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
        <div>
          <p className="text-sm font-semibold">Copy this key now</p>
          <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">
            This secret is shown once. Store it in your harness credential store
            before leaving or refreshing this page.
          </p>
        </div>
      </div>
      <div className="flex min-w-0 items-center gap-2 rounded-lg border bg-background px-3 py-2.5">
        <code className="min-w-0 flex-1 select-all overflow-x-auto whitespace-nowrap font-mono text-xs">
          {value}
        </code>
        <CopyButton
          value={value}
          minimal
          className="size-8 shrink-0 rounded-md"
          label="Copy AI harness API key"
        />
      </div>
    </div>
  )
}
