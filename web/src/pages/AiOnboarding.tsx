// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
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
  deleteApiKeyMutation,
  getAiHarnessStatusOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { CopyButton } from '@/components/ui/copy-button'
import { HighlightedCode } from '@/components/ui/code-block'
import { ScrollRegion } from '@/components/ui/scroll-region'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useManualHarnessCompletion } from '@/hooks/useManualHarnessCompletion'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  AI_HARNESS_KEY_NAME,
  buildAiHarnessCommands,
  buildAiHarnessKeyRequest,
  canRemovePendingKey,
  type CreatedHarnessKey,
  describeHarnessConnection,
  onboardingPollInterval,
  reconcileCreatedKey,
  resolveOnboardingStatus,
} from '@/lib/ai-onboarding'
import { invalidateApiKeyQueries } from '@/lib/api-key-queries'
import { problemDetail } from '@/lib/api-problem'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'
import { cn } from '@/lib/utils'
import { toast } from 'sonner'

const STARTER_PROMPTS = [
  {
    icon: Bot,
    title: 'Verify access',
    prompt:
      'Use the Temps skill to verify my connection and list the services (Temps projects) on this instance. Do not make any changes.',
  },
  {
    icon: FolderPlus,
    title: 'Create a service',
    prompt:
      'Use the Temps skill to create a manual project (a single deployable service) named api-playground. Explain the target and ask for confirmation before creating it, then verify it appears in the project list.',
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
  const [createdKey, setCreatedKey] = useState<CreatedHarnessKey | null>(null)
  const createdKeySecret = createdKey?.secret ?? null
  const origin = typeof window === 'undefined' ? '' : window.location.origin
  const commands = buildAiHarnessCommands(origin)

  const queryClient = useQueryClient()
  // The key the user asked to remove; the dialog stays open only while the
  // server still reports that same key as waiting.
  const [removalKeyId, setRemovalKeyId] = useState<number | null>(null)
  const { markCompleted: markHarnessCompleted } = useManualHarnessCompletion()
  // The creation fallback applies only until the server reports the key.
  const keyJustCreated = createdKey !== null && !createdKey.acknowledged

  const {
    data: harness,
    isLoading: harnessLoading,
    isError: harnessReadFailed,
    error: harnessReadError,
    isFetching: harnessFetching,
    refetch: refetchHarness,
  } = useQuery({
    ...getAiHarnessStatusOptions(),
    retry: false,
    // Faster than the app-wide banner: this page is where the user waits.
    // Polls on what the page shows, so a failed refetch right after creating
    // the key does not leave it showing waiting with polling stopped.
    refetchInterval: (query) =>
      onboardingPollInterval(query.state.data, { keyJustCreated }),
  })
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const createHarnessKey = useMutation({
    ...createApiKeyMutation(),
    meta: {
      errorTitle: 'Failed to create AI harness key',
    },
    onSuccess: async (response) => {
      setCreatedKey({
        secret: response.api_key,
        id: response.id,
        acknowledged: false,
      })
      await refetchHarness()
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

  // The way out when the harness connected with a credential the server
  // cannot attribute to it (an older key, a renamed one): remove the unused
  // admin key, which is audit-logged, and mark the setup step done.
  const removePendingKey = useMutation({
    ...deleteApiKeyMutation(),
    meta: {
      errorTitle: 'Failed to remove the pending AI harness key',
    },
    onSuccess: async () => {
      setCreatedKey(null)
      setRemovalKeyId(null)
      try {
        markHarnessCompleted()
      } catch (error) {
        toast.error(
          error instanceof Error
            ? error.message
            : 'Could not mark the harness step as done'
        )
      }
      await invalidateApiKeyQueries(queryClient)
      toast.success('Pending harness key removed')
    },
    onError: (error) => {
      toast.error(
        sensitiveActionErrorMessage(
          error,
          'Failed to remove the pending AI harness key'
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

  const reconciledKey = reconcileCreatedKey(createdKey, harness)
  if (reconciledKey !== createdKey) setCreatedKey(reconciledKey)

  const harnessKeyId = harness?.key_id ?? null
  // `unknown` (not `missing`) while the status cannot be read: creation is
  // offered only after the server says no key exists.
  const effectiveHarnessStatus = resolveOnboardingStatus(harness, {
    keyJustCreated,
  })
  const statusUnknown = effectiveHarnessStatus === 'unknown'
  // Removal targets a key the server reports as waiting, whether or not this
  // visit created it.
  const pendingKeyId = harness?.status === 'waiting' ? harnessKeyId : null
  const removalAllowed = canRemovePendingKey(harness, removalKeyId)
  if (removalKeyId !== null && !removalAllowed && !removePendingKey.isPending) {
    // The key connected (or changed) while the confirmation was open: drop
    // the request instead of revoking the credential that just connected.
    setRemovalKeyId(null)
  }
  // A dedicated key exists but cannot connect (deactivated, or expired
  // unused). Creating another would conflict on its name, so link to it.
  const harnessKeyUnusable =
    effectiveHarnessStatus === 'missing' && harnessKeyId !== null
  const connectedVia = describeHarnessConnection(harness?.connected_via)

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
                  Your harness can inspect services, create databases, deploy
                  applications, and verify the result using the same pinned
                  Temps CLI workflow.
                </p>
              </div>
            </div>
            {harnessLoading ? (
              <Skeleton className="h-10 w-44" />
            ) : effectiveHarnessStatus === 'connected' ? (
              <div className="flex flex-col items-start gap-1.5 lg:items-end">
                <div className="inline-flex items-center gap-2 rounded-lg border border-emerald-500/30 bg-emerald-500/5 px-4 py-2.5 text-sm font-medium text-emerald-700 dark:text-emerald-400">
                  <Check className="size-4" />
                  Harness connected
                </div>
                {connectedVia && (
                  <p className="text-xs text-muted-foreground">
                    {connectedVia}
                  </p>
                )}
              </div>
            ) : harnessKeyUnusable && harness?.setup_path ? (
              <Button asChild size="lg" variant="outline">
                <Link to={harness.setup_path}>
                  Manage harness key
                  <ArrowRight className="ml-1.5 size-4" />
                </Link>
              </Button>
            ) : statusUnknown ? (
              <Button
                size="lg"
                variant="outline"
                onClick={() => void refetchHarness()}
                disabled={harnessFetching}
              >
                {harnessFetching ? (
                  <Loader2 className="mr-1.5 size-4 animate-spin" />
                ) : null}
                Retry status check
              </Button>
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
            <p>
              Install the skill, run the connect command and paste the key at
              its prompt, then run either read-only verification command below.
              This page detects the first authenticated request automatically,
              including a CLI login you approve in the browser.
            </p>
          </AlertDescription>
        </Alert>
      )}

      {pendingKeyId !== null && (
        <div className="flex flex-col gap-2 rounded-lg border bg-muted/30 px-4 py-3 text-sm sm:flex-row sm:items-center">
          <span>Connected with a different credential?</span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setRemovalKeyId(pendingKeyId)}
            disabled={removePendingKey.isPending}
          >
            {removePendingKey.isPending ? (
              <Loader2 className="mr-1.5 size-3.5 animate-spin" />
            ) : null}
            Remove pending key
          </Button>
        </div>
      )}

      {statusUnknown && harnessReadFailed && (
        <Alert variant="destructive">
          <ShieldAlert className="size-4" />
          <AlertTitle>Couldn&apos;t check the harness connection</AlertTitle>
          <AlertDescription>
            {problemDetail(
              harnessReadError,
              'The status request failed. Retry, or check the key in Settings → API keys.'
            )}
          </AlertDescription>
        </Alert>
      )}

      {harnessKeyUnusable && harness?.reason && (
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <ShieldAlert className="size-4 text-amber-700 dark:text-amber-300" />
          <AlertTitle>The harness key cannot connect</AlertTitle>
          <AlertDescription>{harness.reason}</AlertDescription>
        </Alert>
      )}

      <div className="grid gap-4">
        <OnboardingStep
          number="01"
          icon={KeyRound}
          title="Create a dedicated admin key"
          description="The harness needs platform access to create and verify services, databases, deployments, domains, and other resources."
          ready={
            (harnessKeyId !== null && !harnessKeyUnusable) || !!createdKeySecret
          }
          loading={harnessLoading}
          status={
            statusUnknown
              ? 'Unknown'
              : harnessKeyUnusable
                ? 'Needs attention'
                : harnessKeyId !== null || createdKeySecret
                  ? 'Key ready'
                  : 'Required'
          }
        >
          <div className="space-y-3">
            <Alert className="border-amber-500/25 bg-amber-500/5">
              <ShieldAlert className="size-4 text-amber-700 dark:text-amber-400" />
              <AlertTitle>Full platform access</AlertTitle>
              <AlertDescription>
                This admin key can change the entire instance. Use a dedicated
                key, keep it only in your harness credential store, and revoke
                it when you stop using the integration.
              </AlertDescription>
            </Alert>
            {createdKeySecret ? (
              <OneTimeKeySecret value={createdKeySecret} />
            ) : statusUnknown ? (
              <Button
                variant="outline"
                onClick={() => void refetchHarness()}
                disabled={harnessFetching}
              >
                Retry status check
              </Button>
            ) : harnessKeyId !== null ? (
              <Button asChild variant="outline">
                <Link to={`/settings/keys/${harnessKeyId}`}>
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
          description="Run the skill installer inside the repository where you use your AI agent, then run the connect command and paste the key from step one when it asks."
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
              Works in bash and zsh. The key is read without echo and cleared
              from the shell after login.
            </p>
          </div>
        </OnboardingStep>

        <OnboardingStep
          number="03"
          icon={Terminal}
          title="Verify before changing anything"
          description="Confirm the harness is targeting this instance and can read its services. Neither verification command changes platform state."
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
              label="Verify service access"
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
        <div className="grid gap-3 lg:grid-cols-3">
          {STARTER_PROMPTS.map((example) => (
            <Card key={example.title} className="h-full overflow-hidden">
              <CardContent className="flex h-full flex-col p-0">
                <div className="flex items-center gap-3 border-b px-4 py-3">
                  <div className="flex size-8 items-center justify-center rounded-lg border bg-muted/50 text-muted-foreground">
                    <example.icon className="size-3.5" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="text-[10px] font-medium uppercase tracking-[0.16em] text-muted-foreground">
                      Prompt for your agent
                    </p>
                    <h3 className="mt-0.5 truncate text-sm font-semibold">
                      {example.title}
                    </h3>
                  </div>
                </div>

                <div className="flex flex-1 p-3">
                  <div className="relative flex min-h-36 w-full flex-col rounded-xl border bg-muted/25 p-4 pr-14 shadow-inner">
                    <p className="text-sm leading-6 text-foreground">
                      {example.prompt}
                    </p>
                    <CopyButton
                      value={example.prompt}
                      className="absolute right-3 top-3 size-9 rounded-lg border bg-background text-muted-foreground shadow-sm hover:text-foreground"
                      label={`Copy ${example.title} prompt`}
                    />
                    <div className="mt-auto flex items-center gap-1.5 pt-5 text-[11px] text-muted-foreground">
                      <Terminal className="size-3" />
                      Paste into Claude Code, Codex, Cursor, or your harness
                    </div>
                  </div>
                </div>
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
      <ConfirmDeleteDialog
        open={removalAllowed}
        onOpenChange={(open) => {
          if (!open) setRemovalKeyId(null)
        }}
        resourceType="API key"
        resourceName={AI_HARNESS_KEY_NAME}
        verb="Remove"
        consequences="The unused admin key stops working and this setup step is marked done. A harness already connected with another credential keeps working. The removal is recorded in the audit log."
        isPending={removePendingKey.isPending}
        onConfirm={() => {
          // Re-check at the moment of confirmation, not when the dialog opened.
          if (
            removalKeyId !== null &&
            canRemovePendingKey(harness, removalKeyId)
          ) {
            removePendingKey.mutate({ path: { id: removalKeyId } })
          }
        }}
      />
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
                    ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-500'
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
                    ? 'border-emerald-500/30 bg-emerald-500/5 text-emerald-700 dark:text-emerald-400'
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
          tabIndex={0}
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
        <ScrollRegion
          className="min-w-0 flex-1"
          regionLabel="AI harness API key, scrollable"
        >
          <code className="block w-max select-all whitespace-nowrap font-mono text-xs">
            {value}
          </code>
        </ScrollRegion>
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
