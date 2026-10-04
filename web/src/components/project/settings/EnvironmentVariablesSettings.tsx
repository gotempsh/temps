// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  EnvironmentVariableResponse,
  EnvironmentInfo,
  EnvironmentResponse,
  ProjectResponse,
  listRepositoriesByConnection,
} from '@/api/client'
import {
  createEnvironmentVariableMutation,
  deleteEnvironmentVariableMutation,
  detectPublicEnvExampleOptions,
  getEnvironmentsOptions,
  getEnvironmentVariablesOptions,
  getPublicComposeServicesOptions,
  getRepositoryComposeServicesLiveOptions,
  getRepositoryEnvExampleLiveOptions,
  updateEnvironmentVariableMutation,
} from '@/api/client/@tanstack/react-query.gen'
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableCell,
  TableHead,
} from '@/components/ui/table'
import { Collapsible, CollapsibleContent } from '@/components/ui/collapsible'
import { Button } from '@/components/ui/button'
import { EnvironmentVariableValue } from './EnvironmentVariableValue'
import { RecordLink, useUrlState } from '@temps-sdk/ds'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import { ChevronDown, Eye, EyeOff, KeyRound, Plus, Upload } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import { Skeleton } from '@/components/ui/skeleton'
import { Checkbox } from '@/components/ui/checkbox'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { ImportEnvDialog } from '@/components/ui/import-env-dialog'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { Switch } from '@/components/ui/switch'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  getEnvVarValue,
  getResolvedEnvVars,
  getResolvedEnvVarValue,
  indexResolvedByKey,
  type ResolvedEnvVar,
} from '@/lib/resolved-env-vars'
import {
  createCredentialRevealGuard,
  credentialValueForScope,
  type ScopedCredentialValue,
} from '@/lib/credential-reveal-state'
import { IntegrationBadge } from './IntegrationBadge'
import {
  EnvironmentVariableChecks,
  type EnvironmentVariableCheck,
} from './EnvironmentVariableChecks'
import { useHttpChecks, indicatorsBySubject } from './http-checks'
import { Link, useNavigate } from 'react-router'
import {
  parsePublicRepositoryUrl,
  publicRepositoryProvider,
} from '@/lib/public-repository'
import {
  discoverComposeEnvironmentVariables,
  type DiscoveredEnvironmentVariable,
} from '@/lib/compose-environment-discovery'
import { repositoryFilePath } from '@/lib/repository-file-path'
import {
  compareEnvironmentVariableKeys,
  variableAppliesToEnvironment,
  orderEnvironments,
  orderVariableEnvironments,
} from '@/lib/environment-variable-comparison'

function EnvironmentBadges({
  environments,
  previewIds,
  includeInPreview,
}: {
  environments: EnvironmentInfo[]
  previewIds: ReadonlySet<number>
  includeInPreview: boolean
}) {
  const [expanded, setExpanded] = useState(false)
  const ordered = orderVariableEnvironments(environments, previewIds)
  const visible = expanded ? ordered : ordered.slice(0, 2)
  const hiddenCount = ordered.length - visible.length

  return (
    <div className="flex flex-wrap items-center gap-2">
      {visible.map((env) => (
        <span
          key={env.id}
          className="inline-flex items-center rounded-full px-2 py-1 text-xs font-medium bg-secondary text-secondary-foreground"
        >
          {env.name}
        </span>
      ))}
      {includeInPreview && (
        <span className="inline-flex items-center rounded-full px-2 py-1 text-xs font-medium bg-blue-500/10 text-blue-700 dark:text-blue-400 border border-blue-500/20">
          Preview
        </span>
      )}
      {ordered.length > 2 && (
        <Button
          variant="ghost"
          size="sm"
          className="h-7 px-2 text-xs"
          onClick={() => setExpanded((current) => !current)}
          aria-expanded={expanded}
          aria-label={`${expanded ? 'Show fewer' : 'Show all'} environments`}
        >
          {expanded ? 'Show fewer' : `Show all ${hiddenCount} more`}
        </Button>
      )}
    </div>
  )
}

interface EnvironmentVariableRowProps {
  variable: EnvironmentVariableResponse
  project: ProjectResponse
  refetchEnvVariables: () => void
  isSelected: boolean
  onSelect: (id: number) => void
  showAllValues: boolean
  resolved?: ResolvedEnvVar
  checks: EnvironmentVariableCheck[]
  onManageChecks: () => void
  previewIds: ReadonlySet<number>
  allEnvironments: EnvironmentResponse[]
  environmentChoicesAvailable: boolean
}

function EnvironmentVariableRow({
  variable,
  project,
  refetchEnvVariables,
  isSelected,
  onSelect,
  showAllValues,
  resolved,
  checks,
  onManageChecks,
  previewIds,
  allEnvironments,
  environmentChoicesAvailable,
}: EnvironmentVariableRowProps) {
  const overridesService =
    resolved?.source.type === 'manual'
      ? (resolved.source.overrides_service ?? undefined)
      : undefined
  const [isVisible, setIsVisible] = useState(false)
  const [editValue, setEditValue] = useState('')
  const [isEditMultiline, setIsEditMultiline] = useState(false)
  const [revealedValue, setRevealedValue] = useState<
    ScopedCredentialValue | undefined
  >()
  const [isRevealing, setIsRevealing] = useState(false)
  const revealScope = `${project.id}:${variable.id}:${variable.updated_at}`
  const revealGuard = useRef(createCredentialRevealGuard())
  // Secret values remain write-only. Regular values can be revealed on demand.
  const isSecret = variable.is_secret ?? false

  useEffect(() => {
    const guard = createCredentialRevealGuard()
    revealGuard.current = guard
    // Drop plaintext whenever this row changes project, identity, or version.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setRevealedValue(undefined)
    setEditValue('')
    return () => guard.invalidate()
  }, [revealScope])

  const revealValue = async (): Promise<string | undefined> => {
    if (isSecret) return undefined
    // Capture the guard instance once: revealGuard.current gets swapped to a
    // fresh guard (new, empty request map) whenever revealScope changes, which
    // happens as soon as this row's own edit is saved and the list refetches.
    // Re-reading revealGuard.current after the await would compare against
    // that new, unrelated guard and always report the request as stale.
    const guard = revealGuard.current
    const request = guard.begin('value')
    setIsRevealing(true)
    try {
      const value = await getEnvVarValue(project.id, variable.key, variable.id)
      if (!guard.isCurrent('value', request)) return undefined
      setRevealedValue({ value, scope: revealScope })
      return value
    } catch {
      if (guard.isCurrent('value', request)) {
        toast.error(`Failed to reveal ${variable.key}`)
      }
      return undefined
    } finally {
      if (guard.finish('value', request)) {
        setIsRevealing(false)
      }
    }
  }

  useEffect(() => {
    revealGuard.current.cancel('value')
    // Bulk reveal excludes secrets, including a row promoted since the last
    // list refresh.
    if (isSecret) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setIsVisible(false)
      setRevealedValue(undefined)
      return
    }

    // This synchronizes non-secret rows with the explicit bulk toggle.
    setIsVisible(showAllValues)
    if (showAllValues) {
      void revealValue()
    } else {
      setRevealedValue(undefined)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showAllValues, isSecret, revealScope])

  const dataValue = credentialValueForScope(revealedValue, revealScope) ?? ''

  const toggleVisibility = async () => {
    if (isSecret) return
    if (isVisible) {
      revealGuard.current.cancel('value')
      setIsVisible(false)
      setRevealedValue(undefined)
      setEditValue('')
      setIsEditMultiline(false)
      return
    }
    setIsVisible(true)
    await revealValue()
  }

  const deleteMutation = useMutation({
    ...deleteEnvironmentVariableMutation(),
    meta: {
      errorTitle: 'Failed to delete environment variable',
    },
    onSuccess: () => {
      refetchEnvVariables()
      toast.success('Environment variable deleted')
    },
  })

  const updateMutation = useMutation({
    ...updateEnvironmentVariableMutation(),
    meta: {
      errorTitle: 'Failed to update environment variable',
    },
    onSuccess: () => {
      revealGuard.current.cancel('value')
      setRevealedValue(undefined)
      setEditValue('')
      setIsEditMultiline(false)
      refetchEnvVariables()
      toast.success('Environment variable updated')
    },
  })

  const handleDelete = async () => {
    await deleteMutation.mutateAsync({
      path: {
        project_id: project.id,
        var_id: variable.id,
      },
    })
  }

  const [isEditModalOpen, setIsEditModalOpen] = useState(false)
  const [selectedEditEnvironments, setSelectedEditEnvironments] = useState<
    number[]
  >(variable.environments.map((env) => env.id))
  const [editIncludeInPreview, setEditIncludeInPreview] = useState(
    variable.include_in_preview ?? false
  )
  // Whether the edit box actually holds the variable's current value. False
  // when it has not been explicitly revealed, or when reveal was denied or
  // failed. This distinguishes "cleared on purpose" from "never loaded".
  const [valueLoaded, setValueLoaded] = useState(false)
  // Opt-in conversion of an existing plain variable into a masked secret.
  // The classification stays one-way so a later list response cannot
  // accidentally expose it as a regular value.
  const [convertToSecret, setConvertToSecret] = useState(false)

  // Update selected environments and preview flag when variable changes (after refetch)
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSelectedEditEnvironments(variable.environments.map((env) => env.id))
    setEditIncludeInPreview(variable.include_in_preview ?? false)
  }, [variable.environments, variable.include_in_preview])

  const openEditDialog = async () => {
    setIsEditModalOpen(true)
    if (!isSecret) {
      const value = dataValue || (await revealValue())
      if (value !== undefined) {
        setEditValue(value)
        setIsEditMultiline(value.includes('\n'))
        setValueLoaded(true)
      }
    }
  }

  const handleEditDialogOpenChange = (open: boolean) => {
    setIsEditModalOpen(open)
    if (!open) {
      setEditValue('')
      setIsEditMultiline(false)
      setConvertToSecret(false)
      setValueLoaded(false)
      if (!isVisible && !showAllValues) {
        revealGuard.current.cancel('value')
        setRevealedValue(undefined)
      }
    }
  }

  // Both the secret case and the failed-reveal case mean "blank keeps what is
  // already stored" — say so, so an empty box is never mistaken for an empty value.
  const valuePlaceholder =
    isSecret || !valueLoaded ? 'Leave blank to keep current value' : undefined

  const submitEdit = async () => {
    // An empty box means "keep the existing ciphertext" whenever we never had
    // the value to begin with: always for secrets (never preloaded), and for a
    // regular variable whose reveal was denied or failed. Sending "" in that
    // case would overwrite the credential with an empty string — and if the
    // same save also promotes the variable, that loss is unrecoverable.
    // A cleared box after a *successful* reveal is a deliberate edit and is
    // still sent as-is.
    const valueField =
      editValue.length === 0 && (isSecret || !valueLoaded)
        ? undefined
        : editValue
    await updateMutation.mutateAsync({
      path: {
        project_id: project.id,
        var_id: variable.id,
      },
      body: {
        value: valueField,
        environment_ids: environmentChoicesAvailable
          ? selectedEditEnvironments
          : variable.environments.map((env) => env.id),
        key: variable.key,
        include_in_preview: environmentChoicesAvailable
          ? editIncludeInPreview
          : variable.include_in_preview,
        // Only sent when the operator asked for the conversion. Omitting the
        // field leaves the existing flag untouched; sending `false` against an
        // already-secret variable is rejected by the API as a demotion.
        ...(convertToSecret ? { is_secret: true } : {}),
      },
    })
    setIsEditModalOpen(false)
    setEditValue('')
    setConvertToSecret(false)
    setValueLoaded(false)
  }

  return (
    <>
      <TableRow
        className="border-b border-border/60"
        data-state={isSelected ? 'selected' : undefined}
      >
        <TableCell>
          <Checkbox
            checked={isSelected}
            onCheckedChange={() => onSelect(variable.id)}
            aria-label={`Select ${variable.key}`}
          />
        </TableCell>
        <TableCell>
          <div className="space-y-1 min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              {overridesService && (
                <IntegrationBadge service={overridesService} overridden />
              )}
              <RecordLink
                to={`/projects/${project.slug}/environment-variables/${variable.id}`}
                className="font-mono"
                aria-label={`View ${variable.key} details`}
              >
                {variable.key}
              </RecordLink>
              {isSecret && (
                <span
                  title="Sensitive value — write-only"
                  className="inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide bg-amber-500/10 text-amber-700 dark:text-amber-400 border border-amber-500/20"
                >
                  Secret
                </span>
              )}
              {overridesService && (
                <Link
                  to={`/storage/${overridesService.service_id}`}
                  className="inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide bg-muted text-muted-foreground border hover:bg-secondary hover:text-foreground"
                >
                  Overrides {overridesService.service_name}
                </Link>
              )}
            </div>
          </div>
        </TableCell>
        <TableCell>
          <div className="flex items-center gap-2 min-w-0 w-full sm:w-auto">
            {isVisible &&
            !isSecret &&
            credentialValueForScope(revealedValue, revealScope) !==
              undefined ? (
              <EnvironmentVariableValue name={variable.key} value={dataValue} />
            ) : (
              <span className="font-mono text-sm">
                {isVisible && isRevealing ? 'Revealing…' : '••••••••••••'}
              </span>
            )}
            {!isSecret && (
              <Button
                variant="ghost"
                size="sm"
                onClick={() => void toggleVisibility()}
                disabled={isRevealing}
                aria-label={
                  isVisible ? `Hide ${variable.key}` : `Reveal ${variable.key}`
                }
                title={isVisible ? 'Hide value' : 'Reveal value (audited)'}
              >
                {isVisible ? (
                  <EyeOff className="h-4 w-4" />
                ) : (
                  <Eye className="h-4 w-4" />
                )}
              </Button>
            )}
          </div>
        </TableCell>
        <TableCell>
          <EnvironmentBadges
            environments={variable.environments}
            previewIds={previewIds}
            includeInPreview={variable.include_in_preview}
          />
        </TableCell>
        <TableCell>
          <EnvironmentVariableChecks
            checks={checks}
            onManage={onManageChecks}
          />
        </TableCell>
        <TableCell className="text-right">
          <div className="flex items-center justify-end gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => void openEditDialog()}
              disabled={deleteMutation.isPending || updateMutation.isPending}
            >
              Edit
            </Button>
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button
                  variant="ghost"
                  className="text-muted-foreground hover:text-destructive"
                  size="sm"
                  disabled={
                    deleteMutation.isPending || updateMutation.isPending
                  }
                >
                  Delete
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>
                    Delete environment variable
                  </AlertDialogTitle>
                  <AlertDialogDescription className="space-y-3">
                    <p>
                      Are you sure you want to delete{' '}
                      <span className="font-medium">{variable.key}</span>? This
                      action cannot be undone.
                    </p>
                    {variable.environments &&
                      variable.environments.length > 0 && (
                        <div className="space-y-2">
                          <p className="text-sm font-medium text-foreground">
                            This variable is active on:
                          </p>
                          <div className="flex flex-wrap gap-2">
                            {variable.environments.map((env) => (
                              <span
                                key={env.name}
                                className="inline-flex items-center rounded-full px-2.5 py-1 text-xs font-medium bg-secondary text-secondary-foreground"
                              >
                                {env.name}
                              </span>
                            ))}
                          </div>
                        </div>
                      )}
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Cancel</AlertDialogCancel>
                  <AlertDialogAction onClick={handleDelete}>
                    Delete
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          </div>
        </TableCell>
      </TableRow>

      <Dialog open={isEditModalOpen} onOpenChange={handleEditDialogOpenChange}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit Environment Variable: {variable.key}</DialogTitle>
          </DialogHeader>
          <form
            onSubmit={(e) => {
              e.preventDefault()
              submitEdit()
            }}
          >
            <div className="space-y-4 py-4">
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <label className="text-sm font-medium">Value</label>
                  <label className="flex items-center gap-2 text-xs text-muted-foreground">
                    <Checkbox
                      checked={isEditMultiline}
                      onCheckedChange={(checked) =>
                        setIsEditMultiline(checked === true)
                      }
                    />
                    Multiline (e.g. .npmrc)
                  </label>
                </div>
                {isEditMultiline ? (
                  <Textarea
                    value={editValue}
                    onChange={(e) => setEditValue(e.target.value)}
                    className="font-mono resize-y"
                    rows={6}
                    placeholder={valuePlaceholder}
                  />
                ) : (
                  <Input
                    value={editValue}
                    onChange={(e) => setEditValue(e.target.value)}
                    className="font-mono"
                    placeholder={valuePlaceholder}
                  />
                )}
                {isSecret && (
                  <p className="text-xs text-muted-foreground">
                    Stored secret values cannot be revealed. Enter a replacement
                    value to rotate it, or leave this blank to keep it.
                  </p>
                )}
              </div>
              <div className="space-y-2">
                <label className="text-sm font-medium">Environments</label>
                {!environmentChoicesAvailable && (
                  <p className="text-xs text-muted-foreground">
                    Environment choices are unavailable. Saving keeps the
                    current assignments.
                  </p>
                )}
                <div className="flex flex-wrap gap-2">
                  {(allEnvironments ?? []).map((env) => (
                    <Button
                      type="button"
                      key={env.id}
                      disabled={!environmentChoicesAvailable}
                      variant={
                        selectedEditEnvironments.includes(env.id)
                          ? 'default'
                          : 'outline'
                      }
                      size="sm"
                      onClick={() => {
                        setSelectedEditEnvironments((prev) =>
                          prev.includes(env.id)
                            ? prev.filter((e) => e !== env.id)
                            : [...prev, env.id]
                        )
                      }}
                    >
                      {env.name}
                    </Button>
                  ))}
                </div>
              </div>
              <div className="flex items-center justify-between space-x-2 rounded-lg border p-4">
                <div className="flex-1 space-y-1">
                  <Label
                    htmlFor="edit-include-preview"
                    className="text-sm font-medium"
                  >
                    Include in Preview Environments
                  </Label>
                  <p className="text-sm text-muted-foreground">
                    Automatically add this variable to preview environments
                  </p>
                </div>
                <Switch
                  id="edit-include-preview"
                  disabled={!environmentChoicesAvailable}
                  checked={editIncludeInPreview}
                  onCheckedChange={setEditIncludeInPreview}
                />
              </div>
              {!isSecret && (
                <div
                  className={`flex items-center justify-between space-x-2 rounded-lg border p-4 ${
                    convertToSecret ? 'border-amber-500/40 bg-amber-500/5' : ''
                  }`}
                >
                  <div className="flex-1 space-y-1">
                    <Label
                      htmlFor="edit-convert-secret"
                      className="text-sm font-medium"
                    >
                      Convert to secret
                    </Label>
                    <p className="text-sm text-muted-foreground">
                      {convertToSecret
                        ? `On save, ${variable.key} becomes write-only. To make it a regular variable again you must delete it and create it anew.`
                        : 'Make this value write-only. Converting back requires deleting and recreating the variable.'}
                    </p>
                  </div>
                  <Switch
                    id="edit-convert-secret"
                    checked={convertToSecret}
                    onCheckedChange={setConvertToSecret}
                  />
                </div>
              )}
            </div>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => handleEditDialogOpenChange(false)}
              >
                Cancel
              </Button>
              <Button type="submit">Save Changes</Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  )
}

interface IntegrationEnvVarRowProps {
  projectId: number
  resolved: ResolvedEnvVar
  showAllValues: boolean
  environmentId: number | null
  previewIds: ReadonlySet<number>
}

function IntegrationEnvVarRow({
  projectId,
  resolved,
  showAllValues,
  environmentId,
  previewIds,
}: IntegrationEnvVarRowProps) {
  const [isVisible, setIsVisible] = useState(false)
  const [revealedValue, setRevealedValue] = useState<
    ScopedCredentialValue | undefined
  >()
  const [isFetching, setIsFetching] = useState(false)
  const revealGuard = useRef(createCredentialRevealGuard())
  const isIntegration = resolved.source.type === 'integration'
  const serviceId =
    resolved.source.type === 'integration'
      ? resolved.source.service.service_id
      : 'manual'
  const serviceUpdatedAt =
    resolved.source.type === 'integration'
      ? resolved.source.service.service_updated_at
      : 'manual'
  const revealScope = `${projectId}:${serviceId}:${serviceUpdatedAt}:${resolved.key}:${environmentId ?? 'all'}`
  const currentValue = credentialValueForScope(revealedValue, revealScope)

  useEffect(() => {
    const guard = createCredentialRevealGuard()
    revealGuard.current = guard
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setRevealedValue(undefined)
    return () => guard.invalidate()
  }, [revealScope])

  const revealValue = async () => {
    if (!isIntegration) return
    const guard = revealGuard.current
    const request = guard.begin('value')
    setIsFetching(true)
    try {
      const value = await getResolvedEnvVarValue(
        projectId,
        resolved.key,
        environmentId ?? undefined,
        serviceId === 'manual' ? undefined : serviceId
      )
      if (!guard.isCurrent('value', request)) return
      setRevealedValue({ value, scope: revealScope })
    } catch {
      if (guard.isCurrent('value', request)) {
        toast.error(`Failed to reveal ${resolved.key}`)
      }
    } finally {
      if (guard.finish('value', request)) {
        setIsFetching(false)
      }
    }
  }

  useEffect(() => {
    revealGuard.current.cancel('value')
    // This synchronizes the per-row reveal state with the explicit bulk toggle.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setIsVisible(showAllValues)
    if (showAllValues) {
      void revealValue()
    } else {
      setRevealedValue(undefined)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showAllValues, revealScope])

  if (resolved.source.type !== 'integration') return null
  const service = resolved.source.service

  const toggleVisibility = async () => {
    if (isVisible) {
      revealGuard.current.cancel('value')
      setIsVisible(false)
      setRevealedValue(undefined)
      return
    }
    setIsVisible(true)
    await revealValue()
  }

  const valueText = isVisible
    ? isFetching && !currentValue
      ? 'Revealing…'
      : (currentValue ?? resolved.value_preview)
    : '••••••••••••'

  return (
    <TableRow className="border-b border-border/60">
      <TableCell />
      <TableCell>
        <div className="space-y-1 flex-1 min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <IntegrationBadge service={service} />
            <p className="font-mono text-sm break-all">{resolved.key}</p>
            <span className="text-xs text-muted-foreground">
              from{' '}
              <Link
                to={`/storage/${service.service_id}`}
                className="underline-offset-2 hover:underline hover:text-foreground"
              >
                {service.service_name}
              </Link>
            </span>
          </div>
        </div>
      </TableCell>
      <TableCell>
        <div className="flex items-center gap-2 min-w-0">
          {isVisible && currentValue !== undefined ? (
            <EnvironmentVariableValue
              name={resolved.key}
              value={currentValue}
            />
          ) : (
            <span className="font-mono text-sm text-muted-foreground">
              {valueText}
            </span>
          )}
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void toggleVisibility()}
            disabled={isFetching}
            aria-label={
              isVisible ? `Hide ${resolved.key}` : `Reveal ${resolved.key}`
            }
          >
            {isVisible ? (
              <EyeOff className="h-4 w-4" />
            ) : (
              <Eye className="h-4 w-4" />
            )}
          </Button>
        </div>
      </TableCell>
      <TableCell>
        <EnvironmentBadges
          environments={resolved.environments}
          previewIds={previewIds}
          includeInPreview={resolved.include_in_preview}
        />
      </TableCell>
      <TableCell>
        <EnvironmentVariableChecks />
      </TableCell>
      <TableCell className="text-right">
        <Link
          className="text-sm text-muted-foreground hover:text-foreground underline-offset-4 hover:underline"
          to={`/storage/${service.service_id}`}
        >
          Manage service
        </Link>
      </TableCell>
    </TableRow>
  )
}

interface EnvironmentVariablesSettingsProps {
  project: ProjectResponse
}

interface AddEnvironmentVariableDialogProps {
  isOpen: boolean
  onOpenChange: (open: boolean) => void
  onSubmit: (values: {
    key: string
    value: string
    environments: number[]
    includeInPreview: boolean
    isSecret: boolean
  }) => Promise<void>
  allEnvironments: EnvironmentResponse[]
  disabledReason?: string
}

function AddEnvironmentVariableDialog({
  isOpen,
  onOpenChange,
  onSubmit,
  allEnvironments,
  disabledReason,
}: AddEnvironmentVariableDialogProps) {
  const [key, setKey] = useState('')
  const [value, setValue] = useState('')
  const [isMultiline, setIsMultiline] = useState(false)
  const [selectedEnvironments, setSelectedEnvironments] = useState<number[]>([])
  const [includeInPreview, setIncludeInPreview] = useState(false)
  const [isSecret, setIsSecret] = useState(false)
  const [hasInitialized, setHasInitialized] = useState(false)

  // Default-select all environments when the dialog first opens
  // But allow deselecting when includeInPreview is true
  useEffect(() => {
    if (isOpen && allEnvironments.length > 0) {
      if (!hasInitialized) {
        // Only auto-select on first open
        // eslint-disable-next-line react-hooks/set-state-in-effect
        setSelectedEnvironments(allEnvironments.map((env) => env.id))
        setHasInitialized(true)
      }
    } else if (!isOpen) {
      // Reset initialization flag when dialog closes
      setHasInitialized(false)
    }
  }, [isOpen, allEnvironments, hasInitialized])

  const handleSubmit = async () => {
    if (disabledReason) {
      toast.error(disabledReason)
      return
    }
    // Validate key and value are filled
    if (!key || !value) {
      toast.error('Please fill in all fields')
      return
    }

    // Require at least one environment ONLY if includeInPreview is false
    if (!includeInPreview && selectedEnvironments.length === 0) {
      toast.error('Please select at least one environment')
      return
    }

    await onSubmit({
      key,
      value,
      environments: selectedEnvironments,
      includeInPreview,
      isSecret,
    })
    setKey('')
    setValue('')
    setIsMultiline(false)
    setSelectedEnvironments([])
    setIncludeInPreview(false)
    setIsSecret(false)
  }

  return (
    <Dialog open={isOpen} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add Environment Variable</DialogTitle>
          <DialogDescription>
            Add a new environment variable to your project.
          </DialogDescription>
        </DialogHeader>
        <form
          onSubmit={(e) => {
            e.preventDefault()
            handleSubmit()
          }}
        >
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <label className="text-sm font-medium">Name</label>
              <Input
                placeholder="DATABASE_URL"
                value={key}
                onChange={(e) => setKey(e.target.value)}
                autoFocus
              />
            </div>
            <div className="space-y-2">
              <div className="flex items-center justify-between">
                <label className="text-sm font-medium">Value</label>
                <label className="flex items-center gap-2 text-xs text-muted-foreground">
                  <Checkbox
                    checked={isMultiline}
                    onCheckedChange={(checked) =>
                      setIsMultiline(checked === true)
                    }
                  />
                  Multiline (e.g. .npmrc)
                </label>
              </div>
              {isMultiline ? (
                <Textarea
                  placeholder="Enter multiline value"
                  value={value}
                  onChange={(e) => setValue(e.target.value)}
                  className="font-mono resize-y"
                  rows={6}
                />
              ) : (
                <Input
                  placeholder="Enter value"
                  value={value}
                  onChange={(e) => setValue(e.target.value)}
                  className="font-mono"
                />
              )}
            </div>
            <div className="space-y-2">
              <div className="flex items-center gap-2">
                <label className="text-sm font-medium">Environments</label>
                {includeInPreview && (
                  <span className="text-xs text-muted-foreground">
                    (Optional when including in preview)
                  </span>
                )}
              </div>
              <div className="flex flex-wrap gap-2">
                {allEnvironments.map((env) => (
                  <Button
                    type="button"
                    key={env.id}
                    disabled={Boolean(disabledReason)}
                    variant={
                      selectedEnvironments.includes(env.id)
                        ? 'default'
                        : 'outline'
                    }
                    size="sm"
                    onClick={() => {
                      setSelectedEnvironments((prev) =>
                        prev.includes(env.id)
                          ? prev.filter((e) => e !== env.id)
                          : [...prev, env.id]
                      )
                    }}
                  >
                    {env.name}
                  </Button>
                ))}
              </div>
            </div>
            <div className="flex items-center justify-between space-x-2 rounded-lg border p-4">
              <div className="flex-1 space-y-1">
                <Label
                  htmlFor="include-preview"
                  className="text-sm font-medium"
                >
                  Include in Preview Environments
                </Label>
                <p className="text-sm text-muted-foreground">
                  Also apply this variable to current and future preview
                  environments. Leave off to limit it to the selected
                  environments.
                </p>
              </div>
              <Switch
                id="include-preview"
                disabled={Boolean(disabledReason)}
                checked={includeInPreview}
                onCheckedChange={setIncludeInPreview}
              />
            </div>
            <p className="text-sm text-muted-foreground" role="status">
              {selectedEnvironments.length === 0
                ? 'No existing environments selected.'
                : `Selected existing environments: ${allEnvironments
                    .filter((env) => selectedEnvironments.includes(env.id))
                    .map((env) => env.name)
                    .join(', ')}.`}{' '}
              {includeInPreview
                ? 'Current and future preview environments are also included.'
                : 'Other and future preview environments are excluded.'}
            </p>
            <div className="flex items-center justify-between space-x-2 rounded-lg border p-4">
              <div className="flex-1 space-y-1">
                <Label htmlFor="is-secret" className="text-sm font-medium">
                  Secret
                </Label>
                <p className="text-sm text-muted-foreground">
                  Stored secret values cannot be viewed after saving. This
                  classification cannot be reverted; the value can still be
                  rotated.
                </p>
              </div>
              <Switch
                id="is-secret"
                checked={isSecret}
                onCheckedChange={setIsSecret}
              />
            </div>
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                onOpenChange(false)
                setKey('')
                setValue('')
                setIsMultiline(false)
                setSelectedEnvironments([])
                setIncludeInPreview(false)
                setIsSecret(false)
              }}
            >
              Cancel
            </Button>
            {disabledReason && (
              <p role="alert" className="text-sm text-muted-foreground">
                {disabledReason}
              </p>
            )}
            <Button type="submit" disabled={Boolean(disabledReason)}>
              Save Variable
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

interface EmptyPlaceholderProps extends React.HTMLAttributes<HTMLDivElement> {
  children: React.ReactNode
}

function EmptyPlaceholder({
  className,
  children,
  ...props
}: EmptyPlaceholderProps) {
  return (
    <div
      className={cn(
        'flex min-h-[400px] flex-col items-center justify-center rounded-md border border-dashed p-8 text-center animate-in fade-in-50',
        className
      )}
      {...props}
    >
      <div className="mx-auto flex max-w-[420px] flex-col items-center justify-center text-center">
        {children}
      </div>
    </div>
  )
}

EmptyPlaceholder.Icon = function EmptyPlaceholderIcon({
  className,
  children,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn(
        'flex h-20 w-20 items-center justify-center rounded-full bg-muted',
        className
      )}
      {...props}
    >
      {children}
    </div>
  )
}

EmptyPlaceholder.Title = function EmptyPlaceholderTitle({
  className,
  children,
  ...props
}: React.HTMLAttributes<HTMLHeadingElement>) {
  return (
    <h2 className={cn('mt-6 text-xl font-semibold', className)} {...props}>
      {children}
    </h2>
  )
}

EmptyPlaceholder.Description = function EmptyPlaceholderDescription({
  className,
  children,
  ...props
}: React.HTMLAttributes<HTMLParagraphElement>) {
  return (
    <p
      className={cn(
        'mb-8 mt-2 text-center text-sm font-normal leading-6 text-muted-foreground',
        className
      )}
      {...props}
    >
      {children}
    </p>
  )
}

function EnvironmentVariablesLoadingState() {
  return (
    <div className="space-y-6">
      <div>
        <div className="flex flex-row items-center justify-between mb-6">
          <div className="space-y-1.5">
            <Skeleton className="h-8 w-[230px]" />
            <Skeleton className="h-5 w-[450px]" />
          </div>
        </div>

        <div className="mt-6 space-y-6">
          {[...Array(3)].map((_, i) => (
            <div key={i} className="flex items-center justify-between py-4">
              <div className="space-y-2">
                <Skeleton className="h-5 w-[180px]" />
                <div className="flex gap-2">
                  <Skeleton className="h-6 w-20 rounded-full" />
                  <Skeleton className="h-6 w-20 rounded-full" />
                </div>
              </div>
              <div className="flex items-center gap-2">
                <Skeleton className="h-4 w-[120px]" />
                <div className="flex gap-2">
                  <Skeleton className="h-9 w-16" />
                  <Skeleton className="h-9 w-16" />
                  <Skeleton className="h-9 w-16" />
                </div>
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}

function DiscoveredEnvironmentVariableRow({
  variable,
}: {
  variable: DiscoveredEnvironmentVariable
}) {
  return (
    <TableRow className="border-b border-border/60">
      <TableCell />
      <TableCell>
        <p className="font-mono text-sm break-all">{variable.key}</p>
        {variable.description && (
          <p className="mt-1 text-sm text-muted-foreground max-w-xs">
            {variable.description}
          </p>
        )}
        <p className="mt-1 text-xs text-muted-foreground">
          {variable.sources.join(', ')}
        </p>
      </TableCell>
      <TableCell className="text-sm text-muted-foreground">
        Not configured
      </TableCell>
      <TableCell className="text-sm text-muted-foreground">—</TableCell>
      <TableCell>
        <EnvironmentVariableChecks
          checks={[
            {
              id: 'configuration',
              status: 'warning',
              label: 'Value missing',
              detail:
                'Declared in your repository but not configured in Temps.',
            },
          ]}
        />
      </TableCell>
      <TableCell className="text-right text-sm text-muted-foreground">
        Not added
      </TableCell>
    </TableRow>
  )
}

export function EnvironmentVariablesSettings({
  project,
}: EnvironmentVariablesSettingsProps) {
  const checksQuery = useHttpChecks(project.id)
  const navigate = useNavigate()
  const checksByVariable = useMemo(
    () => indicatorsBySubject(checksQuery.data ?? [], 'env_var'),
    [checksQuery.data]
  )
  const [isAddDialogOpen, setIsAddDialogOpen] = useState(false)
  const [isImportDialogOpen, setIsImportDialogOpen] = useState(false)

  const [selectedVariables, setSelectedVariables] = useState<Set<number>>(
    new Set()
  )
  const [isBulkDeleteDialogOpen, setIsBulkDeleteDialogOpen] = useState(false)
  const { get, patch } = useUrlState<'environment' | 'q'>()
  const search = get('q') ?? ''
  const environmentFilter = get('environment')
  const [revealAllScope, setRevealAllScope] = useState<string | null>(null)
  const [showComparison, setShowComparison] = useState(false)
  const [filtersOpen, setFiltersOpen] = useState(false)
  // Environment selector — when set, the resolved env-vars view shows the
  // values a deployment in that environment would actually receive
  // (per-tenant DB names like `<project>_<env>` for linked services).
  // `null` means "no specific environment" — falls back to the static
  // admin-level values for backward compatibility.

  const [comparisonFirstId, setComparisonFirstId] = useState<number | null>(
    null
  )
  const [comparisonSecondId, setComparisonSecondId] = useState<number | null>(
    null
  )

  const {
    data: projectEnvironments,
    isError: environmentsFailed,
    isPending: environmentsPending,
    refetch: refetchEnvironments,
  } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
    retry: false,
  })
  useKeyboardShortcut({
    key: 'n',
    callback: () => {
      if (projectEnvironments && !environmentsFailed) setIsAddDialogOpen(true)
    },
  })
  const orderedEnvironments = useMemo(
    () => orderEnvironments(projectEnvironments ?? []),
    [projectEnvironments]
  )
  const previewIds = useMemo(
    () =>
      new Set(
        orderedEnvironments
          .filter((environment) => environment.is_preview)
          .map((environment) => environment.id)
      ),
    [orderedEnvironments]
  )
  const comparisonFirst =
    orderedEnvironments.find((env) => env.id === comparisonFirstId) ??
    orderedEnvironments[0]
  const comparisonSecond =
    orderedEnvironments.find(
      (env) => env.id === comparisonSecondId && env.id !== comparisonFirst?.id
    ) ?? orderedEnvironments.find((env) => env.id !== comparisonFirst?.id)

  // Preserve an explicit URL scope while metadata is unavailable. Manual
  // bindings come with IDs; preview inheritance needs the environments response.
  const unavailableEnvironment =
    environmentFilter &&
    /^\d+$/.test(environmentFilter) &&
    !orderedEnvironments.some((env) => String(env.id) === environmentFilter)
      ? {
          id: Number(environmentFilter),
          is_preview: false,
          name: 'Selected environment',
        }
      : undefined
  const selectedEnvironment =
    environmentFilter === 'all'
      ? undefined
      : (orderedEnvironments.find(
          (env) => String(env.id) === environmentFilter
        ) ??
        unavailableEnvironment ??
        orderedEnvironments[0])
  const selectedEnvId = selectedEnvironment?.id ?? null
  const filterScope = `${project.id}:${selectedEnvId ?? 'all'}:${search}`
  const showAllValues = revealAllScope === filterScope

  const {
    data: envVariables,
    refetch,
    isLoading,
    isError: variablesFailed,
  } = useQuery({
    ...getEnvironmentVariablesOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const {
    data: resolvedEnvVars,
    isError: resolvedFailed,
    isPending: resolvedPending,
    refetch: refetchResolved,
  } = useQuery({
    queryKey: ['resolved-env-vars', project.id, selectedEnvId],
    queryFn: () => getResolvedEnvVars(project.id, selectedEnvId ?? undefined),
    staleTime: 15_000,
    enabled: Boolean(projectEnvironments) && !environmentsFailed,
  })

  const resolvedByKey = useMemo(
    () => indexResolvedByKey(resolvedEnvVars),
    [resolvedEnvVars]
  )

  const isDockerCompose = project.preset === 'docker-compose'
  const isPublicRepository = project.is_public_repo
  const publicProvider = publicRepositoryProvider(project.git_url)
  const publicRepository = parsePublicRepositoryUrl(project.git_url)
  const composeConfig =
    (project.preset_config as Record<string, unknown> | null) ?? {}
  const composePath =
    (composeConfig.composePath as string | undefined) ??
    (composeConfig.compose_path as string | undefined) ??
    'docker-compose.yml'
  const composeRepositoryPath = repositoryFilePath(
    project.directory,
    composePath
  )

  const { data: repositoryData } = useQuery({
    queryKey: [
      'environment-variable-repository',
      project.repo_owner,
      project.repo_name,
      project.git_provider_connection_id,
    ],
    queryFn: async () => {
      if (
        !project.repo_owner ||
        !project.repo_name ||
        !project.git_provider_connection_id
      ) {
        return null
      }
      const response = await listRepositoriesByConnection({
        path: { connection_id: project.git_provider_connection_id },
        query: { search: project.repo_name, per_page: 100 },
        throwOnError: true,
      })
      return (
        response.data?.repositories?.find(
          (repository) =>
            repository.owner === project.repo_owner &&
            repository.name === project.repo_name
        ) ?? null
      )
    },
    enabled:
      isDockerCompose &&
      !isPublicRepository &&
      !!project.repo_owner &&
      !!project.repo_name,
  })

  const connectedEnvExample = useQuery({
    ...getRepositoryEnvExampleLiveOptions({
      path: { repository_id: repositoryData?.id ?? 0 },
      query: {
        branch: project.main_branch,
        root_directory: project.directory || './',
      },
    }),
    enabled: isDockerCompose && !!repositoryData?.id,
  })
  const publicEnvExample = useQuery({
    ...detectPublicEnvExampleOptions({
      path: {
        provider: publicProvider,
        owner: project.repo_owner ?? '',
        repo: project.repo_name ?? '',
      },
      query: {
        branch: project.main_branch,
        root_directory: project.directory || './',
        base_url: publicRepository?.instanceUrl,
      },
    }),
    enabled:
      isDockerCompose &&
      isPublicRepository &&
      !!project.repo_owner &&
      !!project.repo_name,
  })
  const connectedComposeServices = useQuery({
    ...getRepositoryComposeServicesLiveOptions({
      path: { repository_id: repositoryData?.id ?? 0 },
      query: { branch: project.main_branch, path: composeRepositoryPath },
    }),
    enabled: isDockerCompose && !!repositoryData?.id,
  })
  const publicComposeServices = useQuery({
    ...getPublicComposeServicesOptions({
      path: {
        provider: publicProvider,
        owner: project.repo_owner ?? '',
        repo: project.repo_name ?? '',
      },
      query: {
        branch: project.main_branch,
        path: composeRepositoryPath,
        base_url: publicRepository?.instanceUrl,
      },
    }),
    enabled:
      isDockerCompose &&
      isPublicRepository &&
      !!project.repo_owner &&
      !!project.repo_name,
  })

  const integrationOnlyResolved = useMemo(() => {
    if (!resolvedEnvVars) return [] as ResolvedEnvVar[]
    const manualKeys = new Set(
      (envVariables ?? [])
        .filter((v) => variableAppliesToEnvironment(v, selectedEnvironment))
        .map((v) => v.key)
    )
    return resolvedEnvVars
      .filter(
        (entry) =>
          entry.source.type === 'integration' && !manualKeys.has(entry.key)
      )
      .sort((a, b) => a.key.localeCompare(b.key))
  }, [resolvedEnvVars, envVariables, selectedEnvironment])

  const createMutation = useMutation({
    ...createEnvironmentVariableMutation(),
    meta: {
      errorTitle: 'Failed to create environment variable',
    },
    onSuccess: () => {
      setIsAddDialogOpen(false)
      refetch()
      toast.success('Environment variable created')
    },
  })

  const handleCreateVariable = async (values: {
    key: string
    value: string
    environments: number[]
    includeInPreview: boolean
    isSecret: boolean
  }) => {
    await createMutation.mutateAsync({
      path: {
        project_id: project.id,
      },
      body: {
        key: values.key,
        value: values.value,
        environment_ids: values.environments,
        include_in_preview: values.includeInPreview,
        is_secret: values.isSecret,
      },
    })
  }

  const handleImportVariables = async (
    variables: { key: string; value: string; environments?: number[] }[]
  ) => {
    let successCount = 0
    let errorCount = 0

    for (const variable of variables) {
      try {
        await createMutation.mutateAsync({
          path: {
            project_id: project.id,
          },
          body: {
            key: variable.key,
            value: variable.value,
            environment_ids: variable.environments || [],
            include_in_preview: false,
          },
        })
        successCount++
      } catch {
        errorCount++
      }
    }

    if (successCount > 0) {
      toast.success(
        `Successfully imported ${successCount} variable${successCount !== 1 ? 's' : ''}`
      )
    }
    if (errorCount > 0) {
      toast.error(
        `Failed to import ${errorCount} variable${errorCount !== 1 ? 's' : ''}`
      )
    }

    refetch()
  }

  const existingKeys = useMemo(() => {
    return new Set((envVariables ?? []).map((v) => v.key))
  }, [envVariables])

  const discoveredMissingVariables = (() => {
    if (!isDockerCompose) return [] as DiscoveredEnvironmentVariable[]

    const configuredKeys = new Set(
      (envVariables ?? [])
        .filter((variable) =>
          variableAppliesToEnvironment(variable, selectedEnvironment)
        )
        .map((variable) => variable.key)
    )
    for (const resolved of environmentsFailed ? [] : (resolvedEnvVars ?? []))
      configuredKeys.add(resolved.key)

    const envExample = isPublicRepository
      ? publicEnvExample.data
      : connectedEnvExample.data
    const envExamplePath = envExample?.path ?? '.env.example'
    const envExampleVariables = (envExample?.variables ?? []).map(
      (variable) => {
        const raw = variable as {
          key: string
          description?: string | null
        }
        return {
          key: raw.key,
          description: raw.description,
        }
      }
    )

    const composeServices = isPublicRepository
      ? publicComposeServices.data?.services
      : connectedComposeServices.data?.services
    const serviceVariables = (composeServices ?? []).map((service) => {
      const raw = service as {
        name: string
        environmentVariables?: string[]
        environment_variables?: string[]
      }
      return {
        name: raw.name,
        environmentVariables:
          raw.environmentVariables ?? raw.environment_variables ?? [],
      }
    })

    return discoverComposeEnvironmentVariables({
      configuredKeys,
      envExamplePath,
      envExampleVariables,
      composePath: composeRepositoryPath,
      composeServices: serviceVariables,
    })
  })()

  const deleteMutation = useMutation({
    ...deleteEnvironmentVariableMutation(),
    meta: {
      errorTitle: 'Failed to delete environment variable',
    },
  })

  const matchesName = (variable: { key: string }) =>
    variable.key.toLowerCase().includes(search.trim().toLowerCase())
  const visibleVariables = (envVariables ?? []).filter(
    (variable) =>
      matchesName(variable) &&
      variableAppliesToEnvironment(variable, selectedEnvironment)
  )
  const visibleIntegrations = environmentsFailed
    ? []
    : integrationOnlyResolved.filter(matchesName)
  const visibleDiscovered = discoveredMissingVariables.filter(matchesName)
  const visibleSelectedIds = new Set(
    visibleVariables.filter((v) => selectedVariables.has(v.id)).map((v) => v.id)
  )

  // Reset before rendering a different filter scope, so stale selection or
  // plaintext cannot briefly appear in the newly filtered list.
  const [previousFilterScope, setPreviousFilterScope] = useState(filterScope)
  if (previousFilterScope !== filterScope) {
    setPreviousFilterScope(filterScope)
    setSelectedVariables(new Set())
    setIsBulkDeleteDialogOpen(false)
    setRevealAllScope(null)
  }

  const handleSelectVariable = (id: number) => {
    setSelectedVariables((prev) => {
      const newSet = new Set(prev)
      if (newSet.has(id)) {
        newSet.delete(id)
      } else {
        newSet.add(id)
      }
      return newSet
    })
  }

  const handleSelectAll = () => {
    if (visibleSelectedIds.size === visibleVariables.length) {
      setSelectedVariables(new Set())
    } else {
      setSelectedVariables(new Set(visibleVariables.map((v) => v.id)))
    }
  }

  const handleBulkDelete = async () => {
    let successCount = 0
    let errorCount = 0

    for (const varId of visibleSelectedIds) {
      try {
        await deleteMutation.mutateAsync({
          path: {
            project_id: project.id,
            var_id: varId,
          },
        })
        successCount++
      } catch {
        errorCount++
      }
    }

    if (successCount > 0) {
      toast.success(
        `Successfully deleted ${successCount} variable${successCount !== 1 ? 's' : ''}`
      )
    }
    if (errorCount > 0) {
      toast.error(
        `Failed to delete ${errorCount} variable${errorCount !== 1 ? 's' : ''}`
      )
    }

    setSelectedVariables(new Set())
    setIsBulkDeleteDialogOpen(false)
    refetch()
  }

  if (isLoading) {
    return <EnvironmentVariablesLoadingState />
  }

  if (variablesFailed)
    return (
      <div role="alert" className="space-y-3">
        <p>Could not load environment variables for {project.name}.</p>
        <Button onClick={() => void refetch()}>Retry variables</Button>
      </div>
    )

  const hasManualVariables = (envVariables?.length ?? 0) > 0
  const hasIntegrationVariables = visibleIntegrations.length > 0
  const hasDiscoveredVariables = discoveredMissingVariables.length > 0
  const hasVariables =
    hasManualVariables || hasIntegrationVariables || hasDiscoveredVariables
  const hasRevealableVariables =
    visibleVariables.some((variable) => !variable.is_secret) ||
    visibleIntegrations.length > 0
  const selectedCount = visibleSelectedIds.size
  const allSelected =
    selectedCount === visibleVariables.length && visibleVariables.length > 0
  const { missingInFirst, missingInSecond } =
    comparisonFirst && comparisonSecond
      ? compareEnvironmentVariableKeys(
          envVariables ?? [],
          comparisonFirst,
          comparisonSecond
        )
      : { missingInFirst: [], missingInSecond: [] }

  return (
    <div className="space-y-6">
      <div>
        <div className="flex flex-col gap-4 mb-5 sm:flex-row sm:items-start sm:justify-between">
          <div className="space-y-1.5">
            <h2 className="text-2xl font-semibold tracking-tight">
              Environment Variables
            </h2>
            <p className="text-sm text-muted-foreground">
              Manage values and automatic credential checks across environments.
            </p>
          </div>
          {(hasVariables ||
            orderedEnvironments.length > 0 ||
            Boolean(search)) && (
            <div className="flex flex-wrap items-center gap-2">
              <Button
                variant="outline"
                onClick={() => setFiltersOpen((open) => !open)}
                aria-expanded={filtersOpen}
                aria-controls="variable-filters"
                aria-label="Filters"
              >
                <ChevronDown
                  className={cn(
                    'size-4 mr-2 transition-transform',
                    filtersOpen && 'rotate-180'
                  )}
                />
                Filters
                <span className="max-w-32 truncate text-muted-foreground">
                  {selectedEnvironment?.name ?? 'All'}
                  {search.trim() ? ' · 1 search' : ''}
                </span>
              </Button>
              <Button
                variant="outline"
                disabled={environmentsFailed || environmentsPending}
                onClick={() => setIsImportDialogOpen(true)}
              >
                <Upload className="h-4 w-4 mr-2" />
                Import .env
              </Button>
              <Button
                disabled={environmentsFailed || environmentsPending}
                onClick={() => setIsAddDialogOpen(true)}
                className="flex-1 sm:flex-initial"
              >
                <Plus className="h-4 w-4 mr-2" />
                Add Variable
                <KbdBadge keys={['N']} className="ml-2 hidden sm:inline-flex" />
              </Button>
            </div>
          )}
        </div>
        {(hasVariables ||
          orderedEnvironments.length > 0 ||
          Boolean(search)) && (
          <Collapsible open={filtersOpen} onOpenChange={setFiltersOpen}>
            <CollapsibleContent id="variable-filters">
              <div className="flex flex-wrap items-center gap-3 border-b pb-3">
                {projectEnvironments && projectEnvironments.length > 0 ? (
                  <div className="flex flex-wrap items-center gap-2">
                    <Label
                      htmlFor="env-preview-select"
                      className="text-xs text-muted-foreground"
                    >
                      Environment
                    </Label>
                    <Select
                      disabled={environmentsFailed || environmentsPending}
                      value={
                        selectedEnvId !== null ? String(selectedEnvId) : 'all'
                      }
                      onValueChange={(environment) =>
                        patch({ environment }, { replace: false })
                      }
                    >
                      <SelectTrigger
                        id="env-preview-select"
                        className="h-8 w-[180px] text-sm"
                      >
                        <SelectValue placeholder="Select environment" />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="all">All environments</SelectItem>
                        {orderedEnvironments.map((env) => (
                          <SelectItem key={env.id} value={String(env.id)}>
                            {env.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                ) : null}
                <Input
                  aria-label="Filter environment variables by name"
                  placeholder="Filter by variable name…"
                  value={search}
                  onChange={(event) => patch({ q: event.target.value })}
                  className="w-full sm:w-72"
                />
              </div>
            </CollapsibleContent>
          </Collapsible>
        )}

        {environmentsFailed && (
          <div
            role="alert"
            className="flex flex-wrap items-center gap-2 py-3 text-sm"
          >
            <p>
              Could not load environments for {project.name}. You can still
              view, edit values, and delete saved variables. Retry to add
              variables or change environments.
              {unavailableEnvironment &&
                ' Showing explicit bindings only; preview inheritance is unavailable.'}
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void refetchEnvironments()}
            >
              Retry environments
            </Button>
          </div>
        )}
        {resolvedFailed && !environmentsFailed && (
          <div role="alert" className="py-3 text-sm">
            Could not load service variables for this environment.{' '}
            <Button variant="link" onClick={() => void refetchResolved()}>
              Retry service variables
            </Button>
          </div>
        )}
        <div className="mt-2">
          {!hasVariables &&
          !environmentsFailed &&
          (environmentsPending || resolvedPending) ? (
            <p role="status">Loading service variables…</p>
          ) : !hasVariables && resolvedFailed ? null : !hasVariables ? (
            <EmptyPlaceholder>
              <EmptyPlaceholder.Icon>
                <KeyRound className="h-6 w-6" />
              </EmptyPlaceholder.Icon>
              <EmptyPlaceholder.Title>
                No environment variables
              </EmptyPlaceholder.Title>
              <EmptyPlaceholder.Description>
                Add environment variables to configure your project across
                different environments.
              </EmptyPlaceholder.Description>
              <div className="flex gap-2">
                <Button
                  variant="outline"
                  disabled={environmentsFailed || environmentsPending}
                  onClick={() => setIsImportDialogOpen(true)}
                >
                  <Upload className="h-4 w-4 mr-2" />
                  Import .env File
                </Button>
                <Button
                  disabled={environmentsFailed || environmentsPending}
                  onClick={() => setIsAddDialogOpen(true)}
                >
                  <Plus className="h-4 w-4 mr-2" />
                  Add Variable
                  <KbdBadge keys={['N']} className="ml-2" />
                </Button>
              </div>
            </EmptyPlaceholder>
          ) : (
            <>
              <div className="w-full min-w-0">
                <Table
                  className="w-full min-w-[900px] text-sm"
                  aria-label="Environment variables"
                >
                  <TableHeader>
                    <TableRow className="text-left">
                      <TableHead scope="col" className="w-8">
                        <Checkbox
                          checked={
                            allSelected
                              ? true
                              : selectedCount > 0
                                ? 'indeterminate'
                                : false
                          }
                          disabled={visibleVariables.length === 0}
                          onCheckedChange={handleSelectAll}
                          aria-label={
                            allSelected
                              ? 'Deselect all environment variables'
                              : 'Select all environment variables'
                          }
                        />
                      </TableHead>
                      <TableHead scope="col" className="w-[28%]">
                        Variable{' '}
                        {selectedCount > 0 && (
                          <span className="ml-2 text-xs text-muted-foreground">
                            {selectedCount} selected
                          </span>
                        )}
                      </TableHead>
                      <TableHead scope="col" className="w-[22%]">
                        <div className="flex items-center gap-2">
                          Value
                          {hasRevealableVariables && (
                            <Button
                              variant="ghost"
                              size="sm"
                              onClick={() =>
                                setRevealAllScope(
                                  showAllValues ? null : filterScope
                                )
                              }
                              aria-label={
                                showAllValues
                                  ? 'Hide all values'
                                  : 'Show all values'
                              }
                              title={
                                showAllValues
                                  ? 'Hide all values'
                                  : 'Show all values'
                              }
                            >
                              {showAllValues ? (
                                <EyeOff className="h-4 w-4" />
                              ) : (
                                <Eye className="h-4 w-4" />
                              )}
                            </Button>
                          )}
                        </div>
                      </TableHead>
                      <TableHead scope="col">Environments</TableHead>
                      <TableHead scope="col">Checks</TableHead>
                      <TableHead scope="col" className="text-right">
                        {selectedCount > 0 ? (
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-destructive"
                            onClick={() => setIsBulkDeleteDialogOpen(true)}
                          >
                            Delete {selectedCount} selected
                          </Button>
                        ) : (
                          'Actions'
                        )}
                      </TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {visibleVariables.length +
                      visibleIntegrations.length +
                      visibleDiscovered.length ===
                      0 && (
                      <TableRow>
                        <TableCell
                          colSpan={6}
                          className="py-8 text-center text-muted-foreground"
                        >
                          {resolvedPending
                            ? 'Loading service variables…'
                            : 'No variables match this environment and search.'}
                          <Button
                            variant="link"
                            onClick={() =>
                              patch({ q: null, environment: 'all' })
                            }
                          >
                            Clear filters
                          </Button>
                        </TableCell>
                      </TableRow>
                    )}
                    {visibleVariables.map((variable) => (
                      <EnvironmentVariableRow
                        key={`${filterScope}:${variable.id}`}
                        variable={variable}
                        project={project}
                        refetchEnvVariables={() => refetch()}
                        isSelected={selectedVariables.has(variable.id)}
                        onSelect={handleSelectVariable}
                        showAllValues={showAllValues}
                        resolved={resolvedByKey.get(variable.key)}
                        checks={
                          checksQuery.isError
                            ? [
                                {
                                  id: 'load',
                                  status: 'unknown',
                                  label: 'Checks unavailable',
                                  detail:
                                    'Could not load check results. Open variable details to retry.',
                                },
                              ]
                            : checksQuery.isPending
                              ? [
                                  {
                                    id: 'load',
                                    status: 'pending',
                                    label: 'Loading checks',
                                    detail: 'Loading configured checks.',
                                  },
                                ]
                              : (checksByVariable.get(variable.id) ?? [])
                        }
                        onManageChecks={() =>
                          navigate(
                            `/projects/${project.slug}/environment-variables/${variable.id}`
                          )
                        }
                        previewIds={previewIds}
                        allEnvironments={projectEnvironments ?? []}
                        environmentChoicesAvailable={
                          !environmentsFailed && !environmentsPending
                        }
                      />
                    ))}
                    {visibleIntegrations.map((entry) => (
                      <IntegrationEnvVarRow
                        key={`${filterScope}:integration-${entry.key}`}
                        projectId={project.id}
                        resolved={entry}
                        showAllValues={showAllValues}
                        environmentId={selectedEnvId}
                        previewIds={previewIds}
                      />
                    ))}
                    {visibleDiscovered.map((variable) => (
                      <DiscoveredEnvironmentVariableRow
                        key={`discovered-${variable.key}`}
                        variable={variable}
                      />
                    ))}
                  </TableBody>
                </Table>
              </div>
              {hasManualVariables && comparisonFirst && comparisonSecond && (
                <section
                  className="mt-4 rounded-lg border border-border/70 bg-card"
                  aria-label="Compare environment variables"
                >
                  <button
                    type="button"
                    className="flex w-full items-center justify-between gap-3 px-4 py-3 text-left text-sm font-medium hover:bg-muted/40"
                    onClick={() => setShowComparison((current) => !current)}
                    aria-expanded={showComparison}
                    aria-controls="environment-variable-comparison"
                  >
                    <span>Compare environments</span>
                    <ChevronDown
                      className={cn(
                        'size-4 shrink-0 text-muted-foreground transition-transform',
                        showComparison && 'rotate-180'
                      )}
                    />
                  </button>
                  <div
                    id="environment-variable-comparison"
                    hidden={!showComparison}
                    className="border-t p-4"
                  >
                    <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                      <div>
                        <p className="mt-1 text-xs text-muted-foreground">
                          Keys present in one environment but missing from the
                          other. Preview inheritance is included.
                        </p>
                      </div>
                      <div className="flex flex-col gap-2 sm:flex-row">
                        <Select
                          value={String(comparisonFirst.id)}
                          onValueChange={(value) =>
                            setComparisonFirstId(Number(value))
                          }
                        >
                          <SelectTrigger
                            className="w-full sm:w-[180px]"
                            aria-label="First comparison environment"
                          >
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            {orderedEnvironments.map((env) => (
                              <SelectItem key={env.id} value={String(env.id)}>
                                {env.name}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                        <Select
                          value={String(comparisonSecond.id)}
                          onValueChange={(value) =>
                            setComparisonSecondId(Number(value))
                          }
                        >
                          <SelectTrigger
                            className="w-full sm:w-[180px]"
                            aria-label="Second comparison environment"
                          >
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            {orderedEnvironments.map((env) => (
                              <SelectItem key={env.id} value={String(env.id)}>
                                {env.name}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                      </div>
                    </div>
                    <div className="mt-4 grid gap-3 sm:grid-cols-2">
                      {[
                        { environment: comparisonFirst, keys: missingInFirst },
                        {
                          environment: comparisonSecond,
                          keys: missingInSecond,
                        },
                      ].map(({ environment, keys }) => (
                        <div
                          key={environment.id}
                          className="rounded-md border p-3"
                        >
                          <div className="flex items-center justify-between gap-2">
                            <h4 className="text-sm font-medium truncate">
                              {environment.name}
                            </h4>
                            <span className="shrink-0 text-xs text-muted-foreground">
                              {keys.length} missing
                            </span>
                          </div>
                          {keys.length === 0 ? (
                            <p className="mt-2 text-xs text-emerald-600 dark:text-emerald-400">
                              No keys missing relative to{' '}
                              {environment.id === comparisonFirst.id
                                ? comparisonSecond.name
                                : comparisonFirst.name}
                            </p>
                          ) : (
                            <ul className="mt-2 max-h-32 space-y-1 overflow-y-auto text-xs font-mono">
                              {keys.map((key) => (
                                <li key={key} className="break-all">
                                  {key}
                                </li>
                              ))}
                            </ul>
                          )}
                        </div>
                      ))}
                    </div>
                  </div>
                </section>
              )}
            </>
          )}
        </div>
      </div>

      <AddEnvironmentVariableDialog
        isOpen={isAddDialogOpen}
        onOpenChange={setIsAddDialogOpen}
        onSubmit={handleCreateVariable}
        allEnvironments={projectEnvironments ?? []}
        disabledReason={
          environmentsFailed || environmentsPending
            ? 'Environment choices are unavailable. Retry environments before adding variables.'
            : undefined
        }
      />
      <ImportEnvDialog
        isOpen={isImportDialogOpen}
        onOpenChange={setIsImportDialogOpen}
        onImport={handleImportVariables}
        allEnvironments={projectEnvironments ?? []}
        existingKeys={existingKeys}
        disabledReason={
          environmentsFailed || environmentsPending
            ? 'Environment choices are unavailable. Retry environments before importing variables.'
            : undefined
        }
      />

      <AlertDialog
        open={isBulkDeleteDialogOpen}
        onOpenChange={setIsBulkDeleteDialogOpen}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete Multiple Variables</AlertDialogTitle>
            <AlertDialogDescription className="space-y-3">
              <p>
                Are you sure you want to delete {selectedCount} environment
                variable{selectedCount !== 1 ? 's' : ''}? This action cannot be
                undone.
              </p>
              {selectedCount > 0 && (
                <div className="space-y-2">
                  <p className="text-sm font-medium text-foreground">
                    Variables to be deleted:
                  </p>
                  <div className="max-h-[200px] overflow-auto border rounded-md p-3 space-y-1">
                    {(envVariables ?? [])
                      .filter((v) => visibleSelectedIds.has(v.id))
                      .map((v) => (
                        <div
                          key={v.id}
                          className="text-sm font-mono flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between"
                        >
                          <span className="break-all">{v.key}</span>
                          <div className="flex flex-wrap gap-1">
                            {v.environments.map((env) => (
                              <span
                                key={env.name}
                                className="inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium bg-secondary text-secondary-foreground"
                              >
                                {env.name}
                              </span>
                            ))}
                          </div>
                        </div>
                      ))}
                  </div>
                </div>
              )}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={handleBulkDelete}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              Delete {selectedCount} Variable{selectedCount !== 1 ? 's' : ''}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
