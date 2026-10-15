// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery } from '@tanstack/react-query'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import * as z from 'zod/v4'
import { toast } from 'sonner'
import { FolderGit2, Globe, Loader2, Plus } from 'lucide-react'
import {
  getPublicRepositoryOptions,
  listConnectionsOptions,
  setGroupRepositoryMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  MemberSyncResult,
  ProjectGroupResponse,
  RepositoryResponse,
} from '@/api/client'
import { BranchSelector } from '@/components/deployments/BranchSelector'
import { GitConnectionRepositoryPicker } from '@/components/repositories/GitConnectionRepositoryPicker'
import { RepositoryAvatar } from '@/components/repositories/RepositoryAvatar'
import { COMPACT_GRID_CLASS } from '@/components/repositories/RepositoryList'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Field, Settings } from '@temps-sdk/ds'
import { Card, CardContent } from '@/components/ui/card'
import { Form, FormField } from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { parsePublicRepositoryUrl } from '@/lib/public-repository'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'
import { useInvalidateGroups } from './useProjectGroups'

interface ConnectRepositoryPanelProps {
  group: ProjectGroupResponse
  title: ReactNode
  description: ReactNode
  /** Rendered first in the form body, e.g. a link back to the project. */
  backLink?: ReactNode
  /** "Cancel": leave without saving. */
  onCancel: () => void
  /** Called once the repository was saved and every service followed it. */
  onConnected: () => void
}

type Mode = 'connection' | 'public'

/** Hosts whose public repositories are read without a connection. */
const PUBLIC_HOSTS = { github: 'github.com', gitlab: 'gitlab.com' } as const

/**
 * Points a project at a repository and branch, either through a git
 * connection or, for a public GitHub/GitLab repository, without one.
 * Services that follow the project's repository are updated to match; each
 * outcome is shown, so a service that could not be moved is never silently
 * left behind.
 *
 * The connection path browses repositories with the same picker as the
 * new-service flow (`GitConnectionRepositoryPicker`).
 */
export function ConnectRepositoryPanel({
  group,
  title,
  description,
  backLink,
  onCancel,
  onConnected,
}: ConnectRepositoryPanelProps) {
  const invalidate = useInvalidateGroups()
  const current = group.repository
  const [failures, setFailures] = useState<MemberSyncResult[]>([])

  const form = useForm<ConnectRepositoryValues>({
    resolver: zodResolver(connectRepositorySchema),
    mode: 'onChange',
    defaultValues: {
      modeChoice: currentPublicUrl(current) ? 'public' : null,
      connectionChoice: current?.git_provider_connection_id ?? null,
      repository: null,
      publicUrl: currentPublicUrl(current),
      branchChoice: '',
    },
  })
  const [
    modeChoice,
    connectionChoice,
    pickedRepository,
    publicUrl,
    branchChoice,
  ] = useWatch({
    control: form.control,
    name: [
      'modeChoice',
      'connectionChoice',
      'repository',
      'publicUrl',
      'branchChoice',
    ],
  })

  const connections = useQuery({ ...listConnectionsOptions() })
  const activeConnections = useMemo(
    () => (connections.data?.connections ?? []).filter((c) => c.is_active),
    [connections.data]
  )
  const connectionsLoading = connections.isPending
  const noConnections = connections.isSuccess && activeConnections.length === 0

  // Without any connection, a public repository is the way in.
  const mode: Mode =
    modeChoice ??
    (noConnections && !current?.git_provider_connection_id
      ? 'public'
      : 'connection')

  // The user's pick if it is still usable, otherwise the first connection,
  // as the new-service flow does.
  const connectionId =
    activeConnections.find((c) => c.id === connectionChoice)?.id ??
    activeConnections[0]?.id ??
    null

  // A repository belongs to the connection it was listed under. When the
  // effective connection changes underneath the pick (the connection was
  // deactivated elsewhere and the list refreshed), the pick no longer applies:
  // saving it would pair its owner and name with a different provider.
  const repository =
    pickedRepository &&
    pickedRepository.git_provider_connection_id === connectionId
      ? pickedRepository
      : null

  // The public repository the URL names, when it is one Temps can read.
  const publicTarget = useMemo(() => publicTargetOf(publicUrl), [publicUrl])
  const publicUrlProblem = form.formState.errors.publicUrl?.message ?? null
  const publicInfo = useQuery({
    ...getPublicRepositoryOptions({
      path: {
        provider: publicTarget?.provider ?? 'github',
        owner: publicTarget?.owner ?? '',
        repo: publicTarget?.name ?? '',
      },
    }),
    enabled: mode === 'public' && Boolean(publicTarget),
    retry: false,
  })

  // A branch the user picked wins; until then it starts on the repository's
  // default branch unless this is the repo the project already uses. Every
  // change of target clears the pick (see the handlers below), so a new
  // repository is never shown with the previous one's branch.
  const lookedUp = mode === 'public' ? (publicInfo.data ?? null) : null
  const branch =
    branchChoice.trim() !== ''
      ? branchChoice
      : seededBranch(current, mode === 'public' ? lookedUp : repository)

  const pickBranch = (value: string) =>
    form.setValue('branchChoice', value, { shouldDirty: true })
  const forgetBranch = () => form.setValue('branchChoice', '')

  const save = useMutation({
    ...setGroupRepositoryMutation(),
    meta: { errorTitle: 'Failed to save repository' },
  })
  const followers = group.projects.filter((p) => p.inherits_repository)

  const target =
    mode === 'public'
      ? publicTarget && publicInfo.isSuccess
        ? { owner: publicTarget.owner, name: publicTarget.name }
        : null
      : connectionId && repository
        ? { owner: repository.owner, name: repository.name }
        : null
  const canSave = Boolean(target) && Boolean(branch.trim()) && !save.isPending

  const submit = async () => {
    if (!target || !branch.trim()) return
    setFailures([])
    try {
      const result = await save.mutateAsync({
        path: { group_id: group.id },
        body:
          mode === 'public'
            ? {
                public_provider: publicTarget?.provider,
                repo_owner: target.owner,
                repo_name: target.name,
                main_branch: branch.trim(),
              }
            : {
                git_provider_connection_id: connectionId,
                repo_owner: target.owner,
                repo_name: target.name,
                main_branch: branch.trim(),
              },
      })
      await invalidate()
      const failed = result.services.filter((s) => s.error)
      if (failed.length > 0) {
        setFailures(failed)
        toast.warning(
          `Saved, but ${failed.length} ${failed.length === 1 ? 'service' : 'services'} could not be updated`
        )
        return
      }
      toast.success(
        `${group.name} now deploys from ${target.owner}/${target.name}@${branch.trim()}`
      )
      onConnected()
    } catch (err) {
      form.setError('root', {
        type: 'server',
        message: getErrorMessage(err, 'The repository could not be saved.'),
      })
    }
  }

  // Enter in a search box must filter, never save: only the Save button does.
  const keepEnterFromSaving = (event: React.KeyboardEvent) => {
    if (event.key === 'Enter' && event.target instanceof HTMLInputElement) {
      event.preventDefault()
    }
  }

  return (
    <Form {...form}>
      <Settings
        title={title}
        description={description}
        errors={publicUrlProblem ? { 'Repository URL': publicUrlProblem } : {}}
        onSubmit={(event) => {
          event.preventDefault()
          void form.handleSubmit(submit)()
        }}
        saving={save.isPending}
        dirty={canSave}
        saveLabel={failures.length > 0 ? 'Retry' : 'Save repository'}
        onCancel={onCancel}
      >
        {backLink}

        <SelectionSummary
          mode={mode}
          repository={repository}
          publicName={publicInfo.data?.full_name ?? null}
          target={target}
          branchSelector={
            target ? (
              <BranchSelector
                repoOwner={target.owner}
                repoName={target.name}
                connectionId={
                  mode === 'connection'
                    ? (connectionId ?? undefined)
                    : undefined
                }
                gitUrl={mode === 'public' ? publicUrl : undefined}
                defaultBranch={
                  mode === 'public'
                    ? publicInfo.data?.default_branch
                    : repository?.default_branch
                }
                value={branch}
                onChange={pickBranch}
              />
            ) : null
          }
        />

        <Card>
          <CardContent
            className="space-y-4 pt-6"
            onKeyDown={keepEnterFromSaving}
          >
            <Tabs
              value={mode}
              onValueChange={(v) => {
                form.setValue('modeChoice', v as Mode)
                form.clearErrors()
                forgetBranch()
              }}
            >
              <TabsList>
                <TabsTrigger value="connection">Git connection</TabsTrigger>
                <TabsTrigger value="public">Public repository</TabsTrigger>
              </TabsList>
            </Tabs>

            {mode === 'public' ? (
              <PublicRepositoryFields
                form={form}
                publicTarget={publicTarget}
                publicInfo={publicInfo}
                problem={publicUrlProblem}
                onUrlChanged={forgetBranch}
              />
            ) : connectionsLoading ? (
              <ConnectionPickerSkeleton />
            ) : connections.isError ? (
              <ReadFailure
                resource="Git connections"
                error={connections.error}
                onRetry={() => void connections.refetch()}
                retrying={connections.isFetching}
              />
            ) : noConnections ? (
              <NoConnectionsOnboarding
                onUsePublic={() => {
                  form.setValue('modeChoice', 'public')
                  form.clearErrors()
                }}
              />
            ) : (
              <GitConnectionRepositoryPicker
                connections={activeConnections}
                connectionId={connectionId}
                onConnectionChange={(id) => {
                  form.setValue('connectionChoice', id)
                  form.setValue('repository', null)
                  forgetBranch()
                }}
                onRepositorySelect={(repo) => {
                  form.setValue('repository', repo)
                  forgetBranch()
                }}
                selectedRepositoryId={repository?.id}
              />
            )}
          </CardContent>
        </Card>

        {followers.length > 0 && (
          <p className="rounded-md bg-muted/50 px-3 py-2 text-sm text-muted-foreground">
            {followers.length === 1
              ? `${followers[0].name} follows this project's repository and will be pointed at the new one.`
              : `${followers.length} services follow this project's repository and will be pointed at the new one.`}{' '}
            Their directories stay as they are; nothing is redeployed until the
            next deploy.
          </p>
        )}

        {form.formState.errors.root && (
          <p role="alert" className="text-sm font-medium text-destructive">
            {form.formState.errors.root.message}
          </p>
        )}
        {failures.length > 0 && (
          <div
            role="alert"
            className="space-y-1 rounded-md border border-destructive/40 px-3 py-2 text-sm"
          >
            <p className="font-medium">
              These services still use their previous repository or branch:
            </p>
            <ul className="list-disc pl-5 text-muted-foreground">
              {failures.map((f) => (
                <li key={f.project_id}>
                  <span className="font-medium text-foreground">{f.name}</span>:{' '}
                  {f.error}
                </li>
              ))}
            </ul>
            <p className="text-muted-foreground">
              Fix the cause and save again to retry them.
            </p>
          </div>
        )}
      </Settings>
    </Form>
  )
}

/** The public repository the project already points at, as a URL. */
function currentPublicUrl(current: ProjectGroupResponse['repository']): string {
  const provider = current?.public_provider as
    keyof typeof PUBLIC_HOSTS | null | undefined
  if (current && provider && PUBLIC_HOSTS[provider]) {
    return `https://${PUBLIC_HOSTS[provider]}/${current.repo_owner}/${current.repo_name}`
  }
  return ''
}

/**
 * What the user typed or picked. Everything else (the tab that applies, the
 * seeded branch, the repository a URL names) is derived from it on render,
 * never copied back into it.
 */
interface ConnectRepositoryValues {
  /** `null` until the user picks a tab. */
  modeChoice: Mode | null
  /** `null` until the user picks one; the first connection is the default. */
  connectionChoice: number | null
  repository: RepositoryResponse | null
  publicUrl: string
  /** The branch the user picked; empty means "the repository's own". */
  branchChoice: string
}

const PUBLIC_URL_FORMAT_HELP =
  'Enter a repository URL like https://github.com/owner/repo'
const PUBLIC_URL_UNSUPPORTED_HELP =
  'Only public repositories on github.com and gitlab.com (without subgroups) can be used without a connection. Connect other repositories through a git provider.'

/** The public repository a URL names, when Temps can read it unauthenticated. */
function publicTargetOf(url: string) {
  const parsed = parsePublicRepositoryUrl(url)
  return parsed && !parsed.instanceUrl && !parsed.owner.includes('/')
    ? parsed
    : null
}

const connectRepositorySchema = z
  .object({
    modeChoice: z.enum(['connection', 'public']).nullable(),
    connectionChoice: z.number().nullable(),
    repository: z.custom<RepositoryResponse>().nullable(),
    publicUrl: z.string(),
    branchChoice: z.string(),
  })
  .superRefine((values, ctx) => {
    // The URL is only a rule on the Public repository tab, and an untouched
    // (empty) one is left to the disabled Save button rather than shouted at.
    if (values.modeChoice === 'connection' || !values.publicUrl.trim()) return
    if (!parsePublicRepositoryUrl(values.publicUrl)) {
      ctx.addIssue({
        code: 'custom',
        path: ['publicUrl'],
        message: PUBLIC_URL_FORMAT_HELP,
      })
    } else if (!publicTargetOf(values.publicUrl)) {
      ctx.addIssue({
        code: 'custom',
        path: ['publicUrl'],
        message: PUBLIC_URL_UNSUPPORTED_HELP,
      })
    }
  })

/**
 * Where a repository starts: the branch the project already uses when it is
 * the same repository, otherwise the repository's default branch.
 */
function seededBranch(
  current: ProjectGroupResponse['repository'],
  repo: { owner: string; name: string; default_branch: string } | null
): string {
  if (!repo) return ''
  return current?.repo_owner === repo.owner && current.repo_name === repo.name
    ? current.main_branch
    : repo.default_branch
}

/** Mirrors the picker's layout so the page does not jump when it arrives. */
function ConnectionPickerSkeleton() {
  return (
    <div
      className="space-y-3"
      aria-busy="true"
      role="status"
      aria-label="Loading repositories"
    >
      <Skeleton className="h-10 w-full" />
      <div className={cn('grid', COMPACT_GRID_CLASS)}>
        {Array.from({ length: 9 }).map((_, i) => (
          <div key={i} className="rounded-lg border bg-card p-2.5">
            <div className="flex items-center gap-3">
              <Skeleton className="size-9 shrink-0 rounded-md" />
              <div className="flex-1 space-y-1.5">
                <Skeleton className="h-3 w-2/3" />
                <Skeleton className="h-2.5 w-1/3" />
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

/** No git provider yet: say what it unlocks and where to set one up. */
function NoConnectionsOnboarding({ onUsePublic }: { onUsePublic: () => void }) {
  return (
    <div className="flex flex-col items-center rounded-lg border border-dashed px-4 py-10 text-center">
      <FolderGit2 className="mb-3 size-8 text-muted-foreground" />
      <p className="text-sm font-medium">No git provider is connected yet</p>
      <p className="mt-1 max-w-md text-sm text-muted-foreground">
        Connect GitHub, GitLab, Gitea or Bitbucket to pick a private repository
        (say <code>acme/web</code>) and deploy on every push. For a public
        repository you can skip this.
      </p>
      <div className="mt-4 flex flex-wrap items-center justify-center gap-2">
        <Button asChild size="sm">
          <Link to="/git-providers/add">
            <Plus className="size-4" />
            Connect a git provider
          </Link>
        </Button>
        <Button type="button" size="sm" variant="outline" onClick={onUsePublic}>
          Use a public repository
        </Button>
      </div>
    </div>
  )
}

interface PublicRepositoryFieldsProps {
  form: ReturnType<typeof useForm<ConnectRepositoryValues>>
  publicTarget: ReturnType<typeof publicTargetOf>
  publicInfo: {
    isFetching: boolean
    isError: boolean
    error: unknown
    data?: { full_name: string; description?: string | null } | undefined
  }
  problem: string | null
  onUrlChanged: () => void
}

function PublicRepositoryFields({
  form,
  publicTarget,
  publicInfo,
  problem,
  onUrlChanged,
}: PublicRepositoryFieldsProps) {
  return (
    <div className="space-y-4">
      <FormField
        control={form.control}
        name="publicUrl"
        render={({ field }) => (
          <div className="space-y-2">
            <Field
              label="Repository URL"
              error={problem ?? undefined}
              description={
                'A public repository on GitHub or GitLab; no git connection needed.'
              }
            >
              {(fieldProps) => (
                <Input
                  {...field}
                  {...fieldProps}
                  placeholder="https://github.com/owner/repository"
                  onChange={(e) => {
                    field.onChange(e)
                    // A different repository starts on its own branch.
                    onUrlChanged()
                  }}
                  autoFocus
                />
              )}
            </Field>
            {problem ? null : publicTarget && publicInfo.isFetching ? (
              <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
                <Loader2 className="size-3 animate-spin" /> Looking up the
                repository…
              </p>
            ) : publicTarget && publicInfo.isError ? (
              <p role="alert" className="text-xs text-destructive">
                {getErrorMessage(
                  publicInfo.error,
                  'This repository could not be found, or it is not public.'
                )}
              </p>
            ) : publicInfo.data ? (
              <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
                <Globe className="size-3" />
                <span className="font-medium text-foreground">
                  {publicInfo.data.full_name}
                </span>
                {publicInfo.data.description
                  ? ` · ${publicInfo.data.description}`
                  : ''}
              </p>
            ) : null}
          </div>
        )}
      />

      <p className="rounded-md bg-muted/50 px-3 py-2 text-sm text-muted-foreground">
        Temps can read a public repository but cannot receive its pushes, so its
        services deploy when you deploy them, not on every push. To deploy on
        push, connect it through a{' '}
        <Link
          to="/git-providers/add"
          className="font-medium text-foreground underline"
        >
          git provider
        </Link>{' '}
        instead.
      </p>
    </div>
  )
}

interface SelectionSummaryProps {
  mode: Mode
  repository: RepositoryResponse | null
  publicName: string | null
  target: { owner: string; name: string } | null
  branchSelector: ReactNode
}

/**
 * What will be saved: the chosen repository and the branch services follow.
 * Always rendered, so choosing a repository never shifts the page.
 */
function SelectionSummary({
  mode,
  repository,
  publicName,
  target,
  branchSelector,
}: SelectionSummaryProps) {
  return (
    <Card>
      <CardContent className="flex flex-col gap-4 pt-6 sm:flex-row sm:items-end sm:justify-between">
        <div className="flex min-w-0 items-center gap-3">
          {mode === 'connection' && repository && (
            <RepositoryAvatar repository={repository} />
          )}
          <div className="min-w-0">
            <p className="text-xs text-muted-foreground">Repository</p>
            {target ? (
              <div className="flex min-w-0 items-center gap-2 font-medium">
                <span className="break-all">
                  {mode === 'public' && publicName
                    ? publicName
                    : `${target.owner}/${target.name}`}
                </span>
                {mode === 'connection' && repository?.private && (
                  <Badge variant="secondary">Private</Badge>
                )}
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">
                {mode === 'public'
                  ? 'Enter a public repository URL below'
                  : 'Choose a repository from the list below'}
              </p>
            )}
          </div>
        </div>
        {target && (
          <Field
            label="Branch"
            description="Pushes to this branch deploy the project's services."
            className="min-w-0 sm:w-72"
          >
            {() => branchSelector}
          </Field>
        )}
      </CardContent>
    </Card>
  )
}
