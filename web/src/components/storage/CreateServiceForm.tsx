import {
  createServiceMutation,
  getServiceTypeParametersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { CreateServiceResponse, ServiceTypeRoute } from '@/api/client/types.gen'
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
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { customAlphabet } from 'nanoid'
import { useEffect, useMemo } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import * as z from 'zod'

// Create a custom nanoid with lowercase alphanumeric characters
const generateId = customAlphabet('0123456789abcdefghijklmnopqrstuvwxyz', 4)

interface CreateServiceFormProps {
  serviceType: ServiceTypeRoute
  onCancel: () => void
  onSuccess: (data: CreateServiceResponse) => void
}

export function CreateServiceForm({
  serviceType,
  onCancel,
  onSuccess,
}: CreateServiceFormProps) {
  const defaultName = useMemo(
    () => `${serviceType}-${generateId()}`,
    [serviceType]
  )

  // Fetch parameters for the selected service type
  const { data: parametersResponse, isLoading: isLoadingParameters } = useQuery(
    {
      ...getServiceTypeParametersOptions({
        path: {
          service_type: serviceType,
        },
      }),
    }
  )

  // Extract parameters array from response (handle JSON schema format)
  const parameters = useMemo(() => {
    if (!parametersResponse) return undefined

    // If it's already an array, return it
    if (Array.isArray(parametersResponse)) return parametersResponse

    // If it has a 'parameters' property, use that
    if (
      typeof parametersResponse === 'object' &&
      parametersResponse !== null &&
      'parameters' in parametersResponse
    ) {
      return (parametersResponse as { parameters: unknown }).parameters
    }

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
        // Track if this field should be a number
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
      service_type: serviceType,
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
            // Convert "null" string or null/undefined to empty string
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

  if (isLoadingParameters) {
    return (
      <div className="space-y-4 p-4">
        <div className="h-4 w-1/4 bg-muted animate-pulse rounded" />
        <div className="space-y-2">
          {[...Array(3)].map((_, i) => (
            <div key={i} className="h-10 bg-muted animate-pulse rounded" />
          ))}
        </div>
      </div>
    )
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
        <FormField
          control={form.control}
          name="name"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Name</FormLabel>
              <FormControl>
                <Input {...field} placeholder={`my-${serviceType}`} />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />

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

        <div className="flex justify-end space-x-2">
          <Button
            type="button"
            variant="outline"
            onClick={onCancel}
            disabled={createServiceMut.isPending}
          >
            Cancel
          </Button>
          <Button
            type="submit"
            disabled={createServiceMut.isPending || !form.formState.isValid}
          >
            {createServiceMut.isPending ? 'Creating...' : 'Create Service'}
          </Button>
        </div>
      </form>
    </Form>
  )
}
