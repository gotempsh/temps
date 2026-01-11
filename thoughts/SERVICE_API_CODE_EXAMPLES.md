# Service & Container API - Code Integration Examples

## Real-World Code Examples from the Codebase

---

## 1. Creating a Service (Full Workflow)

### Component Hierarchy

```
Parent Component (storage management page)
  └─ CreateServiceDialog (wrapper)
     └─ CreateServiceForm (actual form with submission)
```

### CreateServiceDialog Implementation

```typescript
// File: src/components/storage/CreateServiceDialog.tsx
import { CreateServiceResponse, ServiceTypeRoute } from '@/api/client'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { CreateServiceForm } from './CreateServiceForm'

interface CreateServiceDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceType: ServiceTypeRoute
  onSuccess: (data: CreateServiceResponse) => void
}

export function CreateServiceDialog({
  open,
  onOpenChange,
  serviceType,
  onSuccess,
}: CreateServiceDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>Create {serviceType} Service</DialogTitle>
        </DialogHeader>
        <CreateServiceForm
          serviceType={serviceType}
          onCancel={() => onOpenChange(false)}
          onSuccess={onSuccess}
        />
      </DialogContent>
    </Dialog>
  )
}
```

### CreateServiceForm Implementation (Key Sections)

#### 1. Parameter Loading
```typescript
import {
  createServiceMutation,
  getServiceTypeParametersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQuery } from '@tanstack/react-query'

// Fetch parameters for selected service type
const { data: parametersResponse, isLoading: isLoadingParameters } = useQuery(
  {
    ...getServiceTypeParametersOptions({
      path: {
        service_type: serviceType,  // 'postgres', 'mongodb', etc.
      },
    }),
  }
)
```

#### 2. Parameter Processing
```typescript
// Extract parameters array from response (handle JSON schema format)
const parameters = useMemo(() => {
  if (!parametersResponse) return undefined

  // If it's already an array, return it
  if (Array.isArray(parametersResponse)) return parametersResponse

  // If it has a 'parameters' property, use that
  if ('parameters' in parametersResponse) {
    return (parametersResponse as { parameters: unknown }).parameters
  }

  // If it's a JSON schema with 'properties', convert to parameter array
  if ('properties' in parametersResponse) {
    const schema = parametersResponse as {
      properties: Record<string, any>
      required?: string[]
    }

    return Object.entries(schema.properties).map(([key, prop]) => ({
      name: key,
      description: prop.description || '',
      default_value:
        prop.default !== undefined && prop.default !== null
          ? String(prop.default)
          : '',
      required: schema.required?.includes(key) || false,
      encrypted:
        key.toLowerCase().includes('password') ||
        key.toLowerCase().includes('secret'),
      validation_pattern: prop.pattern || undefined,
      type:
        prop.type === 'integer' ||
        prop.format === 'uint32' ||
        prop.format === 'int32'
          ? 'number'
          : 'string',
    }))
  }

  return undefined
}, [parametersResponse])
```

#### 3. Dynamic Form Schema
```typescript
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm } from 'react-hook-form'
import * as z from 'zod'

const formSchema = useMemo(() => {
  // Build dynamic parameter schema based on loaded parameters
  const paramSchema: Record<
    string,
    z.ZodString | z.ZodOptional<z.ZodString>
  > = {}

  if (parameters && Array.isArray(parameters)) {
    parameters.forEach((param) => {
      if (param && typeof param === 'object' && 'name' in param) {
        const paramName = param.name as string
        const isRequired = (param as { required?: boolean }).required || false
        const validationPattern = (param as { validation_pattern?: string })
          .validation_pattern

        // Start with base string validation
        let fieldSchema = z.string()

        // Add pattern validation if provided
        if (validationPattern) {
          fieldSchema = fieldSchema.regex(
            new RegExp(validationPattern),
            `Invalid format for ${paramName}`
          )
        }

        // Make required or optional
        if (isRequired) {
          paramSchema[paramName] = fieldSchema.min(
            1,
            `${paramName} is required`
          )
        } else {
          paramSchema[paramName] = fieldSchema.optional()
        }
      }
    })
  }

  return z.object({
    name: z
      .string()
      .min(1, 'Service name is required')
      .regex(
        /^[a-z0-9-]+$/,
        'Name must contain only lowercase letters, numbers, and hyphens'
      ),
    service_type: z.string(),
    parameters: z.object(paramSchema),
  })
}, [parameters])

type FormValues = z.infer<typeof formSchema>

const form = useForm<FormValues>({
  resolver: zodResolver(formSchema),
  mode: 'onChange',
  reValidateMode: 'onChange',
  defaultValues: {
    name: defaultName,  // Auto-generated: 'postgres-abc123'
    service_type: serviceType,
    parameters: {},
  },
})
```

#### 4. Parameter Submission
```typescript
const createServiceMut = useMutation({
  ...createServiceMutation(),
  meta: {
    errorTitle: 'Failed to create service',
  },
  onSuccess: (data) => {
    toast.success('Service created successfully')
    onSuccess(data)
  },
})

const onSubmit = async (values: FormValues) => {
  // Convert numeric parameters from strings to numbers
  const processedParameters: Record<string, any> = {}

  if (parameters && Array.isArray(parameters)) {
    for (const param of parameters) {
      const value = values.parameters[param.name]

      // For password/encrypted fields, always send empty string if empty
      if (param.encrypted) {
        processedParameters[param.name] = value || ''
      } else if (value !== undefined && value !== '' && value !== null) {
        // Convert to number if the parameter type is 'number'
        if (param.type === 'number') {
          processedParameters[param.name] = Number(value)
        } else {
          processedParameters[param.name] = value
        }
      }
    }
  } else {
    // Fallback if parameters is not an array
    Object.assign(processedParameters, values.parameters)
  }

  await createServiceMut.mutateAsync({
    body: {
      service_type: values.service_type as ServiceTypeRoute,
      name: values.name,
      parameters: processedParameters,
    },
  })
}
```

#### 5. Dynamic Form Field Rendering
```tsx
{Array.isArray(parameters) &&
  parameters.map((param) => {
    if (!param || typeof param !== 'object' || !('name' in param)) {
      return null
    }
    const paramObj = param as {
      name: string
      required?: boolean
      encrypted?: boolean
      validation_pattern?: string
      default_value?: string
      description?: string
      type?: string
    }
    return (
      <FormField
        key={paramObj.name}
        control={form.control}
        name={`parameters.${paramObj.name}`}
        render={({ field }) => (
          <FormItem>
            <FormLabel>
              {paramObj.name}
              {paramObj.required && (
                <span className="text-destructive">*</span>
              )}
            </FormLabel>
            <FormControl>
              <Input
                {...field}
                value={field.value as string}
                type={
                  paramObj.encrypted
                    ? 'password'
                    : paramObj.type === 'number'
                      ? 'number'
                      : 'text'
                }
                required={paramObj.required}
                pattern={paramObj.validation_pattern || undefined}
                placeholder={paramObj.default_value || undefined}
              />
            </FormControl>
            {paramObj.description && (
              <p className="text-sm text-muted-foreground">
                {paramObj.description}
              </p>
            )}
            <FormMessage />
          </FormItem>
        )}
      />
    )
  })}
```

---

## 2. Container Management (Container Actions)

### ContainerActionDialog Implementation

```typescript
// File: src/components/containers/ContainerActionDialog.tsx
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import {
  startContainerMutation,
  stopContainerMutation,
  restartContainerMutation,
  listContainersOptions,
  getContainerDetailOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

interface ContainerActionDialogProps {
  projectId: string
  environmentId: string
  action: 'start' | 'stop' | 'restart' | null
  containerId: string | null
  onClose: () => void
  onSuccess?: () => void
}

export function ContainerActionDialog({
  projectId,
  environmentId,
  action,
  containerId,
  onClose,
  onSuccess,
}: ContainerActionDialogProps) {
  const queryClient = useQueryClient()

  // Dynamic mutation selection based on action type
  const mutation = useMutation({
    mutationFn: async ({
      containerId,
      action,
    }: {
      containerId: string
      action: 'start' | 'stop' | 'restart'
    }) => {
      const baseParams = {
        path: {
          project_id: parseInt(projectId),
          environment_id: parseInt(environmentId),
          container_id: containerId,
        },
      }

      if (action === 'start') {
        const options = startContainerMutation()
        if (options.mutationFn) {
          return await options.mutationFn(baseParams)
        }
      } else if (action === 'stop') {
        const options = stopContainerMutation()
        if (options.mutationFn) {
          return await options.mutationFn(baseParams)
        }
      } else if (action === 'restart') {
        const options = restartContainerMutation()
        if (options.mutationFn) {
          return await options.mutationFn(baseParams)
        }
      }
      throw new Error(`Invalid action: ${action}`)
    },

    // Invalidate queries on success to refresh data
    onSuccess: (_, { action, containerId }) => {
      // Invalidate the containers list
      queryClient.invalidateQueries({
        queryKey: listContainersOptions({
          path: {
            project_id: parseInt(projectId),
            environment_id: parseInt(environmentId),
          },
        }).queryKey,
      })

      // Invalidate the specific container detail
      queryClient.invalidateQueries({
        queryKey: getContainerDetailOptions({
          path: {
            project_id: parseInt(projectId),
            environment_id: parseInt(environmentId),
            container_id: containerId,
          },
        }).queryKey,
      })

      const actionLabel = action.charAt(0).toUpperCase() + action.slice(1)
      toast.success(`Container ${actionLabel.toLowerCase()}ed successfully`)
      onSuccess?.()
    },

    onError: (error: any, { action }) => {
      toast.error(
        `Failed to ${action} container: ${error?.message || 'Unknown error'}`
      )
    },
  })

  const actionLabels = {
    start: 'Start',
    stop: 'Stop',
    restart: 'Restart',
  }

  const actionDescriptions = {
    start: 'This will start the container.',
    stop: 'This will stop the container. Any unsaved data may be lost.',
    restart:
      'This will restart the container. There may be a brief interruption in service.',
  }

  const handleConfirm = async () => {
    if (!action || !containerId) return

    await mutation.mutateAsync({
      containerId,
      action,
    })
    onClose()
  }

  return (
    <AlertDialog open={!!action} onOpenChange={onClose}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            {action ? actionLabels[action] : ''} Container?
          </AlertDialogTitle>
          <AlertDialogDescription>
            {action ? actionDescriptions[action] : ''}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <div className="bg-muted p-3 rounded-md text-sm">
          <p className="text-muted-foreground">This action cannot be undone.</p>
        </div>
        <div className="flex justify-end gap-3">
          <AlertDialogCancel disabled={mutation.isPending}>
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction
            onClick={handleConfirm}
            disabled={mutation.isPending}
            className={
              action === 'stop' || action === 'restart'
                ? 'bg-destructive hover:bg-destructive/90'
                : ''
            }
          >
            {mutation.isPending ? 'Processing...' : 'Confirm'}
          </AlertDialogAction>
        </div>
      </AlertDialogContent>
    </AlertDialog>
  )
}
```

---

## 3. Integration with Parent Components

### Using CreateServiceDialog in a Page

```typescript
// Example: Storage management page
import { useState } from 'react'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'
import { ServiceTypeRoute } from '@/api/client'

function StorageManagementPage() {
  const [createDialogOpen, setCreateDialogOpen] = useState(false)
  const [selectedServiceType, setSelectedServiceType] = useState<ServiceTypeRoute | null>(null)

  const handleCreateService = (type: ServiceTypeRoute) => {
    setSelectedServiceType(type)
    setCreateDialogOpen(true)
  }

  const handleServiceCreated = (data) => {
    // Refresh service list
    queryClient.invalidateQueries({ queryKey: ['services'] })
    setCreateDialogOpen(false)
    setSelectedServiceType(null)
  }

  return (
    <div>
      {/* Service type selection buttons */}
      <button onClick={() => handleCreateService('postgres')}>
        Create PostgreSQL Service
      </button>
      <button onClick={() => handleCreateService('mongodb')}>
        Create MongoDB Service
      </button>
      <button onClick={() => handleCreateService('redis')}>
        Create Redis Service
      </button>
      <button onClick={() => handleCreateService('s3')}>
        Create S3 Service
      </button>

      {/* Dialog */}
      {selectedServiceType && (
        <CreateServiceDialog
          open={createDialogOpen}
          onOpenChange={setCreateDialogOpen}
          serviceType={selectedServiceType}
          onSuccess={handleServiceCreated}
        />
      )}
    </div>
  )
}
```

### Using ContainerActionDialog in a Component

```typescript
// Example: Container list component
import { useState } from 'react'
import { ContainerActionDialog } from '@/components/containers/ContainerActionDialog'
import { useQuery } from '@tanstack/react-query'
import { listContainersOptions } from '@/api/client/@tanstack/react-query.gen'

function ContainerList({ projectId, environmentId }) {
  const [action, setAction] = useState<'start' | 'stop' | 'restart' | null>(null)
  const [selectedContainerId, setSelectedContainerId] = useState<string | null>(null)

  const { data: containerList } = useQuery(
    listContainersOptions({
      path: {
        project_id: parseInt(projectId),
        environment_id: parseInt(environmentId),
      },
    })
  )

  const handleAction = (
    containerId: string,
    actionType: 'start' | 'stop' | 'restart'
  ) => {
    setSelectedContainerId(containerId)
    setAction(actionType)
  }

  return (
    <>
      <div className="space-y-2">
        {containerList?.containers.map((container) => (
          <div key={container.container_id} className="flex items-center gap-2">
            <span>{container.container_name}</span>
            <span className="text-sm text-muted-foreground">
              {container.status}
            </span>
            <button
              onClick={() => handleAction(container.container_id, 'start')}
              disabled={container.status === 'running'}
            >
              Start
            </button>
            <button
              onClick={() => handleAction(container.container_id, 'stop')}
              disabled={container.status !== 'running'}
            >
              Stop
            </button>
            <button
              onClick={() => handleAction(container.container_id, 'restart')}
            >
              Restart
            </button>
          </div>
        ))}
      </div>

      <ContainerActionDialog
        projectId={projectId}
        environmentId={environmentId}
        action={action}
        containerId={selectedContainerId}
        onClose={() => {
          setAction(null)
          setSelectedContainerId(null)
        }}
        onSuccess={() => {
          // Additional handling if needed
        }}
      />
    </>
  )
}
```

---

## 4. Direct API Usage (Without Components)

### Calling createService Directly

```typescript
import { createServiceMutation } from '@/api/client/@tanstack/react-query.gen'
import { useMutation } from '@tanstack/react-query'

function useCreatePostgresService() {
  return useMutation({
    ...createServiceMutation(),
    onSuccess: (data) => {
      console.log('Service created:', data)
    },
    onError: (error) => {
      console.error('Failed to create service:', error)
    },
  })
}

// Usage
const createMutation = useCreatePostgresService()

await createMutation.mutateAsync({
  body: {
    name: 'my-database',
    service_type: 'postgres',
    parameters: {
      username: 'admin',
      password: 'secret123',
      version: '15',
    },
  },
})
```

### Calling listContainers with Query

```typescript
import { useQuery } from '@tanstack/react-query'
import { listContainersOptions } from '@/api/client/@tanstack/react-query.gen'

function useContainers(projectId: number, environmentId: number) {
  return useQuery(
    listContainersOptions({
      path: {
        project_id: projectId,
        environment_id: environmentId,
      },
    })
  )
}

// Usage
const { data: containers, isLoading, error } = useContainers(123, 456)

if (isLoading) return <div>Loading...</div>
if (error) return <div>Error: {error.message}</div>

return (
  <div>
    {containers?.containers.map((c) => (
      <div key={c.container_id}>{c.container_name}</div>
    ))}
  </div>
)
```

---

## 5. Error Handling Patterns

### Service Creation with Error Handling

```typescript
const createServiceMut = useMutation({
  ...createServiceMutation(),
  onSuccess: (data) => {
    toast.success(`Service "${data.name}" created successfully`)
    onSuccess(data)
  },
  onError: (error: any) => {
    // Network error
    if (!error?.response) {
      toast.error('Network error. Please check your connection.')
      return
    }

    // Server error
    const status = error?.response?.status
    if (status === 400) {
      toast.error('Invalid service configuration')
    } else if (status === 409) {
      toast.error('Service name already exists')
    } else if (status === 500) {
      toast.error('Server error. Please try again later.')
    } else {
      toast.error('Failed to create service')
    }
  },
})
```

### Container Action with Error Handling

```typescript
const mutation = useMutation({
  mutationFn: async (params) => {
    // Make API call
  },
  onSuccess: () => {
    // Invalidate queries
    queryClient.invalidateQueries({
      queryKey: listContainersOptions({
        path: { project_id: 123, environment_id: 456 },
      }).queryKey,
    })
    toast.success('Container action successful')
  },
  onError: (error: any) => {
    const message = error?.response?.data?.message || error?.message
    toast.error(`Failed to perform action: ${message}`)
  },
})
```

---

## 6. Advanced: Custom Hooks

### Custom Hook for Service Creation

```typescript
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  createServiceMutation,
  getServiceTypeParametersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ServiceTypeRoute } from '@/api/client'

export function useServiceCreation(serviceType: ServiceTypeRoute) {
  // Load parameters for the service type
  const { data: parameters, isLoading: isLoadingParameters } = useQuery(
    getServiceTypeParametersOptions({
      path: { service_type: serviceType },
    })
  )

  // Mutation for creating the service
  const createMutation = useMutation({
    ...createServiceMutation(),
    meta: {
      errorTitle: `Failed to create ${serviceType} service`,
    },
  })

  return {
    parameters,
    isLoadingParameters,
    createService: (name: string, params: Record<string, any>) =>
      createMutation.mutateAsync({
        body: {
          name,
          service_type: serviceType,
          parameters: params,
        },
      }),
    isCreating: createMutation.isPending,
    error: createMutation.error,
    data: createMutation.data,
  }
}

// Usage
function ServiceCreationPage() {
  const {
    parameters,
    isLoadingParameters,
    createService,
    isCreating,
  } = useServiceCreation('postgres')

  const handleSubmit = async (formData: any) => {
    await createService(formData.name, formData.parameters)
  }

  return (
    // JSX
  )
}
```

### Custom Hook for Container Management

```typescript
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  listContainersOptions,
  startContainerMutation,
  stopContainerMutation,
  restartContainerMutation,
} from '@/api/client/@tanstack/react-query.gen'

export function useContainerManagement(projectId: number, environmentId: number) {
  const queryClient = useQueryClient()

  // Load containers
  const { data: containerList, isLoading: isLoadingContainers } = useQuery(
    listContainersOptions({
      path: { project_id: projectId, environment_id: environmentId },
    })
  )

  // Create mutations
  const startMutation = useMutation({
    ...startContainerMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: listContainersOptions({
          path: { project_id: projectId, environment_id: environmentId },
        }).queryKey,
      })
    },
  })

  const stopMutation = useMutation({
    ...stopContainerMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: listContainersOptions({
          path: { project_id: projectId, environment_id: environmentId },
        }).queryKey,
      })
    },
  })

  const restartMutation = useMutation({
    ...restartContainerMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: listContainersOptions({
          path: { project_id: projectId, environment_id: environmentId },
        }).queryKey,
      })
    },
  })

  return {
    containers: containerList?.containers || [],
    isLoading: isLoadingContainers,
    startContainer: (containerId: string) =>
      startMutation.mutateAsync({
        path: { project_id: projectId, environment_id: environmentId, container_id: containerId },
      }),
    stopContainer: (containerId: string) =>
      stopMutation.mutateAsync({
        path: { project_id: projectId, environment_id: environmentId, container_id: containerId },
      }),
    restartContainer: (containerId: string) =>
      restartMutation.mutateAsync({
        path: { project_id: projectId, environment_id: environmentId, container_id: containerId },
      }),
  }
}

// Usage
function ContainerManagementPage() {
  const { containers, startContainer, stopContainer } = useContainerManagement(123, 456)

  return (
    <div>
      {containers.map((c) => (
        <div key={c.container_id}>
          <span>{c.container_name}</span>
          <button onClick={() => startContainer(c.container_id)}>Start</button>
          <button onClick={() => stopContainer(c.container_id)}>Stop</button>
        </div>
      ))}
    </div>
  )
}
```

---

## Key Takeaways

1. **Auto-Generated Code** - All API functions and types are auto-generated from OpenAPI spec
2. **React Query Integration** - All hooks follow React Query patterns with proper cache invalidation
3. **Dynamic Forms** - Service parameters are loaded dynamically and forms adjust automatically
4. **Type Safety** - Full TypeScript support with generated types
5. **Error Handling** - Standardized error handling with toast notifications
6. **State Management** - Using React Query for server state, React Hook Form for form state
7. **Reusability** - Components and hooks are composable and reusable across the application
