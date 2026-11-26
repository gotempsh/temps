// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, ExternalLink, Loader2 } from 'lucide-react'
import { Callout, PageHeader, Status } from '@temps-sdk/ds'
import {
  createApiKeyMutation,
  deleteApiKeyMutation,
  getAiHarnessStatusOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
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
  type OnboardingHarnessStatus,
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
    title: 'Verify access',
    prompt:
      'Use the Temps skill to verify my connection and list the services (Temps projects) on this instance. Do not make any changes.',
  },
  {
    title: 'Create a service',
    prompt:
      'Use the Temps skill to create a manual project (a single deployable service) named api-playground. Explain the target and ask for confirmation before creating it, then verify it appears in the project list.',
  },
  {
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
      toast.success('Admin key created')
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
      toast.success('Pending key removed')
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

  usePageTitle('Connect an agent')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Connect' },
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

  const connected = effectiveHarnessStatus === 'connected'
  const keyReady =
    (harnessKeyId !== null && !harnessKeyUnusable) || !!createdKeySecret

  return (
    <div className="w-full min-w-0 space-y-6">
      <PageHeader
        title="Connect an agent"
        verdict={
          harnessLoading ? (
            <Skeleton className="h-6 w-32" />
          ) : (
            <ConnectionStatus
              status={effectiveHarnessStatus}
              keyUnusable={harnessKeyUnusable}
            />
          )
        }
        description={
          connected
            ? connectedVia
            : harnessKeyUnusable
              ? harness?.reason
              : effectiveHarnessStatus === 'waiting'
                ? pendingKeyId !== null
                  ? 'Updates on the agent’s first request. Connected with another credential? Remove the unused key.'
                  : 'Updates on the agent’s first request.'
                : undefined
        }
        actions={
          statusUnknown && !harnessLoading ? (
            <Button
              variant="outline"
              size="sm"
              onClick={() => void refetchHarness()}
              disabled={harnessFetching}
            >
              {harnessFetching ? (
                <Loader2 className="mr-1.5 size-3.5 animate-spin" />
              ) : null}
              Retry
            </Button>
          ) : pendingKeyId !== null ? (
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
          ) : undefined
        }
      />

      {statusUnknown && harnessReadFailed && (
        <Callout tone="error" title="Could not check the connection">
          <p>
            {problemDetail(
              harnessReadError,
              'The status request failed. Retry, or check the key in Settings → API keys.'
            )}
          </p>
        </Callout>
      )}

      <ol
        aria-label="Steps to connect an agent"
        className="divide-y rounded-lg border bg-card text-card-foreground"
      >
        <Step
          number={1}
          title="Create an admin key"
          caption="This key can change everything on this server. Keep it only in your agent’s credential store, and revoke it when you stop using it."
          done={keyReady}
          action={
            harnessLoading ? (
              <Skeleton className="h-9 w-36" />
            ) : createdKeySecret || statusUnknown ? null : harnessKeyId !==
              null ? (
              <Button asChild variant="outline" size="sm">
                <Link
                  to={
                    (harnessKeyUnusable && harness?.setup_path) ||
                    `/settings/keys/${harnessKeyId}`
                  }
                >
                  Manage key
                </Link>
              </Button>
            ) : (
              <Button
                size="sm"
                onClick={handleCreateHarnessKey}
                disabled={createHarnessKey.isPending}
              >
                {createHarnessKey.isPending ? (
                  <Loader2 className="mr-1.5 size-3.5 animate-spin" />
                ) : null}
                Create admin key
              </Button>
            )
          }
        >
          {createdKeySecret ? (
            <OneTimeKeySecret value={createdKeySecret} />
          ) : null}
        </Step>

        <Step
          number={2}
          title="Add Temps to your agent"
          caption="Run both in the repository where you use your agent. Paste the key when the second one asks."
          done={connected}
        >
          <Command
            label="Install the Temps skill"
            value={commands.installSkill}
          />
          <Command
            label="Save this server as a CLI context"
            value={commands.connectCli}
            multiline
          />
          <p className="text-xs text-muted-foreground">
            Works in bash and zsh. The key is read without echo and cleared from
            the shell after login.
          </p>
        </Step>

        <Step
          number={3}
          title="Check the connection"
          caption="Neither command changes anything."
          done={connected}
        >
          <Command
            label="Verify identity and context"
            value={commands.verifyIdentity}
          />
          <Command
            label="Verify service access"
            value={commands.verifyProjects}
          />
        </Step>
      </ol>

      <section className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-lg font-semibold">Prompts to try</h2>
          <a
            href="https://temps.sh/docs/features/skills"
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-1 text-sm text-primary"
          >
            More skills
            <ExternalLink className="size-3.5" aria-hidden />
          </a>
        </div>
        <ul className="divide-y rounded-lg border bg-card text-card-foreground">
          {STARTER_PROMPTS.map((example) => (
            <li key={example.title} className="flex items-start gap-3 p-4">
              <div className="min-w-0 flex-1 space-y-1">
                <h3 className="text-sm font-medium">{example.title}</h3>
                <p className="text-sm text-muted-foreground">
                  {example.prompt}
                </p>
              </div>
              <CopyButton
                value={example.prompt}
                minimal
                className="size-8 shrink-0"
                label={`Copy ${example.title} prompt`}
              />
            </li>
          ))}
        </ul>
      </section>

      {verificationDialog}
      <ConfirmDeleteDialog
        open={removalAllowed}
        onOpenChange={(open) => {
          if (!open) setRemovalKeyId(null)
        }}
        resourceType="API key"
        resourceName={AI_HARNESS_KEY_NAME}
        verb="Remove"
        consequences="The unused admin key stops working and this setup step is marked done. An agent already connected with another credential keeps working. The removal is recorded in the audit log."
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

/** The connection's state, in a shape and a word as well as a colour. */
function ConnectionStatus({
  status,
  keyUnusable,
}: {
  status: OnboardingHarnessStatus
  keyUnusable: boolean
}) {
  if (status === 'connected') return <Status tone="ok" label="Connected" />
  if (status === 'waiting')
    return <Status tone="running" label="Waiting for first request" />
  if (status === 'unknown') return <Status tone="idle" label="Status unknown" />
  if (keyUnusable) return <Status tone="warn" label="Key cannot connect" />
  return <Status tone="idle" label="Not connected" />
}

/**
 * One step of the list: its number (a check once done), what to do, the one
 * line needed to do it, and the action or commands.
 */
function Step({
  number,
  title,
  caption,
  done,
  action,
  children,
}: {
  number: number
  title: string
  caption: string
  done: boolean
  action?: ReactNode
  children?: ReactNode
}) {
  return (
    <li className="flex gap-3 p-4 sm:gap-4 sm:p-5">
      <span
        aria-hidden="true"
        className={cn(
          'flex size-7 shrink-0 items-center justify-center rounded-full border text-xs font-medium tabular-nums',
          done
            ? 'border-foreground text-foreground'
            : 'border-border text-muted-foreground'
        )}
      >
        {done ? <Check className="size-4" /> : number}
      </span>
      <div className="min-w-0 flex-1 space-y-3">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="min-w-0 space-y-1">
            <h2 className="text-base font-medium leading-7">
              {title}
              {done ? <span className="sr-only">, done</span> : null}
            </h2>
            <p className="text-sm text-muted-foreground">{caption}</p>
          </div>
          {action ? <div className="shrink-0">{action}</div> : null}
        </div>
        {children}
      </div>
    </li>
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
      <p className="text-sm font-medium">{label}</p>
      <div className="flex min-w-0 items-start gap-2 rounded-md border bg-muted/40 px-3 py-2">
        <pre
          tabIndex={0}
          className={cn(
            'm-0 min-w-0 flex-1 overflow-x-auto py-1 font-mono text-xs',
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
    <div className="space-y-1.5">
      <p className="text-sm font-medium">
        Copy this key now. It is shown once.
      </p>
      <div className="flex min-w-0 items-center gap-2 rounded-md border bg-muted/40 px-3 py-2">
        <ScrollRegion
          className="min-w-0 flex-1"
          regionLabel="Admin key, scrollable"
        >
          <code className="block w-max select-all whitespace-nowrap font-mono text-xs">
            {value}
          </code>
        </ScrollRegion>
        <CopyButton
          value={value}
          minimal
          className="size-7 shrink-0 rounded-md"
          label="Copy admin key"
        />
      </div>
    </div>
  )
}
