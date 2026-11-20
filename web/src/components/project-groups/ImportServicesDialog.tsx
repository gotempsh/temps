// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import * as z from 'zod/v4'
import { toast } from 'sonner'
import { Check, FolderGit2, Loader2, RefreshCw } from 'lucide-react'
import {
  detectPublicPresetsOptions,
  getGroupRepositoryOptions,
  getRepositoryPresetLiveOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  getInstantiation,
  importProjectServices,
  retryInstantiation,
} from '@/api/client/sdk.gen'
import type { ProjectGroupResponse, ProjectPresetResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
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
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { getErrorMessage } from '@/utils/errorHandling'
import { ServicePresetIcon } from './ServicePresetIcon'
import {
  followInstantiation,
  InstantiationPollError,
  type FollowOutcome,
} from './follow-instantiation'
import {
  defaultSelection,
  fromPublicPreset,
  importedApp,
  importRowStates,
  isNestedApp,
  normalizeDirectory,
  suggestServiceName,
  type ImportRowState,
} from './repository-import'
import { useInvalidateGroups } from './useProjectGroups'

interface ImportServicesDialogProps {
  group: ProjectGroupResponse
  open: boolean
  onOpenChange: (open: boolean) => void
}

/**
 * An import the server has not finished. The server creates each service,
 * adds it to the project and makes it follow the repository, recording every
 * step; retrying continues from the step that failed instead of creating a
 * second service. `keys[i]` is the row of the app sent at position `i`.
 */
interface ActiveImport {
  id: number
  keys: string[]
  names: string[]
  states: ImportRowState[]
}

const keyOf = (app: ProjectPresetResponse) =>
  `${app.preset}::${normalizeDirectory(app.path)}`

/** One scanned app: whether to add it and what to call the service. */
const importFormSchema = z.object({
  rows: z
    .array(
      z.object({
        key: z.string(),
        name: z.string(),
        selected: z.boolean(),
      })
    )
    .superRefine((rows, ctx) => {
      rows.forEach((row, index) => {
        if (row.selected && !row.name.trim()) {
          ctx.addIssue({
            code: 'custom',
            path: [index, 'name'],
            message: 'Give the service a name.',
          })
        }
      })
    }),
})

type ImportFormValues = z.infer<typeof importFormSchema>

/** The names, checked rows and row states a freshly scanned list starts with. */
function seedRows({
  apps,
  allDirs,
  repoName,
  serviceNames,
  usedDirectories,
  unfinished,
}: {
  apps: ProjectPresetResponse[]
  allDirs: string[]
  repoName: string
  serviceNames: string[]
  usedDirectories: Map<string, string>
  unfinished: ActiveImport | null
}): {
  names: Record<string, string>
  selected: Set<string>
  status: Record<string, ImportRowState>
} {
  const taken = new Set(serviceNames.map((name) => name.toLowerCase()))
  const preselected = defaultSelection(allDirs, new Set(usedDirectories.keys()))
  const names: Record<string, string> = {}
  const selected = new Set<string>()
  for (const app of apps) {
    const key = keyOf(app)
    const name = suggestServiceName(app.path, repoName, taken)
    taken.add(name)
    names[key] = name
    if (preselected.has(normalizeDirectory(app.path))) selected.add(key)
  }
  // An import left half-done keeps its rows checked, so finishing it is
  // one click instead of a service stranded outside the project.
  const status: Record<string, ImportRowState> = {}
  if (unfinished) {
    unfinished.keys.forEach((key, i) => {
      names[key] = unfinished.names[i]
      selected.add(key)
      status[key] = unfinished.states[i] ?? {
        state: 'failed',
        message: 'Not added yet. Retry to continue.',
      }
    })
  }
  return { names, selected, status }
}

/**
 * Scans the project's repository and turns the apps it finds into services
 * of this project, each following the project's repository and branch.
 */
export function ImportServicesDialog({
  group,
  open,
  onOpenChange,
}: ImportServicesDialogProps) {
  const invalidate = useInvalidateGroups()
  // Directories and names already taken in this project. The server plans
  // unique slugs itself, so services elsewhere need no client-side check.
  const services = group.projects
  const repoQuery = useQuery({
    ...getGroupRepositoryOptions({ path: { group_id: group.id } }),
    enabled: open,
  })
  const repo = repoQuery.data?.repository ?? null

  // A connected repository is scanned through its synced record; a public
  // one anonymously.
  const connectedScan = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: repo?.repository_id ?? 0 },
      query: { branch: repo?.main_branch },
    }),
    enabled: open && !repo?.public_provider && Boolean(repo?.repository_id),
    retry: false,
  })
  const publicScan = useQuery({
    ...detectPublicPresetsOptions({
      path: {
        provider: repo?.public_provider ?? 'github',
        owner: repo?.repo_owner ?? '',
        repo: repo?.repo_name ?? '',
      },
      query: { branch: repo?.main_branch },
    }),
    enabled: open && Boolean(repo?.public_provider),
    retry: false,
  })
  const isPublic = Boolean(repo?.public_provider)
  const scan = isPublic ? publicScan : connectedScan
  const scannable = isPublic || Boolean(repo?.repository_id)
  const apps = useMemo(
    () =>
      isPublic
        ? (publicScan.data?.presets ?? []).map(fromPublicPreset)
        : (connectedScan.data?.presets ?? []),
    [isPublic, publicScan.data, connectedScan.data]
  )

  // Directories of this repository that already have a service.
  const usedDirectories = useMemo(() => {
    const used = new Map<string, string>()
    if (!repo) return used
    for (const s of services) {
      if (s.repo_owner === repo.repo_owner && s.repo_name === repo.repo_name) {
        used.set(normalizeDirectory(s.directory), s.name)
      }
    }
    return used
  }, [repo, services])

  // What the form starts from; replaced (and the form with it) whenever a
  // freshly scanned list is seeded below. Edits live in the form.
  const [seed, setSeed] = useState<ImportFormValues>({ rows: [] })
  const [status, setStatus] = useState<Record<string, ImportRowState>>({})
  const [running, setRunning] = useState(false)
  const [importError, setImportError] = useState<string | null>(null)
  // Kept across retries and reopening the dialog, unlike `status`.
  const [active, setActive] = useState<ActiveImport | null>(null)
  // Stops following when the dialog closes or unmounts; the server keeps
  // running the import either way.
  const following = useRef<AbortController | null>(null)
  useEffect(() => () => following.current?.abort(), [])

  const allDirs = useMemo(
    () => apps.map((a) => normalizeDirectory(a.path)),
    [apps]
  )

  // Suggest a name for every app; check the ones worth adding by default.
  // Recomputed when the scan result or dialog changes, not on every edit, by
  // comparing the inputs while rendering rather than in an effect.
  const seedInputs = [
    open,
    apps,
    repo?.repository_id,
    repo?.public_provider,
  ] as const
  const [seededFrom, setSeededFrom] = useState<typeof seedInputs | null>(null)
  if (!seededFrom || seedInputs.some((v, i) => v !== seededFrom[i])) {
    setSeededFrom(seedInputs)
    if (open && repo && apps.length > 0) {
      const seeded = seedRows({
        apps,
        allDirs,
        repoName: repo.repo_name,
        serviceNames: services.map((s) => s.name),
        usedDirectories,
        unfinished: active,
      })
      setSeed({
        rows: apps.map((app) => ({
          key: keyOf(app),
          name: seeded.names[keyOf(app)] ?? '',
          selected: seeded.selected.has(keyOf(app)),
        })),
      })
      setStatus(seeded.status)
    }
  }

  const form = useForm<ImportFormValues>({
    resolver: zodResolver(importFormSchema),
    values: seed,
  })
  const watchedRows = useWatch({ control: form.control, name: 'rows' })
  // The form takes a new seed one commit after it is set; until it has, the
  // seed itself stands in, so a row is never looked up in the previous scan.
  const rows =
    watchedRows?.length === seed.rows.length &&
    watchedRows.every((row, i) => row.key === seed.rows[i].key)
      ? watchedRows
      : seed.rows
  const rowIndexOf = (key: string) => rows.findIndex((r) => r.key === key)
  const isSelected = (key: string) =>
    rows.find((r) => r.key === key)?.selected ?? false
  const selectedCount = rows.filter((r) => r.selected).length

  const toggle = (key: string) => {
    const index = rowIndexOf(key)
    if (index >= 0) {
      form.setValue(`rows.${index}.selected`, !rows[index].selected, {
        shouldDirty: true,
      })
    }
  }

  const selectOnly = (keys: Set<string>) =>
    rows.forEach((row, index) =>
      form.setValue(`rows.${index}.selected`, keys.has(row.key), {
        shouldDirty: true,
      })
    )

  /**
   * Poll the import, updating its rows, until the server finishes it, the
   * deadline passes or the dialog stops following it.
   */
  const follow = (current: ActiveImport): Promise<FollowOutcome> => {
    following.current?.abort()
    const controller = new AbortController()
    following.current = controller
    return followInstantiation(current.id, {
      signal: controller.signal,
      fetchInstantiation: async (id, signal) =>
        (await getInstantiation({ path: { id }, signal, throwOnError: true }))
          .data,
      onUpdate: (data) => {
        const states = importRowStates(data)
        setActive((prev) =>
          prev?.id === current.id ? { ...prev, states } : prev
        )
        setStatus((prev) => {
          const next = { ...prev }
          current.keys.forEach((key, i) => {
            const state = states[i]
            if (state) next[key] = state
          })
          return next
        })
      },
    })
  }

  /** Close the dialog; the server finishes the import on its own. */
  const keepInBackground = () => {
    following.current?.abort()
    // The project overview tracks it from here (UnfinishedImportsNotice);
    // reopening the dialog starts a fresh selection.
    setActive(null)
    setRunning(false)
    onOpenChange(false)
    toast.info(
      `The import keeps running. Its progress is shown on ${group.name}'s overview.`
    )
  }

  const importSelected = async (values: ImportFormValues) => {
    if (!repo) return
    setRunning(true)
    setImportError(null)
    let startedId: number | null = active?.id ?? null
    try {
      let current = active
      if (current) {
        await retryInstantiation({
          path: { id: current.id },
          throwOnError: true,
        })
      } else {
        // Names are already checked by the form's schema.
        const nameOf = (key: string) =>
          values.rows.find((row) => row.key === key)?.name ?? ''
        const chosen = apps.filter(
          (app) =>
            values.rows.find((row) => row.key === keyOf(app))?.selected ===
              true && status[keyOf(app)]?.state !== 'done'
        )
        const { data } = await importProjectServices({
          path: { group_id: group.id },
          body: {
            apps: chosen.map((app) => importedApp(app, nameOf(keyOf(app)))),
            idempotency_key: crypto.randomUUID(),
          },
          throwOnError: true,
        })
        current = {
          id: data.id,
          keys: chosen.map(keyOf),
          names: chosen.map((app) => nameOf(keyOf(app)).trim()),
          states: importRowStates(data),
        }
        setActive(current)
        startedId = current.id
      }
      const outcome = await follow(current)
      if (outcome.kind === 'aborted') return
      if (outcome.kind === 'timed_out') {
        setImportError(
          `Import #${current.id} is taking longer than expected and keeps running on the server. Close this dialog; its progress is shown on the project overview.`
        )
        return
      }
      const result = outcome.instantiation
      if (result.status === 'succeeded') {
        setActive(null)
        const added = result.services.length
        toast.success(
          `Added ${added} ${added === 1 ? 'service' : 'services'} to ${group.name}`
        )
      } else {
        setImportError(
          result.status === 'interrupted'
            ? 'The server restarted before the import finished. Retry to continue where it stopped.'
            : `The import stopped: ${result.error ?? 'a step failed'}. Retrying continues where it stopped; services already added are kept.`
        )
      }
    } catch (err) {
      setImportError(
        err instanceof InstantiationPollError
          ? err.message
          : startedId !== null
            ? getErrorMessage(err, `Import #${startedId} could not be retried.`)
            : getErrorMessage(err, 'The import could not be started.')
      )
    } finally {
      setRunning(false)
      await invalidate()
    }
  }

  // Closing is only held back between the click and the server accepting
  // the import; once it has an id it can keep running without the dialog.
  const canClose = !running || active !== null

  const pending = apps.filter(
    (app) => isSelected(keyOf(app)) && status[keyOf(app)]?.state !== 'done'
  ).length
  const failed =
    importError !== null ||
    Object.values(status).some((s) => s.state === 'failed')
  const allDone =
    apps.length > 0 &&
    pending === 0 &&
    Object.keys(status).length > 0 &&
    !failed

  let body: React.ReactNode
  if (repoQuery.isLoading) {
    body = <Scanning label="Loading the project's repository…" />
  } else if (repoQuery.isError) {
    body = (
      <Problem
        message={getErrorMessage(
          repoQuery.error,
          "The project's repository could not be loaded."
        )}
        onRetry={() => void repoQuery.refetch()}
      />
    )
  } else if (!repo) {
    body = (
      <p className="text-sm text-muted-foreground">
        This project has no repository yet. Connect one first.
      </p>
    )
  } else if (!scannable) {
    body = (
      <p className="text-sm text-muted-foreground">
        {repo.repo_owner}/{repo.repo_name} is not in its git connection&apos;s
        repository list yet, so it cannot be scanned. Sync the connection under
        Git providers, then try again.
      </p>
    )
  } else if (scan.isLoading) {
    body = (
      <Scanning
        label={`Scanning ${repo.repo_owner}/${repo.repo_name}@${repo.main_branch} for apps…`}
      />
    )
  } else if (scan.isError) {
    body = (
      <Problem
        message={getErrorMessage(
          scan.error,
          'The repository could not be scanned.'
        )}
        onRetry={() => void scan.refetch()}
      />
    )
  } else if (apps.length === 0) {
    body = (
      <p className="text-sm text-muted-foreground">
        No apps were detected in {repo.repo_owner}/{repo.repo_name}@
        {repo.main_branch}. Apps are recognised by their framework files or a
        Dockerfile.
      </p>
    )
  } else {
    const selectable = apps.filter((a) => status[keyOf(a)]?.state !== 'done')
    const allChecked =
      selectable.length > 0 && selectable.every((a) => isSelected(keyOf(a)))
    body = (
      <div className="space-y-2">
        <div className="flex items-center justify-between text-xs text-muted-foreground">
          <span>
            {apps.length} {apps.length === 1 ? 'app' : 'apps'} found ·{' '}
            {selectedCount} selected
            {selectedCount === 0 && ' — check the ones to deploy'}
          </span>
          <button
            type="button"
            className="font-medium text-foreground hover:underline disabled:opacity-50"
            disabled={running}
            onClick={() =>
              selectOnly(
                allChecked
                  ? new Set()
                  : new Set(selectable.map((a) => keyOf(a)))
              )
            }
          >
            {allChecked ? 'Clear' : 'Select all'}
          </button>
        </div>
        <ul className="max-h-80 divide-y overflow-y-auto rounded-lg border">
          {apps.map((app) => {
            const key = keyOf(app)
            const dir = normalizeDirectory(app.path)
            const existing = usedDirectories.get(dir)
            const rowStatus = status[key]
            const done = rowStatus?.state === 'done'
            const created = active?.keys.includes(key) ?? false
            const index = rowIndexOf(key)
            return (
              <li key={key} className="px-3 py-2">
                <FormField
                  control={form.control}
                  name={`rows.${index}.name`}
                  render={({ field }) => (
                    <FormItem className="space-y-1">
                      <div className="flex items-center gap-3">
                        <Checkbox
                          checked={isSelected(key)}
                          disabled={running || done || created}
                          onCheckedChange={() => toggle(key)}
                          aria-label={`Add ${dir}`}
                        />
                        <ServicePresetIcon
                          preset={app.preset}
                          className="size-6"
                        />
                        <div className="min-w-0 flex-1">
                          <p className="truncate font-mono text-sm">
                            {dir === '.' ? '(repository root)' : dir}
                          </p>
                          <p className="truncate text-xs text-muted-foreground">
                            {app.presetLabel}
                            {isNestedApp(dir, allDirs) &&
                              ' · inside another app'}
                            {existing &&
                              !done &&
                              !created &&
                              ` · already deployed as ${existing}`}
                          </p>
                        </div>
                        {done ? (
                          <span className="flex items-center gap-1 text-xs text-muted-foreground">
                            <Check className="size-3.5" /> Added
                          </span>
                        ) : rowStatus?.state === 'running' ? (
                          <span className="flex items-center gap-1 text-xs text-muted-foreground">
                            <Loader2 className="size-3.5 animate-spin" />{' '}
                            Adding…
                          </span>
                        ) : (
                          <FormControl>
                            <Input
                              {...field}
                              value={field.value ?? ''}
                              disabled={running || created || !isSelected(key)}
                              className="h-8 w-40"
                              aria-label={`Service name for ${dir}`}
                            />
                          </FormControl>
                        )}
                      </div>
                      <FormMessage className="pl-7 text-xs font-normal" />
                      {rowStatus?.state === 'failed' && (
                        <p
                          role="alert"
                          className="pl-7 text-xs text-destructive"
                        >
                          {rowStatus.message}
                        </p>
                      )}
                    </FormItem>
                  )}
                />
              </li>
            )
          })}
        </ul>
      </div>
    )
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(v) => {
        if (v) return onOpenChange(true)
        if (running && active) keepInBackground()
        else if (canClose) onOpenChange(false)
      }}
    >
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Add services from the repository</DialogTitle>
          <DialogDescription>
            {repo ? (
              <>
                Apps found in{' '}
                <span className="font-mono">
                  {repo.repo_owner}/{repo.repo_name}@{repo.main_branch}
                </span>
                . Each becomes a service of {group.name} that builds from its
                directory and follows the project&apos;s branch.
                {isPublic &&
                  ' The repository is public and read without a connection, so they deploy when you deploy them, not on push.'}
              </>
            ) : (
              'Each app in the repository becomes a service of this project.'
            )}
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            onSubmit={(event) => {
              event.preventDefault()
              if (running || (pending === 0 && !active)) return
              // Retrying a started import sends nothing from the form.
              if (active) void importSelected(form.getValues())
              else void form.handleSubmit(importSelected)(event)
            }}
            className="grid gap-4"
          >
            {body}
            {importError && (
              <p role="alert" className="text-sm text-destructive">
                {importError}
              </p>
            )}
            <DialogFooter>
              {scannable && scan.isSuccess && (
                <Button
                  type="button"
                  variant="ghost"
                  className="mr-auto"
                  onClick={() => void scan.refetch()}
                  disabled={running || scan.isFetching}
                >
                  <RefreshCw
                    className={
                      scan.isFetching ? 'size-4 animate-spin' : 'size-4'
                    }
                  />
                  Scan again
                </Button>
              )}
              {running && active ? (
                <Button
                  type="button"
                  variant="outline"
                  onClick={keepInBackground}
                >
                  Keep running in background
                </Button>
              ) : (
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => onOpenChange(false)}
                  disabled={!canClose}
                >
                  {allDone ? 'Done' : 'Cancel'}
                </Button>
              )}
              {!allDone && (
                <Button
                  type="submit"
                  disabled={running || (pending === 0 && !active)}
                >
                  {running ? (
                    <>
                      <Loader2 className="size-4 animate-spin" /> Adding…
                    </>
                  ) : active ? (
                    'Retry'
                  ) : (
                    `Add ${pending} ${pending === 1 ? 'service' : 'services'}`
                  )}
                </Button>
              )}
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}

function Scanning({ label }: { label: string }) {
  return (
    <p className="flex items-center gap-2 text-sm text-muted-foreground">
      <FolderGit2 className="size-4" />
      {label}
      <Loader2 className="size-4 animate-spin" />
    </p>
  )
}

function Problem({
  message,
  onRetry,
}: {
  message: string
  onRetry: () => void
}) {
  return (
    <div role="alert" className="flex items-center gap-3 text-sm">
      <p className="flex-1 text-destructive">{message}</p>
      <Button type="button" variant="outline" size="sm" onClick={onRetry}>
        <RefreshCw className="size-4" /> Retry
      </Button>
    </div>
  )
}
