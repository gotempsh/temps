// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery } from '@tanstack/react-query'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import * as z from 'zod/v4'
import { toast } from 'sonner'
import { Globe, Loader2 } from 'lucide-react'
import {
  getPublicRepositoryOptions,
  listConnectionsOptions,
  listGitProvidersOptions,
  setGroupRepositoryMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  MemberSyncResult,
  ProjectGroupResponse,
  RepositoryResponse,
} from '@/api/client'
import { BranchSelector } from '@/components/deployments/BranchSelector'
import { RepositorySelector } from '@/components/repositories/RepositorySelector'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { parsePublicRepositoryUrl } from '@/lib/public-repository'
import { getErrorMessage } from '@/utils/errorHandling'
import { useOpenSession } from './useOpenSession'
import { useInvalidateGroups } from './useProjectGroups'

interface ConnectRepositoryDialogProps {
  group: ProjectGroupResponse
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Called after the repository was saved, e.g. to open the import. */
  onConnected?: () => void
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
 */
export function ConnectRepositoryDialog({
  group,
  open,
  onOpenChange,
  onConnected,
}: ConnectRepositoryDialogProps) {
  // Each opening remounts the form, so it starts from the project's current
  // repository instead of whatever was left from the last time.
  const session = useOpenSession(open)
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <ConnectRepositoryForm
          key={session}
          group={group}
          open={open}
          onOpenChange={onOpenChange}
          onConnected={onConnected}
        />
      </DialogContent>
    </Dialog>
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
  /** `null` until the user picks one; a single connection needs no choice. */
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

function ConnectRepositoryForm({
  group,
  open,
  onOpenChange,
  onConnected,
}: ConnectRepositoryDialogProps) {
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
  const [modeChoice, connectionChoice, repository, publicUrl, branchChoice] =
    useWatch({
      control: form.control,
      name: [
        'modeChoice',
        'connectionChoice',
        'repository',
        'publicUrl',
        'branchChoice',
      ],
    })

  const connections = useQuery({ ...listConnectionsOptions(), enabled: open })
  const providers = useQuery({ ...listGitProvidersOptions(), enabled: open })
  const activeConnections = useMemo(
    () => (connections.data?.connections ?? []).filter((c) => c.is_active),
    [connections.data]
  )
  const providerOf = (providerId: number) =>
    (providers.data ?? []).find((p) => p.id === providerId)

  const noConnections = connections.isSuccess && activeConnections.length === 0

  // Without any connection, a public repository is the way in.
  const mode: Mode =
    modeChoice ??
    (noConnections && !current?.git_provider_connection_id
      ? 'public'
      : 'connection')

  const connectionId =
    connectionChoice ??
    (activeConnections.length === 1 ? activeConnections[0].id : null)

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
    enabled: open && mode === 'public' && Boolean(publicTarget),
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

  const save = useMutation({ ...setGroupRepositoryMutation() })
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
      onOpenChange(false)
      onConnected?.()
    } catch (err) {
      form.setError('root', {
        type: 'server',
        message: getErrorMessage(err, 'The repository could not be saved.'),
      })
    }
  }

  return (
    <Form {...form}>
      <form
        onSubmit={(event) => void form.handleSubmit(submit)(event)}
        className="space-y-4"
      >
        <DialogHeader>
          <DialogTitle>
            {current ? 'Change repository' : 'Connect a repository'}
          </DialogTitle>
          <DialogDescription>
            For monorepos: connect the repository once, then add each app in it
            (say <code>apps/web</code> and <code>apps/api</code>) as a service.
            Services created from it follow the project&apos;s branch; each
            keeps its own directory.
          </DialogDescription>
        </DialogHeader>

        <Tabs
          value={mode}
          onValueChange={(v) => {
            form.setValue('modeChoice', v as Mode)
            form.clearErrors()
            forgetBranch()
          }}
        >
          <TabsList className="grid w-full grid-cols-2">
            <TabsTrigger value="connection">Git connection</TabsTrigger>
            <TabsTrigger value="public">Public repository</TabsTrigger>
          </TabsList>
        </Tabs>

        {mode === 'public' ? (
          <div className="space-y-4">
            <FormField
              control={form.control}
              name="publicUrl"
              render={({ field }) => (
                <FormItem className="space-y-2">
                  <FormLabel htmlFor="public-repository-url">
                    Repository URL
                  </FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      id="public-repository-url"
                      placeholder="https://github.com/owner/repository"
                      onChange={(e) => {
                        field.onChange(e)
                        // A different repository starts on its own branch.
                        forgetBranch()
                      }}
                      autoFocus
                    />
                  </FormControl>
                  {publicUrlProblem ? (
                    <FormMessage className="text-xs font-normal" />
                  ) : publicInfo.isFetching ? (
                    <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
                      <Loader2 className="size-3 animate-spin" /> Looking up the
                      repository…
                    </p>
                  ) : publicInfo.isError ? (
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
                  ) : (
                    <p className="text-xs text-muted-foreground">
                      A public repository on GitHub or GitLab; no git connection
                      needed.
                    </p>
                  )}
                </FormItem>
              )}
            />

            {publicTarget && publicInfo.data && (
              <div className="space-y-2">
                <Label>Branch</Label>
                <BranchSelector
                  repoOwner={publicTarget.owner}
                  repoName={publicTarget.name}
                  gitUrl={publicUrl}
                  defaultBranch={publicInfo.data.default_branch}
                  value={branch}
                  onChange={pickBranch}
                />
              </div>
            )}

            <p className="rounded-md bg-muted/50 px-3 py-2 text-sm text-muted-foreground">
              Temps can read a public repository but cannot receive its pushes,
              so its services deploy when you deploy them, not on every push. To
              deploy on push, connect it through a{' '}
              <Link
                to="/git-providers/add"
                className="font-medium text-foreground underline"
              >
                git provider
              </Link>{' '}
              instead.
            </p>
          </div>
        ) : connections.isError ? (
          <p role="alert" className="text-sm text-destructive">
            Git connections could not be loaded.{' '}
            <button
              type="button"
              className="font-medium underline"
              onClick={() => void connections.refetch()}
            >
              Retry
            </button>
          </p>
        ) : noConnections ? (
          <div className="rounded-lg border border-dashed px-4 py-6 text-center text-sm">
            <p className="font-medium">No git provider is connected yet</p>
            <p className="mt-1 text-muted-foreground">
              Connect GitHub, GitLab, Gitea or Bitbucket to use private
              repositories and deploy on every push. For a public repository,
              use the Public repository tab.
            </p>
            <Button asChild variant="outline" size="sm" className="mt-3">
              <Link to="/git-providers/add">Connect a git provider</Link>
            </Button>
          </div>
        ) : (
          <div className="space-y-4">
            {activeConnections.length > 1 && (
              <div className="space-y-2">
                <Label>Git connection</Label>
                <Select
                  value={connectionId ? String(connectionId) : undefined}
                  onValueChange={(v) => {
                    form.setValue('connectionChoice', Number(v))
                    form.setValue('repository', null)
                    forgetBranch()
                  }}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Choose a connection" />
                  </SelectTrigger>
                  <SelectContent>
                    {activeConnections.map((c) => {
                      const provider = providerOf(c.provider_id)
                      return (
                        <SelectItem key={c.id} value={String(c.id)}>
                          <span className="flex items-center gap-2">
                            <ProviderLogo
                              providerType={
                                provider?.provider_type ?? 'generic'
                              }
                              className="size-4"
                            />
                            {c.account_name}
                            {provider && (
                              <span className="text-muted-foreground">
                                · {provider.name}
                              </span>
                            )}
                          </span>
                        </SelectItem>
                      )
                    })}
                  </SelectContent>
                </Select>
              </div>
            )}

            {connectionId && (
              <RepositorySelector
                connectionId={connectionId}
                selectedRepository={repository}
                onSelect={(repo) => {
                  form.setValue('repository', repo)
                  forgetBranch()
                }}
                showAsCard={false}
                compactMode
                itemsPerPage={6}
                providerType={
                  providerOf(
                    activeConnections.find((c) => c.id === connectionId)
                      ?.provider_id ?? -1
                  )?.provider_type
                }
              />
            )}

            {repository && (
              <div className="space-y-2">
                <Label>Branch</Label>
                <BranchSelector
                  repoOwner={repository.owner}
                  repoName={repository.name}
                  connectionId={connectionId ?? undefined}
                  defaultBranch={repository.default_branch}
                  value={branch}
                  onChange={pickBranch}
                />
                <p className="text-xs text-muted-foreground">
                  Pushes to this branch deploy the project&apos;s services.
                </p>
              </div>
            )}
          </div>
        )}

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

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {failures.length > 0 ? 'Close' : 'Cancel'}
          </Button>
          <Button type="submit" disabled={!canSave}>
            {save.isPending
              ? 'Saving…'
              : failures.length > 0
                ? 'Retry'
                : 'Save repository'}
          </Button>
        </DialogFooter>
      </form>
    </Form>
  )
}
