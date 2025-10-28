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
import { useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { Checkbox } from '@/components/ui/checkbox'
import { KbdBadge } from '@/components/ui/kbd-badge'

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

interface ParsedEnvVariable {
  key: string
  value: string
  selected: boolean
}

interface ImportEnvDialogProps {
  isOpen: boolean
  onOpenChange: (open: boolean) => void
  onImport: (
    variables: { key: string; value: string; environments: number[] }[]
  ) => Promise<void>
  allEnvironments: any[]
  existingKeys: Set<string>
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

function ImportEnvDialog({
  isOpen,
  onOpenChange,
  onImport,
  allEnvironments,
  existingKeys,
}: ImportEnvDialogProps) {
  const [parsedVariables, setParsedVariables] = useState<ParsedEnvVariable[]>(
    []
  )
  const [selectedEnvironments, setSelectedEnvironments] = useState<number[]>([])
  const [isImporting, setIsImporting] = useState(false)
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [rawContent, setRawContent] = useState('')

  // Default-select all environments when the dialog opens
  useEffect(() => {
    if (
      isOpen &&
      allEnvironments.length > 0 &&
      selectedEnvironments.length === 0
    ) {
      setSelectedEnvironments(allEnvironments.map((env: any) => env.id))
    }
  }, [isOpen, allEnvironments, selectedEnvironments.length])

  const parseEnvFile = (content: string) => {
    const lines = content.split('\n')
    const variables: ParsedEnvVariable[] = []

    for (const line of lines) {
      const trimmedLine = line.trim()

      // Skip empty lines and comments
      if (!trimmedLine || trimmedLine.startsWith('#')) {
        continue
      }

      // Parse KEY=VALUE format
      const equalIndex = trimmedLine.indexOf('=')
      if (equalIndex > 0) {
        const key = trimmedLine.substring(0, equalIndex).trim()
        let value = trimmedLine.substring(equalIndex + 1).trim()

        // Remove surrounding quotes if present
        if (
          (value.startsWith('"') && value.endsWith('"')) ||
          (value.startsWith("'") && value.endsWith("'"))
        ) {
          value = value.substring(1, value.length - 1)
        }

        // Check if key already exists
        const alreadyExists = existingKeys.has(key)

        variables.push({
          key,
          value,
          selected: !alreadyExists, // Auto-select only new variables
        })
      }
    }

    return variables
  }

  const handleFileUpload = (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0]
    if (!file) return

    const reader = new FileReader()
    reader.onload = (e) => {
      const content = e.target?.result as string
      setRawContent(content)
      const parsed = parseEnvFile(content)
      setParsedVariables(parsed)

      if (parsed.length === 0) {
        toast.error('No valid environment variables found in the file')
      } else {
        toast.success(
          `Found ${parsed.length} environment variable${parsed.length !== 1 ? 's' : ''}`
        )
      }
    }
    reader.readAsText(file)
  }

  const handlePasteContent = () => {
    if (!rawContent.trim()) {
      toast.error('Please paste or upload .env file content')
      return
    }
    const parsed = parseEnvFile(rawContent)
    setParsedVariables(parsed)

    if (parsed.length === 0) {
      toast.error('No valid environment variables found')
    } else {
      toast.success(
        `Found ${parsed.length} environment variable${parsed.length !== 1 ? 's' : ''}`
      )
    }
  }

  const toggleVariable = (index: number) => {
    setParsedVariables((prev) =>
      prev.map((v, i) => (i === index ? { ...v, selected: !v.selected } : v))
    )
  }

  const toggleAll = () => {
    const allSelected = parsedVariables.every((v) => v.selected)
    setParsedVariables((prev) =>
      prev.map((v) => ({ ...v, selected: !allSelected }))
    )
  }

  const handleImport = async () => {
    const selectedVars = parsedVariables.filter((v) => v.selected)

    if (selectedVars.length === 0) {
      toast.error('Please select at least one variable to import')
      return
    }

    if (selectedEnvironments.length === 0) {
      toast.error('Please select at least one environment')
      return
    }

    setIsImporting(true)
    try {
      await onImport(
        selectedVars.map((v) => ({
          key: v.key,
          value: v.value,
          environments: selectedEnvironments,
        }))
      )

      // Reset state
      setParsedVariables([])
      setSelectedEnvironments([])
      setRawContent('')
      if (fileInputRef.current) {
        fileInputRef.current.value = ''
      }
      onOpenChange(false)
    } finally {
      setIsImporting(false)
    }
  }

  const selectedCount = parsedVariables.filter((v) => v.selected).length

  return (
    <Dialog open={isOpen} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[80vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>Import Environment Variables</DialogTitle>
          <DialogDescription>
            Upload a .env file or paste its contents to import multiple
            variables at once.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-4 flex-1 overflow-auto">
          <div className="space-y-2">
            <label className="text-sm font-medium">Upload .env file</label>
            <div className="flex gap-2">
              <Input
                ref={fileInputRef}
                type="file"
                accept=".env,.env.local,.env.production,.env.development,text/plain"
                onChange={handleFileUpload}
                className="flex-1"
              />
            </div>
          </div>

          <div className="relative">
            <div className="absolute inset-0 flex items-center">
              <span className="w-full border-t" />
            </div>
            <div className="relative flex justify-center text-xs uppercase">
              <span className="bg-background px-2 text-muted-foreground">
                Or paste content
              </span>
            </div>
          </div>

          <div className="space-y-2">
            <label className="text-sm font-medium">.env file content</label>
            <Textarea
              placeholder="DATABASE_URL=postgresql://localhost:5432/db&#10;API_KEY=your_api_key_here&#10;NODE_ENV=production"
              value={rawContent}
              onChange={(e) => setRawContent(e.target.value)}
              className="font-mono text-xs min-h-[120px]"
            />
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={handlePasteContent}
              className="w-full"
            >
              Parse Content
            </Button>
          </div>

          {parsedVariables.length > 0 && (
            <>
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <label className="text-sm font-medium">
                    Select variables to import ({selectedCount}/
                    {parsedVariables.length})
                  </label>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={toggleAll}
                  >
                    {parsedVariables.every((v) => v.selected)
                      ? 'Deselect All'
                      : 'Select All'}
                  </Button>
                </div>
                <div className="border rounded-md max-h-[250px] overflow-auto">
                  <div className="divide-y">
                    {parsedVariables.map((variable, index) => {
                      const alreadyExists = existingKeys.has(variable.key)
                      return (
                        <div
                          key={index}
                          className={cn(
                            'flex items-start gap-3 p-3 hover:bg-muted/50',
                            alreadyExists && 'bg-amber-50 dark:bg-amber-950/20'
                          )}
                        >
                          <Checkbox
                            checked={variable.selected}
                            onCheckedChange={() => toggleVariable(index)}
                            className="mt-1"
                          />
                          <div className="flex-1 space-y-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <p className="font-medium font-mono text-sm">
                                {variable.key}
                              </p>
                              {alreadyExists && (
                                <span className="text-xs px-2 py-0.5 rounded-full bg-amber-100 dark:bg-amber-900 text-amber-800 dark:text-amber-200">
                                  Already exists
                                </span>
                              )}
                            </div>
                            <p className="font-mono text-xs text-muted-foreground truncate">
                              {variable.value}
                            </p>
                          </div>
                        </div>
                      )
                    })}
                  </div>
                </div>
              </div>

              <div className="space-y-2">
                <label className="text-sm font-medium">
                  Target Environments
                </label>
                <div className="flex flex-wrap gap-2">
                  {allEnvironments.map((env: any) => (
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
            </>
          )}
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => {
              onOpenChange(false)
              setParsedVariables([])
              setSelectedEnvironments([])
              setRawContent('')
              if (fileInputRef.current) {
                fileInputRef.current.value = ''
              }
            }}
          >
            Cancel
          </Button>
          <Button
            type="button"
            onClick={handleImport}
            disabled={
              selectedCount === 0 ||
              selectedEnvironments.length === 0 ||
              isImporting
            }
          >
            {isImporting
              ? 'Importing...'
              : `Import ${selectedCount} Variable${selectedCount !== 1 ? 's' : ''}`}
          </Button>
        </DialogFooter>
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
    variables: { key: string; value: string; environments: number[] }[]
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
            environment_ids: variable.environments,
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
      setSelectedVariables(
        new Set((envVariables ?? []).map((v) => v.id))
      )
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
  const allSelected = selectedCount === (envVariables?.length ?? 0) && hasVariables

  // Keyboard shortcut to add new variable (N key)
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
                  Delete {selectedCount} Variable{selectedCount !== 1 ? 's' : ''}
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
