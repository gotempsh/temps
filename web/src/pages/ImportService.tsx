import {
  listAvailableContainersOptions,
  importExternalServiceMutation,
  getServiceTypeParametersOptions,
  getServiceTypesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AvailableContainerInfo } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Card } from '@/components/ui/card'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { ArrowLeft, AlertCircle, Loader2, CheckCircle } from 'lucide-react'
import { customAlphabet } from 'nanoid'
import { getServiceTypeWithFallback } from '@/lib/service-type-detector'
import { useEffect, useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import * as z from 'zod'

const generateId = customAlphabet('0123456789abcdefghijklmnopqrstuvwxyz', 4)

type Step = 'select-container' | 'configure-service'

export function ImportService() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [step, setStep] = useState<Step>('select-container')
  const [selectedContainer, setSelectedContainer] =
    useState<AvailableContainerInfo | null>(null)
  const [selectedServiceType, setSelectedServiceType] = useState<string | null>(
    null
  )

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Storage', href: '/storage' },
      { label: 'Import Service', href: '/storage/import' },
    ])
  }, [setBreadcrumbs])

  // Fetch available containers
  const { data: containers, isLoading: isLoadingContainers, error: containersError } =
    useQuery({
      ...listAvailableContainersOptions(),
    })

  // Memoize service type extraction for each container
  const containerServiceTypes = useMemo(() => {
    if (!containers) return {}
    return containers.reduce(
      (acc, container) => {
        acc[container.container_id] = getServiceTypeWithFallback(
          container.service_type,
          container.image
        )
        return acc
      },
      {} as Record<string, string | null>
    )
  }, [containers])

  // Fetch available service types
  const { data: serviceTypes } = useQuery({
    ...getServiceTypesOptions(),
  })

  // Auto-detect service type from container image when container is selected
  useEffect(() => {
    if (selectedContainer && !selectedServiceType) {
      const detectedType = getServiceTypeWithFallback(
        selectedContainer.service_type,
        selectedContainer.image
      )
      if (detectedType) {
        setSelectedServiceType(detectedType)
      }
    }
  }, [selectedContainer, selectedServiceType])

  // Fetch parameters for the selected service type
  const { data: parametersResponse, isLoading: isLoadingParameters } = useQuery(
    {
      ...getServiceTypeParametersOptions({
        path: {
          service_type: selectedServiceType as any,
        },
      }),
      enabled: !!selectedServiceType,
    }
  )

  // Extract parameters array from response
  const parameters = useMemo(() => {
    if (!parametersResponse) return undefined

    if (Array.isArray(parametersResponse)) return parametersResponse

    if (
      typeof parametersResponse === 'object' &&
      parametersResponse !== null &&
      'properties' in parametersResponse
    ) {
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

  // Dynamically create the form schema based on parameters
  const formSchema = useMemo(() => {
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

          let fieldSchema = z.string()

          if (validationPattern) {
            fieldSchema = fieldSchema.regex(
              new RegExp(validationPattern),
              `Invalid format for ${paramName}`
            )
          }

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
      parameters: z.object(paramSchema),
    })
  }, [parameters])

  type FormValues = z.infer<typeof formSchema>

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    mode: 'onChange',
    reValidateMode: 'onChange',
    defaultValues: {
      name: selectedContainer
        ? `${selectedContainer.container_name}-${generateId()}`
        : '',
      parameters: {},
    },
  })

  // Set default values for parameters when they are loaded
  useEffect(() => {
    if (Array.isArray(parameters)) {
      const defaultParameters = parameters.reduce<Record<string, string>>(
        (acc, param) => {
          if (param && typeof param === 'object' && 'name' in param) {
            const defaultValue = (param as { default_value?: string })
              .default_value
            acc[param.name as string] =
              defaultValue && defaultValue !== 'null' ? defaultValue : ''
          }
          return acc
        },
        {}
      )
      form.setValue('parameters', defaultParameters)
    }
  }, [parameters, form])

  const importServiceMut = useMutation({
    ...importExternalServiceMutation(),
    meta: {
      errorTitle: 'Failed to import service',
    },
    onSuccess: () => {
      toast.success('Service imported successfully')
      navigate('/storage')
    },
    onError: (error) => {
      toast.error(error?.message || 'Failed to import service')
    },
  })

  const onSubmit = async (values: FormValues) => {
    if (!selectedContainer) return

    await importServiceMut.mutateAsync({
      body: {
        container_id: selectedContainer.container_id,
        name: values.name,
        parameters: values.parameters || {},
      },
    })
  }

  const handleContainerSelected = (container: AvailableContainerInfo) => {
    setSelectedContainer(container)
    setSelectedServiceType(null)
    form.setValue('name', `${container.container_name}-${generateId()}`)
    setStep('configure-service')
  }

  const handleBackToContainers = () => {
    setStep('select-container')
    setSelectedContainer(null)
    setSelectedServiceType(null)
    form.reset()
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-4 space-y-6 md:p-6 max-w-4xl mx-auto">
        {/* Header */}
        <div className="flex items-center gap-3">
          <Link to="/storage">
            <Button variant="ghost" size="icon">
              <ArrowLeft className="h-4 w-4" />
            </Button>
          </Link>
          <div>
            <h1 className="text-2xl font-semibold">Import Service</h1>
            <p className="text-sm text-muted-foreground">
              {step === 'select-container'
                ? 'Select a running container to import as a service'
                : `Configure ${selectedContainer?.container_name}`}
            </p>
          </div>
        </div>

        {/* Step 1: Select Container */}
        {step === 'select-container' && (
          <div className="space-y-4">
            {isLoadingContainers && (
              <div className="flex items-center justify-center py-12">
                <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
              </div>
            )}

            {containersError && (
              <Alert variant="destructive">
                <AlertCircle className="h-4 w-4" />
                <AlertDescription>
                  Failed to load available containers. Please try again.
                </AlertDescription>
              </Alert>
            )}

            {!isLoadingContainers &&
              !containersError &&
              (!containers || containers.length === 0) && (
                <Alert>
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    No containers available to import. Make sure you have running
                    containers in your environment.
                  </AlertDescription>
                </Alert>
              )}

            {!isLoadingContainers &&
              !containersError &&
              containers &&
              containers.length > 0 && (
                <div className="grid gap-3">
                  {containers.map((container) => (
                    <Card
                      key={container.container_id}
                      className="p-4 cursor-pointer hover:bg-muted/50 transition-colors"
                      onClick={() => handleContainerSelected(container)}
                    >
                      <div className="flex items-center justify-between gap-3">
                        <div className="flex items-center gap-3 flex-1 min-w-0">
                          {containerServiceTypes[container.container_id] && (
                            <ServiceLogo
                              service={
                                containerServiceTypes[
                                  container.container_id
                                ] as any
                              }
                              className="h-10 w-10 shrink-0"
                            />
                          )}
                          <div className="flex-1 min-w-0">
                            <h3 className="font-medium">{container.container_name}</h3>
                            <p className="text-sm text-muted-foreground font-mono truncate">
                              {container.image}
                            </p>
                            <p className="text-xs text-muted-foreground">
                              {container.container_id.substring(0, 12)}
                            </p>
                          </div>
                        </div>
                        <div className="flex items-center gap-2 shrink-0">
                          {container.is_running && (
                            <CheckCircle className="h-4 w-4 text-green-500" />
                          )}
                          <Button
                            variant="outline"
                            onClick={(e) => {
                              e.stopPropagation()
                              handleContainerSelected(container)
                            }}
                          >
                            Select
                          </Button>
                        </div>
                      </div>
                    </Card>
                  ))}
                </div>
              )}
          </div>
        )}

        {/* Step 2: Configure Service */}
        {step === 'configure-service' && selectedContainer && (
          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
              {/* Service Type Selection */}
              <FormItem>
                <FormLabel>Service Type</FormLabel>
                <Select
                  value={selectedServiceType || ''}
                  onValueChange={setSelectedServiceType}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Select service type" />
                  </SelectTrigger>
                  <SelectContent>
                    {serviceTypes?.map((type) => (
                      <SelectItem key={type} value={type}>
                        {type}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground mt-2">
                  Select the type of service this container is running
                </p>
              </FormItem>

              {/* Service Name */}
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Service Name</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="my-service"
                        {...field}
                        disabled={importServiceMut.isPending}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              {/* Dynamic Parameters */}
              {selectedServiceType && isLoadingParameters && (
                <div className="text-sm text-muted-foreground py-4">
                  Loading service parameters...
                </div>
              )}

              {selectedServiceType &&
                parameters &&
                Array.isArray(parameters) &&
                parameters.map((param) => {
                  if (!param || typeof param !== 'object' || !('name' in param)) {
                    return null
                  }

                  const paramName = param.name as string
                  const paramDescription = (param as { description?: string })
                    .description
                  const isRequired = (param as { required?: boolean })
                    .required || false

                  return (
                    <FormField
                      key={paramName}
                      control={form.control}
                      name={`parameters.${paramName}`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel>
                            {paramName}
                            {isRequired && (
                              <span className="text-destructive"> *</span>
                            )}
                          </FormLabel>
                          <FormControl>
                            <Input
                              placeholder={paramDescription || paramName}
                              {...field}
                              disabled={importServiceMut.isPending}
                              type={
                                (param as { encrypted?: boolean }).encrypted
                                  ? 'password'
                                  : 'text'
                              }
                            />
                          </FormControl>
                          {paramDescription && (
                            <p className="text-xs text-muted-foreground">
                              {paramDescription}
                            </p>
                          )}
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                  )
                })}

              {/* Form Actions */}
              <div className="flex gap-3 pt-4">
                <Button
                  type="button"
                  variant="outline"
                  onClick={handleBackToContainers}
                  disabled={importServiceMut.isPending}
                >
                  Back
                </Button>
                <Button
                  type="submit"
                  disabled={
                    !selectedServiceType ||
                    isLoadingParameters ||
                    importServiceMut.isPending ||
                    !form.formState.isValid
                  }
                >
                  {importServiceMut.isPending ? 'Importing...' : 'Import Service'}
                </Button>
              </div>
            </form>
          </Form>
        )}
      </div>
    </div>
  )
}
