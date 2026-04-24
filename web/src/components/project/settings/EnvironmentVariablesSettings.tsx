import { EnvironmentVariableResponse, ProjectResponse } from '@/api/client'
import {
  createEnvironmentVariableMutation,
  deleteEnvironmentVariableMutation,
  getEnvironmentsOptions,
  getEnvironmentVariablesOptions,
  getEnvironmentVariableValueOptions,
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
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Skeleton } from '@/components/ui/skeleton'
import { Checkbox } from '@/components/ui/checkbox'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { ImportEnvDialog } from '@/components/ui/import-env-dialog'
import { Switch } from '@/components/ui/switch'
import { Label } from '@/components/ui/label'
import {
  getResolvedEnvVars,
  getResolvedEnvVarValue,
  indexResolvedByKey,
  type ResolvedEnvVar,
} from '@/lib/resolved-env-vars'
import { IntegrationBadge } from './IntegrationBadge'
import { Link } from 'react-router-dom'

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
  const [isEditing, setIsEditing] = useState(false)
  const [editValue, setEditValue] = useState('')
  const [isEditMultiline, setIsEditMultiline] = useState(false)

  const { data, refetch } = useQuery({
    ...getEnvironmentVariableValueOptions({
      path: {
        project_id: project.id,
        key: variable.key,
      },
    }),
    enabled: isVisible || isEditing || showAllValues,
  })

  useEffect(() => {
    if (data && typeof data === 'object' && 'value' in data) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setEditValue(data.value)
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setIsEditMultiline(data.value.includes('\n'))
    }
  }, [data])

  useEffect(() => {
    setIsVisible(showAllValues)
    if (showAllValues) {
      refetch()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showAllValues])

  const dataValue = useMemo(() => data?.value ?? '', [data])

  const toggleVisibility = async () => {
    setIsVisible(!isVisible)
    if (!isVisible) {
      refetch()
    }
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
      setIsEditing(false)
      refetch()
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

  // Update selected environments and preview flag when variable changes (after refetch)
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSelectedEditEnvironments(variable.environments.map((env) => env.id))
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setEditIncludeInPreview(variable.include_in_preview ?? false)
  }, [variable.environments, variable.include_in_preview])

  const handleEdit = async () => {
    if (isEditing) {
      await updateMutation.mutateAsync({
        path: {
          project_id: project.id,
          var_id: variable.id,
        },
        body: {
          value: editValue,
          environment_ids: selectedEditEnvironments,
          key: variable.key,
          include_in_preview: editIncludeInPreview,
        },
      })
      setIsEditModalOpen(false)
      setIsEditing(false)
    } else {
      setIsEditing(true)
      setIsEditModalOpen(true)
    }
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
              {isVisible ? dataValue : '••••••••••••'}
            </span>
            <Button variant="ghost" size="sm" onClick={toggleVisibility}>
              {isVisible ? (
                <EyeOff className="h-4 w-4" />
              ) : (
                <Eye className="h-4 w-4" />
              )}
            </Button>
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={handleEdit}
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

      <Dialog open={isEditModalOpen} onOpenChange={setIsEditModalOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit Environment Variable: {variable.key}</DialogTitle>
          </DialogHeader>
          <form
            onSubmit={(e) => {
              e.preventDefault()
              handleEdit()
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
                  />
                ) : (
                  <Input
                    value={editValue}
                    onChange={(e) => setEditValue(e.target.value)}
                    className="font-mono"
                  />
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
                  <Label htmlFor="edit-include-preview" className="text-sm font-medium">
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
            </div>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => setIsEditModalOpen(false)}
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
}

function IntegrationEnvVarRow({
  projectId,
  resolved,
  showAllValues,
}: IntegrationEnvVarRowProps) {
  const [isVisible, setIsVisible] = useState(false)
  const isIntegration = resolved.source.type === 'integration'

  const shouldFetch = isIntegration && (isVisible || showAllValues)

  const { data: revealedValue, refetch, isFetching } = useQuery({
    queryKey: ['resolved-env-var-value', projectId, resolved.key],
    queryFn: () => getResolvedEnvVarValue(projectId, resolved.key),
    enabled: shouldFetch,
    staleTime: 15_000,
  })

  useEffect(() => {
    setIsVisible(showAllValues)
  }, [showAllValues])

  if (resolved.source.type !== 'integration') return null
  const service = resolved.source.service

  const toggleVisibility = () => {
    setIsVisible((prev) => {
      const next = !prev
      if (next) refetch()
      return next
    })
  }

  const valueText = isVisible
    ? isFetching && !revealedValue
      ? 'Revealing…'
      : (revealedValue ?? resolved.value_preview)
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
          onClick={toggleVisibility}
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
    })
    setKey('')
    setValue('')
    setIsMultiline(false)
    setSelectedEnvironments([])
    setIncludeInPreview(false)
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
                <Label htmlFor="include-preview" className="text-sm font-medium">
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
    queryKey: ['resolved-env-vars', project.id],
    queryFn: () => getResolvedEnvVars(project.id),
    staleTime: 15_000,
  })

  const resolvedByKey = useMemo(
    () => indexResolvedByKey(resolvedEnvVars),
    [resolvedEnvVars],
  )

  const integrationOnlyResolved = useMemo(() => {
    if (!resolvedEnvVars) return [] as ResolvedEnvVar[]
    const manualKeys = new Set((envVariables ?? []).map((v) => v.key))
    return resolvedEnvVars
      .filter(
        (entry) =>
          entry.source.type === 'integration' && !manualKeys.has(entry.key),
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
  const hasVariables = hasManualVariables || hasIntegrationVariables
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
              <Button onClick={() => setIsAddDialogOpen(true)} className="flex-1 sm:flex-initial">
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
