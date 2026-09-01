// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  EnvironmentVariableResponse,
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
import { Eye, EyeOff, KeyRound, Plus, Upload } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import { Skeleton } from '@/components/ui/skeleton'
import { Checkbox } from '@/components/ui/checkbox'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { ImportEnvDialog } from '@/components/ui/import-env-dialog'
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
import { Link } from 'react-router'
import {
  parsePublicRepositoryUrl,
  publicRepositoryProvider,
} from '@/lib/public-repository'
import {
  discoverComposeEnvironmentVariables,
  type DiscoveredEnvironmentVariable,
} from '@/lib/compose-environment-discovery'
import { repositoryFilePath } from '@/lib/repository-file-path'

interface EnvironmentVariableRowProps {
  variable: EnvironmentVariableResponse
  project: ProjectResponse
  refetchEnvVariables: () => void
  isSelected: boolean
  onSelect: (id: number) => void
  showAllValues: boolean
  resolved?: ResolvedEnvVar
}

function EnvironmentVariableRow({
  variable,
  project,
  refetchEnvVariables,
  isSelected,
  onSelect,
  showAllValues,
  resolved,
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
  // Secret env vars are write-only: the value is never fetched, the reveal
  // button is hidden, and the edit dialog defaults to "leave blank to keep
  // the existing value". This mirrors the file-based Secrets UX.
  const isSecret = variable.is_secret ?? false

  useEffect(() => {
    const guard = createCredentialRevealGuard()
    revealGuard.current = guard
    // Drop plaintext whenever this row changes project, identity, or version.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setRevealedValue(undefined)
    // eslint-disable-next-line react-hooks/set-state-in-effect
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
    if (isSecret) return
    revealGuard.current.cancel('value')
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
  // when the reveal was denied (it needs SecretsRead on top of EnvironmentsWrite)
  // or failed, which is what distinguishes "cleared on purpose" from "never
  // loaded" when the box is empty on save.
  const [valueLoaded, setValueLoaded] = useState(false)
  // Opt-in conversion of an existing plain variable into a write-only secret.
  // One-way: once saved, the value can never be read back through the UI or the
  // API, so it stays off unless the operator explicitly turns it on.
  const [convertToSecret, setConvertToSecret] = useState(false)

  // Update selected environments and preview flag when variable changes (after refetch)
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSelectedEditEnvironments(variable.environments.map((env) => env.id))
    // eslint-disable-next-line react-hooks/set-state-in-effect
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
        environment_ids: selectedEditEnvironments,
        key: variable.key,
        include_in_preview: editIncludeInPreview,
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

  const { data: allEnvironments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  return (
    <>
      <div className="py-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
        <div className="flex items-start gap-3 flex-1 min-w-0">
          <Checkbox
            checked={isSelected}
            onCheckedChange={() => onSelect(variable.id)}
            className="mt-1 sm:mt-0"
          />
          <div className="space-y-1 flex-1 min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              {overridesService && (
                <IntegrationBadge service={overridesService} overridden />
              )}
              <p className="font-medium break-all">{variable.key}</p>
              {isSecret && (
                <span
                  title="Write-only secret — value is never returned by the API"
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
            <div className="flex flex-wrap gap-2">
              {variable.environments.map((env) => (
                <span
                  key={env.name}
                  className="inline-flex items-center rounded-full px-2 py-1 text-xs font-medium bg-secondary text-secondary-foreground"
                >
                  {env.name}
                </span>
              ))}
              {variable.include_in_preview && (
                <span className="inline-flex items-center rounded-full px-2 py-1 text-xs font-medium bg-blue-500/10 text-blue-700 dark:text-blue-400 border border-blue-500/20">
                  Preview
                </span>
              )}
            </div>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2 pl-7 sm:pl-0">
          <div className="flex items-center gap-2 min-w-0 w-full sm:w-auto">
            <span className="font-mono text-sm truncate max-w-[180px] sm:max-w-[220px]">
              {isSecret
                ? '••••••••••••'
                : isVisible
                  ? isRevealing && !dataValue
                    ? 'Revealing…'
                    : dataValue || '••••••••••••'
                  : '••••••••••••'}
            </span>
            {!isSecret && (
              <Button variant="ghost" size="sm" onClick={toggleVisibility}>
                {isVisible ? (
                  <EyeOff className="h-4 w-4" />
                ) : (
                  <Eye className="h-4 w-4" />
                )}
              </Button>
            )}
          </div>
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
                variant="destructive"
                size="sm"
                disabled={deleteMutation.isPending || updateMutation.isPending}
              >
                Delete
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete environment variable</AlertDialogTitle>
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
      </div>

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
                    This variable is a write-only secret. Leave the value blank
                    to change only environments or preview settings.
                  </p>
                )}
              </div>
              <div className="space-y-2">
                <label className="text-sm font-medium">Environments</label>
                <div className="flex flex-wrap gap-2">
                  {(allEnvironments ?? []).map((env) => (
                    <Button
                      type="button"
                      key={env.id}
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
                        ? `On save, ${variable.key} becomes write-only: the value is masked in the UI and no longer returned by the API. You can still overwrite it, but never read it back — to make it a regular variable again you must delete it and create it anew.`
                        : 'Make this variable write-only so its value can never be read from the UI or the API again. One-way: converting back means deleting and recreating the variable.'}
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
}

function IntegrationEnvVarRow({
  projectId,
  resolved,
  showAllValues,
  environmentId,
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
    const request = revealGuard.current.begin('value')
    setIsFetching(true)
    try {
      const value = await getResolvedEnvVarValue(
        projectId,
        resolved.key,
        environmentId ?? undefined,
        serviceId === 'manual' ? undefined : serviceId
      )
      if (!revealGuard.current.isCurrent('value', request)) return
      setRevealedValue({ value, scope: revealScope })
    } catch {
      if (revealGuard.current.isCurrent('value', request)) {
        toast.error(`Failed to reveal ${resolved.key}`)
      }
    } finally {
      if (revealGuard.current.finish('value', request)) {
        setIsFetching(false)
      }
    }
  }

  useEffect(() => {
    revealGuard.current.cancel('value')
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
    <div className="py-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
      <div className="flex items-start gap-3 flex-1 min-w-0">
        <div className="hidden sm:block w-4 shrink-0" aria-hidden />
        <div className="space-y-1 flex-1 min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <IntegrationBadge service={service} />
            <p className="font-medium break-all">{resolved.key}</p>
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
          <div className="flex gap-2 flex-wrap">
            {resolved.environments.map((env) => (
              <span
                key={env.name}
                className="inline-flex items-center rounded-full px-2 py-1 text-xs font-medium bg-secondary text-secondary-foreground"
              >
                {env.name}
              </span>
            ))}
            {resolved.include_in_preview && (
              <span className="inline-flex items-center rounded-full px-2 py-1 text-xs font-medium bg-blue-500/10 text-blue-700 dark:text-blue-400 border border-blue-500/20">
                Preview
              </span>
            )}
          </div>
        </div>
      </div>
      <div className="flex items-center gap-2 min-w-0">
        <span className="font-mono text-sm text-muted-foreground truncate max-w-[200px] sm:max-w-[240px]">
          {valueText}
        </span>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => void toggleVisibility()}
          aria-label={isVisible ? 'Hide value' : 'Reveal value'}
        >
          {isVisible ? (
            <EyeOff className="h-4 w-4" />
          ) : (
            <Eye className="h-4 w-4" />
          )}
        </Button>
      </div>
    </div>
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
  allEnvironments: any[]
}

function AddEnvironmentVariableDialog({
  isOpen,
  onOpenChange,
  onSubmit,
  allEnvironments,
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
        setSelectedEnvironments(allEnvironments.map((env) => env.id))
        setHasInitialized(true)
      }
    } else if (!isOpen) {
      // Reset initialization flag when dialog closes
      setHasInitialized(false)
    }
  }, [isOpen, allEnvironments, hasInitialized])

  const handleSubmit = async () => {
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
                  Automatically add this variable to preview environments
                </p>
              </div>
              <Switch
                id="include-preview"
                checked={includeInPreview}
                onCheckedChange={setIncludeInPreview}
              />
            </div>
            <div className="flex items-center justify-between space-x-2 rounded-lg border p-4">
              <div className="flex-1 space-y-1">
                <Label htmlFor="is-secret" className="text-sm font-medium">
                  Secret (write-only)
                </Label>
                <p className="text-sm text-muted-foreground">
                  Mask the value in the UI and never return it from the API.
                  Once enabled this cannot be reverted — only the value can be
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
            <Button type="submit">Save Variable</Button>
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
    <div className="py-4 flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
      <div className="space-y-1 min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <p className="font-medium font-mono break-all">{variable.key}</p>
          <span className="inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide bg-amber-500/10 text-amber-700 dark:text-amber-400 border border-amber-500/20">
            Not added
          </span>
        </div>
        {variable.description ? (
          <p className="text-xs text-muted-foreground">
            {variable.description}
          </p>
        ) : null}
        <div className="flex flex-wrap gap-1.5">
          {variable.sources.map((source) => (
            <span
              key={source}
              className="inline-flex items-center rounded-full px-2 py-1 text-xs font-medium bg-secondary text-secondary-foreground"
            >
              {source}
            </span>
          ))}
        </div>
      </div>
      <span className="font-mono text-xs text-muted-foreground break-all sm:max-w-[320px]">
        Value required
      </span>
    </div>
  )
}

export function EnvironmentVariablesSettings({
  project,
}: EnvironmentVariablesSettingsProps) {
  const [isAddDialogOpen, setIsAddDialogOpen] = useState(false)
  const [isImportDialogOpen, setIsImportDialogOpen] = useState(false)
  const [selectedVariables, setSelectedVariables] = useState<Set<number>>(
    new Set()
  )
  const [isBulkDeleteDialogOpen, setIsBulkDeleteDialogOpen] = useState(false)
  const [showAllValues, setShowAllValues] = useState(false)
  // Environment selector — when set, the resolved env-vars view shows the
  // values a deployment in that environment would actually receive
  // (per-tenant DB names like `<project>_<env>` for linked services).
  // `null` means "no specific environment" — falls back to the static
  // admin-level values for backward compatibility.
  const [selectedEnvId, setSelectedEnvId] = useState<number | null>(null)

  const { data: projectEnvironments } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
  })

  // Default to the production environment (or first available) once the
  // env list loads, so the preview is never blank.
  useEffect(() => {
    if (selectedEnvId !== null) return
    const envs = projectEnvironments
    if (!envs || envs.length === 0) return
    const prod = envs.find((e: any) => e.name === 'production') ?? envs[0]
    if (prod) setSelectedEnvId(prod.id)
  }, [projectEnvironments, selectedEnvId])

  const {
    data: envVariables,
    refetch,
    isLoading,
  } = useQuery({
    ...getEnvironmentVariablesOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const { data: resolvedEnvVars } = useQuery({
    queryKey: ['resolved-env-vars', project.id, selectedEnvId],
    queryFn: () => getResolvedEnvVars(project.id, selectedEnvId ?? undefined),
    staleTime: 15_000,
    enabled: selectedEnvId !== null,
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
    const manualKeys = new Set((envVariables ?? []).map((v) => v.key))
    return resolvedEnvVars
      .filter(
        (entry) =>
          entry.source.type === 'integration' && !manualKeys.has(entry.key)
      )
      .sort((a, b) => a.key.localeCompare(b.key))
  }, [resolvedEnvVars, envVariables])

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

    const configuredKeys = new Set(existingKeys)
    for (const resolved of resolvedEnvVars ?? [])
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

  const { data: allEnvironments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const deleteMutation = useMutation({
    ...deleteEnvironmentVariableMutation(),
    meta: {
      errorTitle: 'Failed to delete environment variable',
    },
  })

  // Keyboard shortcut to add new variable (N key)
  // IMPORTANT: This useEffect must be called BEFORE any early returns to follow React's Rules of Hooks
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Check if the key is 'N' and no input/textarea is focused
      if (
        e.key === 'n' &&
        !e.metaKey &&
        !e.ctrlKey &&
        !e.shiftKey &&
        !e.altKey
      ) {
        const target = e.target as HTMLElement
        // Only trigger if not typing in an input/textarea
        if (
          target.tagName !== 'INPUT' &&
          target.tagName !== 'TEXTAREA' &&
          !target.isContentEditable
        ) {
          e.preventDefault()
          setIsAddDialogOpen(true)
        }
      }
    }

    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [])

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
    if (selectedVariables.size === (envVariables?.length ?? 0)) {
      setSelectedVariables(new Set())
    } else {
      setSelectedVariables(new Set((envVariables ?? []).map((v) => v.id)))
    }
  }

  const handleBulkDelete = async () => {
    let successCount = 0
    let errorCount = 0

    for (const varId of selectedVariables) {
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

  const hasManualVariables = (envVariables?.length ?? 0) > 0
  const hasIntegrationVariables = integrationOnlyResolved.length > 0
  const hasDiscoveredVariables = discoveredMissingVariables.length > 0
  const hasVariables =
    hasManualVariables || hasIntegrationVariables || hasDiscoveredVariables
  const selectedCount = selectedVariables.size
  const allSelected =
    selectedCount === (envVariables?.length ?? 0) && hasManualVariables

  return (
    <div className="space-y-6">
      <div>
        <div className="flex flex-col gap-4 mb-6 lg:flex-row lg:items-center lg:justify-between">
          <div className="space-y-1.5">
            <h2 className="text-2xl font-semibold tracking-tight">
              Environment Variables
            </h2>
            <p className="text-base/6 sm:text-sm text-muted-foreground">
              Manage your project&apos;s environment variables across different
              environments.
            </p>
            {projectEnvironments && projectEnvironments.length > 0 ? (
              <div className="flex flex-wrap items-center gap-2 pt-2">
                <Label
                  htmlFor="env-preview-select"
                  className="text-xs text-muted-foreground"
                >
                  Preview values for
                </Label>
                <Select
                  value={selectedEnvId !== null ? String(selectedEnvId) : ''}
                  onValueChange={(v) => setSelectedEnvId(Number(v))}
                >
                  <SelectTrigger
                    id="env-preview-select"
                    className="h-8 w-[180px] text-sm"
                  >
                    <SelectValue placeholder="Select environment" />
                  </SelectTrigger>
                  <SelectContent>
                    {projectEnvironments.map((env: any) => (
                      <SelectItem key={env.id} value={String(env.id)}>
                        {env.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <span className="text-[11px] text-muted-foreground">
                  Linked services show{' '}
                  <code className="font-mono">{`<project>_<env>`}</code> values.
                </span>
              </div>
            ) : null}
          </div>
          {hasVariables && (
            <div className="flex flex-wrap gap-2">
              {selectedCount > 0 && (
                <Button
                  variant="destructive"
                  onClick={() => setIsBulkDeleteDialogOpen(true)}
                  className="flex-1 sm:flex-initial"
                >
                  Delete {selectedCount} Variable
                  {selectedCount !== 1 ? 's' : ''}
                </Button>
              )}
              <Button
                variant="outline"
                onClick={() => setShowAllValues(!showAllValues)}
                title={showAllValues ? 'Hide all values' : 'Show all values'}
              >
                {showAllValues ? (
                  <>
                    <EyeOff className="h-4 w-4 sm:mr-2" />
                    <span className="hidden sm:inline">Hide all</span>
                  </>
                ) : (
                  <>
                    <Eye className="h-4 w-4 sm:mr-2" />
                    <span className="hidden sm:inline">Show all</span>
                  </>
                )}
              </Button>
              <Button
                variant="outline"
                onClick={() => setIsImportDialogOpen(true)}
              >
                <Upload className="h-4 w-4 sm:mr-2" />
                <span className="hidden sm:inline">Import .env</span>
              </Button>
              <Button
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

        <div className="mt-6">
          {!hasVariables ? (
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
                  onClick={() => setIsImportDialogOpen(true)}
                >
                  <Upload className="h-4 w-4 mr-2" />
                  Import .env File
                </Button>
                <Button onClick={() => setIsAddDialogOpen(true)}>
                  <Plus className="h-4 w-4 mr-2" />
                  Add Variable
                  <KbdBadge keys={['N']} className="ml-2" />
                </Button>
              </div>
            </EmptyPlaceholder>
          ) : (
            <>
              {hasManualVariables && (
                <div className="flex items-center gap-3 py-3 border-b">
                  <Checkbox
                    checked={allSelected}
                    onCheckedChange={handleSelectAll}
                  />
                  <span className="text-sm font-medium">
                    {selectedCount > 0
                      ? `${selectedCount} of ${envVariables?.length ?? 0} selected`
                      : 'Select all'}
                  </span>
                </div>
              )}
              <div className="divide-y divide-border">
                {(envVariables ?? []).map((variable) => (
                  <EnvironmentVariableRow
                    key={variable.id}
                    variable={variable}
                    project={project}
                    refetchEnvVariables={() => refetch()}
                    isSelected={selectedVariables.has(variable.id)}
                    onSelect={handleSelectVariable}
                    showAllValues={showAllValues}
                    resolved={resolvedByKey.get(variable.key)}
                  />
                ))}
                {integrationOnlyResolved.map((entry) => (
                  <IntegrationEnvVarRow
                    key={`integration-${entry.key}`}
                    projectId={project.id}
                    resolved={entry}
                    showAllValues={showAllValues}
                    environmentId={selectedEnvId}
                  />
                ))}
                {discoveredMissingVariables.map((variable) => (
                  <DiscoveredEnvironmentVariableRow
                    key={`discovered-${variable.key}`}
                    variable={variable}
                  />
                ))}
              </div>
            </>
          )}
        </div>
      </div>

      <AddEnvironmentVariableDialog
        isOpen={isAddDialogOpen}
        onOpenChange={setIsAddDialogOpen}
        onSubmit={handleCreateVariable}
        allEnvironments={allEnvironments ?? []}
      />
      <ImportEnvDialog
        isOpen={isImportDialogOpen}
        onOpenChange={setIsImportDialogOpen}
        onImport={handleImportVariables}
        allEnvironments={allEnvironments ?? []}
        existingKeys={existingKeys}
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
                      .filter((v) => selectedVariables.has(v.id))
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
