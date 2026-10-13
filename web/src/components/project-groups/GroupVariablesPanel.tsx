// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useState } from 'react'
import { useMutation, useQueries, useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import { KeyRound, Lock, MoreHorizontal, Plus, RefreshCw } from 'lucide-react'
import {
  createGroupEnvVarMutation,
  deleteGroupEnvVarMutation,
  getEnvironmentsOptions,
  getInheritedEnvVarsOptions,
  listGroupEnvVarsOptions,
  updateGroupEnvVarMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { GroupEnvVarResponse, ProjectGroupResponse } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
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
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { getErrorMessage } from '@/utils/errorHandling'
import { useInvalidateGroups } from './useProjectGroups'

const ALL_ENVIRONMENTS = '__all__'

export function GroupVariablesPanel({
  group,
}: {
  group: ProjectGroupResponse
}) {
  const invalidate = useInvalidateGroups()
  const [editing, setEditing] = useState<GroupEnvVarResponse | 'new' | null>(
    null
  )
  const [deleting, setDeleting] = useState<GroupEnvVarResponse | null>(null)

  const vars = useQuery({
    ...listGroupEnvVarsOptions({ path: { group_id: group.id } }),
    retry: false,
  })

  // Which services define a key themselves (and therefore ignore ours).
  const inherited = useQueries({
    queries: group.projects.map((p) => ({
      ...getInheritedEnvVarsOptions({ path: { project_id: p.id } }),
      retry: false,
    })),
  })
  const overriddenBy = useMemo(() => {
    const map = new Map<string, string[]>()
    inherited.forEach((result, index) => {
      const member = group.projects[index]
      if (!member) return
      const seen = new Set<string>()
      for (const v of result.data?.variables ?? []) {
        if (v.defined_on_project && !seen.has(v.key)) {
          seen.add(v.key)
          map.set(v.key, [...(map.get(v.key) ?? []), member.name])
        }
      }
    })
    return map
  }, [group.projects, inherited])
  // Services whose own variables could not be read: whether they override a
  // shared variable is unknown, and the rows say so.
  const uncheckedServices = inherited.filter((r) => r.isError).length
  const retryInherited = () => {
    for (const result of inherited) if (result.isError) void result.refetch()
  }

  // Scope choices: the environments the group's services actually have.
  const environments = useQueries({
    queries: group.projects.map((p) => ({
      ...getEnvironmentsOptions({ path: { project_id: p.id } }),
      retry: false,
    })),
  })
  const environmentSlugs = useMemo(() => {
    const slugs = new Set<string>(['production'])
    for (const result of environments) {
      for (const env of result.data ?? []) {
        if (!env.is_preview) slugs.add(env.slug)
      }
    }
    return [...slugs].sort()
  }, [environments])

  const remove = useMutation({ ...deleteGroupEnvVarMutation() })

  const confirmDelete = async () => {
    if (!deleting) return
    try {
      await remove.mutateAsync({
        path: { group_id: group.id, var_id: deleting.id },
      })
      await invalidate()
      toast.success(
        `Removed ${deleting.key}; services drop it on their next deployment`
      )
    } catch (err) {
      toast.error(getErrorMessage(err, `Could not remove ${deleting.key}`))
    } finally {
      setDeleting(null)
    }
  }

  return (
    <section aria-labelledby="shared-vars" className="space-y-3">
      <div className="flex flex-col gap-3 border-b pb-2 sm:flex-row sm:items-end sm:justify-between">
        <div>
          <h2 id="shared-vars" className="font-semibold">
            Shared variables
          </h2>
          <p className="max-w-2xl text-sm text-muted-foreground">
            Every service in {group.name} receives these on its next deployment.
            A service's own variable with the same name always wins, so use this
            for values that are genuinely shared, like an API base URL or a log
            level.
          </p>
        </div>
        <Button onClick={() => setEditing('new')} className="shrink-0">
          <Plus className="size-4" />
          Add variable
        </Button>
      </div>

      {vars.isLoading ? (
        <Skeleton className="h-24 w-full" />
      ) : vars.isError ? (
        <Alert variant="destructive">
          <AlertTitle>Shared variables could not be loaded</AlertTitle>
          <AlertDescription>
            <p>
              {getErrorMessage(vars.error, 'Check your connection and retry.')}
            </p>
            <Button
              variant="outline"
              size="sm"
              className="mt-3"
              onClick={() => void vars.refetch()}
            >
              <RefreshCw className="size-4" />
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      ) : (vars.data ?? []).length === 0 ? (
        <div className="rounded-xl border border-dashed px-6 py-10 text-center">
          <p className="font-medium">No shared variables yet</p>
          <p className="mx-auto mt-1 max-w-md text-sm text-muted-foreground">
            For example, set <code className="font-mono">API_BASE_URL</code>{' '}
            once here instead of on each of the {group.projects.length}{' '}
            services.
          </p>
          <Button
            variant="outline"
            size="sm"
            className="mt-4"
            onClick={() => setEditing('new')}
          >
            <Plus className="size-4" />
            Add the first variable
          </Button>
        </div>
      ) : (
        <div className="divide-y rounded-lg border">
          {uncheckedServices > 0 && (
            <p className="flex flex-wrap items-center gap-2 px-3 py-2 text-xs text-destructive">
              The variables of {uncheckedServices}{' '}
              {uncheckedServices === 1 ? 'service' : 'services'} could not be
              read, so overrides by{' '}
              {uncheckedServices === 1 ? 'it are' : 'them are'} not shown.
              <Button
                variant="link"
                size="sm"
                className="h-auto p-0 text-xs"
                onClick={retryInherited}
              >
                Retry
              </Button>
            </p>
          )}
          {(vars.data ?? []).map((v) => {
            const overrides = overriddenBy.get(v.key) ?? []
            return (
              <div key={v.id} className="flex items-center gap-3 px-3 py-2.5">
                <div className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                  {v.is_secret ? (
                    <Lock className="size-4" />
                  ) : (
                    <KeyRound className="size-4" />
                  )}
                </div>
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="truncate font-mono text-sm font-medium">
                      {v.key}
                    </span>
                    <Badge variant="secondary" className="font-normal">
                      {v.environment_slug ?? 'All environments'}
                    </Badge>
                    {v.is_secret && (
                      <Badge variant="outline" className="font-normal">
                        Secret
                      </Badge>
                    )}
                    {!v.environment_slug && v.include_in_preview && (
                      <Badge variant="outline" className="font-normal">
                        Previews
                      </Badge>
                    )}
                  </div>
                  {!v.is_secret && (
                    <p
                      className="truncate font-mono text-xs"
                      title={v.value ?? ''}
                    >
                      {v.value ? (
                        v.value
                      ) : (
                        <span className="italic text-muted-foreground">
                          empty
                        </span>
                      )}
                    </p>
                  )}
                  <p className="truncate text-xs text-muted-foreground">
                    {appliesTo({
                      key: v.key,
                      overrides,
                      serviceTotal: group.projects.length,
                      unchecked: uncheckedServices,
                    })}
                  </p>
                </div>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="size-8"
                      aria-label={`Actions for ${v.key}`}
                    >
                      <MoreHorizontal className="size-4" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem onSelect={() => setEditing(v)}>
                      Change value
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      className="text-destructive"
                      onSelect={() => setDeleting(v)}
                    >
                      Remove
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            )
          })}
        </div>
      )}

      <VariableDialog
        groupId={group.id}
        variable={editing}
        environmentSlugs={environmentSlugs}
        onClose={() => setEditing(null)}
      />

      <AlertDialog
        open={Boolean(deleting)}
        onOpenChange={(open) => !open && setDeleting(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Remove {deleting?.key}?</AlertDialogTitle>
            <AlertDialogDescription>
              Services in {group.name} stop receiving it on their next
              deployment. Running deployments keep their current value until
              then.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={() => void confirmDelete()}>
              Remove variable
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  )
}

interface VariableDialogProps {
  groupId: number
  variable: GroupEnvVarResponse | 'new' | null
  environmentSlugs: string[]
  onClose: () => void
}

function VariableDialog({
  groupId,
  variable,
  environmentSlugs,
  onClose,
}: VariableDialogProps) {
  const invalidate = useInvalidateGroups()
  // The last variable opened, kept while the dialog animates closed so its
  // title does not flash "Change undefined".
  const [shown, setShown] = useState<GroupEnvVarResponse | 'new' | null>(
    variable
  )
  if (variable && variable !== shown) setShown(variable)
  const isNew = shown === 'new'
  const existing = shown && shown !== 'new' ? shown : null
  // A secret's value cannot be read back: empty means "keep it".
  const keepsSecret = Boolean(existing?.is_secret)
  const [key, setKey] = useState('')
  const [value, setValue] = useState('')
  const [scope, setScope] = useState(ALL_ENVIRONMENTS)
  const [isSecret, setIsSecret] = useState(false)
  // Previews are opt-in, as for a service's own variables.
  const [includeInPreview, setIncludeInPreview] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!variable) return
    setError(null)
    if (variable === 'new') {
      setKey('')
      setValue('')
      setScope(ALL_ENVIRONMENTS)
      setIsSecret(false)
      setIncludeInPreview(false)
    } else {
      setKey(variable.key)
      setValue(variable.is_secret ? '' : (variable.value ?? ''))
      setScope(variable.environment_slug ?? ALL_ENVIRONMENTS)
      setIsSecret(variable.is_secret)
      setIncludeInPreview(variable.include_in_preview)
    }
  }, [variable])

  const create = useMutation({ ...createGroupEnvVarMutation() })
  const update = useMutation({ ...updateGroupEnvVarMutation() })
  const pending = create.isPending || update.isPending

  const submit = async (e: { preventDefault: () => void }) => {
    e.preventDefault()
    setError(null)
    try {
      if (isNew) {
        await create.mutateAsync({
          path: { group_id: groupId },
          body: {
            key: key.trim(),
            value,
            is_secret: isSecret,
            environment_slug: scope === ALL_ENVIRONMENTS ? null : scope,
            include_in_preview: includeInPreview,
          },
        })
        toast.success(
          `${key.trim()} added; services pick it up on their next deployment`
        )
      } else if (existing) {
        await update.mutateAsync({
          path: { group_id: groupId, var_id: existing.id },
          body: {
            // A plain value is sent as shown, empty included; an empty secret
            // field keeps the stored secret.
            value: keepsSecret && value === '' ? null : value,
            is_secret: isSecret,
            include_in_preview: includeInPreview,
          },
        })
        toast.success(
          `${existing.key} updated; services pick it up on their next deployment`
        )
      }
      await invalidate()
      onClose()
    } catch (err) {
      setError(getErrorMessage(err, 'The variable could not be saved.'))
    }
  }

  return (
    <Dialog
      open={Boolean(variable)}
      onOpenChange={(open) => !open && onClose()}
    >
      <DialogContent className="sm:max-w-lg">
        <form onSubmit={submit} className="space-y-4">
          <DialogHeader>
            <DialogTitle>
              {isNew ? 'Add shared variable' : `Change ${existing?.key}`}
            </DialogTitle>
            <DialogDescription>
              Applies to each service's next deployment. A service that sets the
              same name itself keeps its own value.
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-2">
            <Label htmlFor="group-var-key">Name</Label>
            <Input
              id="group-var-key"
              className="font-mono"
              placeholder="API_BASE_URL"
              value={key}
              disabled={!isNew}
              onChange={(e) => setKey(e.target.value.toUpperCase())}
              autoFocus={isNew}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="group-var-value">Value</Label>
            <Textarea
              id="group-var-value"
              className="font-mono"
              rows={2}
              placeholder={
                keepsSecret
                  ? 'Leave empty to keep the current secret'
                  : 'https://api.example.com'
              }
              value={value}
              onChange={(e) => setValue(e.target.value)}
              autoFocus={!isNew}
            />
          </div>

          <div className="space-y-2">
            <Label>Environments</Label>
            <Select value={scope} onValueChange={setScope} disabled={!isNew}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL_ENVIRONMENTS}>
                  All environments
                </SelectItem>
                {environmentSlugs.map((slug) => (
                  <SelectItem key={slug} value={slug}>
                    Only {slug}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {!isNew && (
              <p className="text-xs text-muted-foreground">
                To change where it applies, remove it and add it again.
              </p>
            )}
          </div>

          {scope === ALL_ENVIRONMENTS && (
            <div className="flex items-center justify-between gap-4 rounded-md border px-3 py-2">
              <div>
                <Label htmlFor="group-var-preview">Include in previews</Label>
                <p className="text-xs text-muted-foreground">
                  Preview deployments of pull requests also receive it.
                </p>
              </div>
              <Switch
                id="group-var-preview"
                checked={includeInPreview}
                onCheckedChange={setIncludeInPreview}
              />
            </div>
          )}

          <div className="flex items-center justify-between gap-4 rounded-md border px-3 py-2">
            <div>
              <Label htmlFor="group-var-secret">Secret</Label>
              <p className="text-xs text-muted-foreground">
                Write-only: nobody can read the value back, including you.
              </p>
            </div>
            <Switch
              id="group-var-secret"
              checked={isSecret}
              onCheckedChange={setIsSecret}
              disabled={keepsSecret && value === ''}
            />
          </div>

          {error && (
            <p role="alert" className="text-sm font-medium text-destructive">
              {error}
            </p>
          )}

          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending || (isNew && !key.trim())}>
              {pending ? 'Saving…' : isNew ? 'Add variable' : 'Save'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function appliesTo({
  key,
  overrides,
  serviceTotal,
  unchecked,
}: {
  key: string
  overrides: string[]
  serviceTotal: number
  unchecked: number
}): string {
  if (overrides.length > 0) {
    return `Overridden by ${overrides.join(', ')}, which ${overrides.length === 1 ? 'sets' : 'set'} ${key} itself`
  }
  if (unchecked > 0) {
    return `Applies to ${serviceTotal} ${serviceTotal === 1 ? 'service' : 'services'} unless one of the ${unchecked} not checked sets ${key} itself`
  }
  return `Applies to all ${serviceTotal} ${serviceTotal === 1 ? 'service' : 'services'}`
}
