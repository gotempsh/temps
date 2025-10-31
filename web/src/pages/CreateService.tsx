import {
  createServiceMutation,
  getProviderMetadataOptions,
  getServiceTypeParametersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  CreateServiceResponse,
  ServiceTypeRoute,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Form,
  FormControl,
  FormDescription,
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
import { DynamicForm } from '@/components/forms/DynamicForm'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { customAlphabet } from 'nanoid'
import { ArrowLeft, Loader2 } from 'lucide-react'
import { useEffect, useMemo } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate, useSearchParams } from 'react-router-dom'
import { toast } from 'sonner'
import * as z from 'zod'

// Create a custom nanoid with lowercase alphanumeric characters
const generateId = customAlphabet('0123456789abcdefghijklmnopqrstuvwxyz', 4)

export function CreateService() {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const serviceType = searchParams.get('type') as ServiceTypeRoute | null
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Storage', href: '/storage' },
      { label: 'Create Service', href: '/storage/create' },
    ])
  }, [setBreadcrumbs])

  // Fetch provider metadata for display
  const { data: providerMetadata } = useQuery({
    ...getProviderMetadataOptions({
      path: {
        service_type: serviceType || '',
      },
    }),
    enabled: !!serviceType,
  })

  const defaultName = useMemo(
    () => (serviceType ? `${serviceType}-${generateId()}` : ''),
    [serviceType]
  )

  // Fetch parameters for the selected service type
  const { data: parametersResponse, isLoading: isLoadingParameters } = useQuery({
    ...getServiceTypeParametersOptions({
      path: {
        service_type: serviceType || '',
      },
    }),
    enabled: !!serviceType,
  })

  // Convert JSON schema to parameters array if needed
  const parameters = useMemo(() => {
    if (!parametersResponse) return undefined

    // If it's already an array, return it
    if (Array.isArray(parametersResponse)) return parametersResponse

    // If it's a JSON schema with 'properties', convert to parameter array
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
        choices: prop.enum || undefined,
      }))
    }

    return undefined
  }, [parametersResponse])

  // Dynamically create the form schema based on parameters
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
    mode: 'onChange', // Validate on change for immediate feedback
    reValidateMode: 'onChange', // Revalidate on every change
    defaultValues: {
      name: defaultName,
      service_type: serviceType || '',
      parameters: {},
    },
  })

  // Set default values for parameters when they are loaded
  useEffect(() => {
    if (parameters) {
      const defaultParameters = parameters.reduce<Record<string, string>>(
        (acc, param) => {
          // Convert "null" string or empty to empty string
          acc[param.name] =
            param.default_value && param.default_value !== 'null'
              ? param.default_value
              : ''
          return acc
        },
        {}
      )
      form.setValue('parameters', defaultParameters)
    }
  }, [parameters, form])

  const createServiceMut = useMutation({
    ...createServiceMutation(),
    meta: {
      errorTitle: 'Failed to create service',
    },
    onSuccess: (data: CreateServiceResponse) => {
      toast.success('Service created successfully')
      navigate(`/storage/${data.id}`)
    },
  })

  const onSubmit = async (values: FormValues) => {
    // Convert numeric parameters from strings to numbers
    const processedParameters: Record<string, any> = {}

    if (parameters && Array.isArray(parameters)) {
      for (const param of parameters) {
        const value = values.parameters[param.name]

        // For password/encrypted fields, always send empty string even if empty
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

  if (!serviceType) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6 max-w-4xl mx-auto">
          <div className="space-y-2">
            <h1 className="text-2xl font-semibold">Create Service</h1>
            <p className="text-muted-foreground">
              Please select a service type from the URL parameter.
            </p>
          </div>
          <Link to="/storage">
            <Button variant="outline">
              <ArrowLeft className="h-4 w-4 mr-2" />
              Back to Storage
            </Button>
          </Link>
        </div>
      </div>
    )
  }

  if (isLoadingParameters) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6 max-w-4xl mx-auto">
          <div className="space-y-4">
            <div className="h-8 w-1/3 bg-muted animate-pulse rounded" />
            <div className="space-y-3">
              {[...Array(5)].map((_, i) => (
                <div key={i} className="space-y-2">
                  <div className="h-4 w-1/4 bg-muted animate-pulse rounded" />
                  <div className="h-10 bg-muted animate-pulse rounded" />
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-4 space-y-6 md:p-6 max-w-4xl mx-auto">
        {/* Header with provider info */}
        <div className="space-y-4">
          <Link to="/storage">
            <Button variant="ghost" size="sm" className="gap-2">
              <ArrowLeft className="h-4 w-4" />
              Back to Storage
            </Button>
          </Link>

          {providerMetadata && (
            <div className="flex items-center gap-4">
              <div
                className="flex items-center justify-center rounded-md p-3"
                style={{ backgroundColor: providerMetadata.color }}
              >
                <img
                  src={providerMetadata.icon_url}
                  alt={`${providerMetadata.display_name} logo`}
                  width={40}
                  height={40}
                  className="rounded-md brightness-0 invert"
                />
              </div>
              <div>
                <h1 className="text-2xl font-semibold">
                  Create {providerMetadata.display_name} Service
                </h1>
                <p className="text-muted-foreground">
                  {providerMetadata.description}
                </p>
              </div>
            </div>
          )}
        </div>

        {/* Form */}
        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
            {/* Service Name */}
            <FormField
              control={form.control}
              name="name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Service Name</FormLabel>
                  <FormControl>
                    <Input {...field} placeholder={`my-${serviceType}`} />
                  </FormControl>
                  <FormDescription>
                    A unique name to identify this service
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            {/* Dynamic Parameters */}
            {parameters?.map((param: ServiceTypeParameterResponse, index: number) => {
              // Check if this parameter should be grouped with the next one
              const isHost = param.name === 'host'
              const isUsername = param.name === 'username'
              const nextParam = parameters[index + 1]
              const shouldGroup =
                (isHost && nextParam?.name === 'port') ||
                (isUsername && nextParam?.name === 'password')

              // Skip rendering if this is 'port' or 'password' (they'll be rendered with their pair)
              if (param.name === 'port' || param.name === 'password') {
                return null
              }

              if (shouldGroup && nextParam) {
                // Render paired fields (host/port or username/password)
                return (
                  <div key={param.name} className="grid grid-cols-2 gap-4">
                    <FormField
                      control={form.control}
                      name={`parameters.${param.name}`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel>
                            {param.name.charAt(0).toUpperCase() +
                              param.name.slice(1)}
                            {param.required && (
                              <span className="text-destructive ml-1">*</span>
                            )}
                          </FormLabel>
                          <FormControl>
                            <Input
                              {...field}
                              value={field.value as string}
                              type={
                                param.encrypted
                                  ? 'password'
                                  : param.type === 'number'
                                    ? 'number'
                                    : 'text'
                              }
                              required={param.required}
                              pattern={param.validation_pattern || undefined}
                              placeholder={param.default_value || undefined}
                            />
                          </FormControl>
                          {param.description && (
                            <FormDescription>
                              {param.description}
                            </FormDescription>
                          )}
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                    <FormField
                      control={form.control}
                      name={`parameters.${nextParam.name}`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel>
                            {nextParam.name.charAt(0).toUpperCase() +
                              nextParam.name.slice(1)}
                            {nextParam.required && (
                              <span className="text-destructive ml-1">*</span>
                            )}
                          </FormLabel>
                          <FormControl>
                            <Input
                              {...field}
                              value={field.value as string}
                              type={nextParam.encrypted ? 'password' : 'text'}
                              required={nextParam.required}
                              pattern={
                                nextParam.validation_pattern || undefined
                              }
                              placeholder={
                                nextParam.default_value || undefined
                              }
                            />
                          </FormControl>
                          {nextParam.description && (
                            <FormDescription>
                              {nextParam.description}
                            </FormDescription>
                          )}
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                  </div>
                )
              }

              // Render single field
              return (
                <FormField
                  key={param.name}
                  control={form.control}
                  name={`parameters.${param.name}`}
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>
                        {param.name.charAt(0).toUpperCase() +
                          param.name.slice(1)}
                        {param.required && (
                          <span className="text-destructive ml-1">*</span>
                        )}
                      </FormLabel>
                      <FormControl>
                        {param.choices && param.choices.length > 0 ? (
                          // Render Select for fields with choices
                          <Select
                            onValueChange={field.onChange}
                            value={field.value as string || param.default_value || undefined}
                          >
                            <SelectTrigger>
                              <SelectValue
                                placeholder={param.default_value ? `Default: ${param.default_value}` : `Select ${param.name}`}
                              />
                            </SelectTrigger>
                            <SelectContent>
                              {param.choices.map((choice) => (
                                <SelectItem key={choice} value={choice}>
                                  {choice}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        ) : (
                          // Render Input for fields without choices
                          <Input
                            {...field}
                            value={field.value as string}
                            type={
                              param.encrypted
                                ? 'password'
                                : param.type === 'number'
                                  ? 'number'
                                  : 'text'
                            }
                            required={param.required}
                            pattern={param.validation_pattern || undefined}
                            placeholder={param.default_value || undefined}
                          />
                        )}
                      </FormControl>
                      {param.description && (
                        <FormDescription>{param.description}</FormDescription>
                      )}
                      <FormMessage />
                    </FormItem>
                  )}
                />
              )
            })}

            {/* Action Buttons */}
            <div className="flex justify-end space-x-3 pt-6">
              <Button
                type="button"
                variant="outline"
                onClick={() => navigate('/storage')}
                disabled={createServiceMut.isPending}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={createServiceMut.isPending || !form.formState.isValid}
              >
                {createServiceMut.isPending ? (
                  <>
                    <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                    Creating...
                  </>
                ) : (
                  'Create Service'
                )}
              </Button>
            </div>
          </form>
        </Form>
      </div>
    </div>
  )
}
