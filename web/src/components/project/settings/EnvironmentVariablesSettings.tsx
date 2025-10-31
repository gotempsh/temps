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
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Eye, EyeOff, KeyRound, Plus, Upload } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Skeleton } from '@/components/ui/skeleton'
import { Checkbox } from '@/components/ui/checkbox'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { ImportEnvDialog } from '@/components/ui/import-env-dialog'

interface EnvironmentVariableRowProps {
  variable: EnvironmentVariableResponse
  project: ProjectResponse
  refetchEnvVariables: () => void
  isSelected: boolean
  onSelect: (id: number) => void
}

function EnvironmentVariableRow({
  variable,
  project,
  refetchEnvVariables,
  isSelected,
  onSelect,
}: EnvironmentVariableRowProps) {
  const [isVisible, setIsVisible] = useState(false)
  const [isEditing, setIsEditing] = useState(false)
  const [editValue, setEditValue] = useState('')
  const queryClient = useQueryClient()

  const { data, refetch } = useQuery({
    ...getEnvironmentVariableValueOptions({
      path: {
        project_id: project.id,
        key: variable.key,
      },
    }),
    enabled: isVisible || isEditing,
  })

  useEffect(() => {
    if (data && typeof data === 'object' && 'value' in data) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setEditValue(data.value)
    }
  }, [data])

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
      queryClient.invalidateQueries({ queryKey: ['environmentVariables'] })
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
      queryClient.invalidateQueries({ queryKey: ['environmentVariables'] })
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

  // Update selected environments when variable changes (after refetch)
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSelectedEditEnvironments(variable.environments.map((env) => env.id))
  }, [variable.environments])

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
      <div className="py-4 flex items-center justify-between gap-4">
        <div className="flex items-center gap-3 flex-1">
          <Checkbox
            checked={isSelected}
            onCheckedChange={() => onSelect(variable.id)}
          />
          <div className="space-y-1 flex-1">
            <p className="font-medium">{variable.key}</p>
            <div className="flex gap-2">
              {variable.environments.map((env) => (
                <span
                  key={env.name}
                  className="inline-flex items-center rounded-full px-2 py-1 text-xs font-medium bg-secondary text-secondary-foreground"
                >
                  {env.name}
                </span>
              ))}
            </div>
          </div>
        </div>
        <div className="flex gap-2">
          <div className="flex items-center gap-2">
            {isVisible ? (
              <span className="font-mono text-sm">{dataValue}</span>
            ) : (
              <span className="font-mono text-sm">••••••••••••</span>
            )}
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
                <label className="text-sm font-medium">Value</label>
                <Input
                  value={editValue}
                  onChange={(e) => setEditValue(e.target.value)}
                  className="font-mono"
                />
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
  const [selectedEnvironments, setSelectedEnvironments] = useState<number[]>([])

  // Default-select all environments when the dialog opens
  useEffect(() => {
    if (
      isOpen &&
      allEnvironments.length > 0 &&
      selectedEnvironments.length === 0
    ) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSelectedEnvironments(allEnvironments.map((env) => env.id))
    }
  }, [isOpen, allEnvironments, selectedEnvironments.length])

  const handleSubmit = async () => {
    if (!key || !value || selectedEnvironments.length === 0) {
      toast.error(
        'Please fill in all fields and select at least one environment'
      )
      return
    }

    await onSubmit({ key, value, environments: selectedEnvironments })
    setKey('')
    setValue('')
    setSelectedEnvironments([])
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
              <label className="text-sm font-medium">Value</label>
              <Input
                placeholder="Enter value"
                value={value}
                onChange={(e) => setValue(e.target.value)}
                className="font-mono"
              />
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium">Environments</label>
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
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                onOpenChange(false)
                setKey('')
                setValue('')
                setSelectedEnvironments([])
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
  const queryClient = useQueryClient()

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

  const createMutation = useMutation({
    ...createEnvironmentVariableMutation(),
    meta: {
      errorTitle: 'Failed to create environment variable',
    },
    onSuccess: () => {
      setIsAddDialogOpen(false)
      queryClient.invalidateQueries({ queryKey: ['environmentVariables'] })
      refetch()
      toast.success('Environment variable created')
    },
  })

  const handleCreateVariable = async (values: {
    key: string
    value: string
    environments: number[]
  }) => {
    await createMutation.mutateAsync({
      path: {
        project_id: project.id,
      },
      body: {
        key: values.key,
        value: values.value,
        environment_ids: values.environments,
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

    queryClient.invalidateQueries({ queryKey: ['environmentVariables'] })
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
    queryClient.invalidateQueries({ queryKey: ['environmentVariables'] })
    refetch()
  }

  if (isLoading) {
    return <EnvironmentVariablesLoadingState />
  }

  const hasVariables = (envVariables?.length ?? 0) > 0
  const selectedCount = selectedVariables.size
  const allSelected =
    selectedCount === (envVariables?.length ?? 0) && hasVariables

  return (
    <div className="space-y-6">
      <div>
        <div className="flex flex-row items-center justify-between mb-6">
          <div className="space-y-1.5">
            <h2 className="text-2xl font-semibold tracking-tight">
              Environment Variables
            </h2>
            <p className="text-sm text-muted-foreground">
              Manage your project&apos;s environment variables across different
              environments.
            </p>
          </div>
          {hasVariables && (
            <div className="flex gap-2">
              {selectedCount > 0 && (
                <Button
                  variant="destructive"
                  onClick={() => setIsBulkDeleteDialogOpen(true)}
                >
                  Delete {selectedCount} Variable
                  {selectedCount !== 1 ? 's' : ''}
                </Button>
              )}
              <Button
                variant="outline"
                onClick={() => setIsImportDialogOpen(true)}
              >
                <Upload className="h-4 w-4 mr-2" />
                Import .env
              </Button>
              <Button onClick={() => setIsAddDialogOpen(true)}>
                <Plus className="h-4 w-4 mr-2" />
                Add Variable
                <KbdBadge keys={['N']} className="ml-2" />
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
              <div className="divide-y divide-border">
                {(envVariables ?? []).map((variable) => (
                  <EnvironmentVariableRow
                    key={variable.id}
                    variable={variable}
                    project={project}
                    refetchEnvVariables={() => refetch()}
                    isSelected={selectedVariables.has(variable.id)}
                    onSelect={handleSelectVariable}
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
                          className="text-sm font-mono flex items-center justify-between"
                        >
                          <span>{v.key}</span>
                          <div className="flex gap-1">
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
