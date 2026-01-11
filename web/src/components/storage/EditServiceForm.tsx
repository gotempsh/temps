import {
  updateServiceMutation,
  getServiceTypeParametersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ExternalServiceInfo, ServiceTypeRoute } from '@/api/client/types.gen'
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
import { useEffect, useMemo } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import * as z from 'zod'

interface EditServiceFormProps {
  service: ExternalServiceInfo
  currentParameters?: Record<string, string> | null
  onCancel: () => void
  onSuccess: () => void
}

export function EditServiceForm({
  service,
  currentParameters,
  onCancel,
  onSuccess,
}: EditServiceFormProps) {
  // Fetch parameters for the service type
  const { data: parametersResponse, isLoading: isLoadingParameters } = useQuery(
    {
      ...getServiceTypeParametersOptions({
        path: {
          service_type: service.service_type as ServiceTypeRoute,
        },
      }),
    }
  )

  // Extract and filter parameters array from response
  // Only includes parameters where x-editable is true (excludes immutable parameters)
  const parameters = useMemo(() => {
    if (!parametersResponse) return undefined

    if (Array.isArray(parametersResponse)) return parametersResponse

    if (
      typeof parametersResponse === 'object' &&
      parametersResponse !== null &&
      'parameters' in parametersResponse
    ) {
      return (parametersResponse as { parameters: unknown }).parameters
    }

    if (
      typeof parametersResponse === 'object' &&
      parametersResponse !== null &&
      'properties' in parametersResponse
    ) {
      const schema = parametersResponse as {
        properties: Record<string, any>
        required?: string[]
      }

      return Object.entries(schema.properties)
        .filter(([, prop]) => {
          // Only include parameters that are editable (x-editable is not explicitly false)
          // If x-editable is not specified, default to true for backward compatibility
          return prop['x-editable'] !== false
        })
        .map(([key, prop]) => ({
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
          x_editable: prop['x-editable'] !== false,
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
      parameters: z.object(paramSchema),
    })
  }, [parameters])

  type FormValues = z.infer<typeof formSchema>

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    mode: 'onChange',
    reValidateMode: 'onChange',
    defaultValues: {
      parameters: {},
    },
  })

  // Set default values for parameters when they are loaded
  // Uses current parameter values as defaults, falling back to empty strings
  useEffect(() => {
    if (Array.isArray(parameters)) {
      const defaultParameters = parameters.reduce<Record<string, string>>(
        (acc, param) => {
          if (param && typeof param === 'object' && 'name' in param) {
            const paramName = param.name as string
            // Priority: current value > empty string
            // For encrypted fields (passwords), keep empty to allow optional updates
            if (currentParameters?.[paramName]) {
              acc[paramName] = currentParameters[paramName]
            } else {
              acc[paramName] = ''
            }
          }
          return acc
        },
        {}
      )
      form.setValue('parameters', defaultParameters)
    }
  }, [parameters, currentParameters, form])

  const updateServiceMut = useMutation({
    ...updateServiceMutation(),
    meta: {
      errorTitle: 'Failed to update service',
    },
    onSuccess: () => {
      toast.success('Service updated successfully')
      onSuccess()
    },
  })

  const onSubmit = async (values: FormValues) => {
    // Convert numeric parameters from strings to numbers
    const processedParameters: Record<string, any> = {}

    if (parameters && Array.isArray(parameters)) {
      for (const param of parameters) {
        const value = values.parameters[param.name]

        // Skip empty encrypted fields (passwords)
        if (param.encrypted) {
          if (value && value !== '') {
            processedParameters[param.name] = value
          }
          // Don't include empty password fields
        } else if (value !== undefined && value !== '' && value !== null) {
          if (param.type === 'number') {
            processedParameters[param.name] = Number(value)
          } else {
            processedParameters[param.name] = value
          }
        }
      }
    } else {
      Object.assign(processedParameters, values.parameters)
    }

    await updateServiceMut.mutateAsync({
      path: { id: service.id },
      body: {
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
              x_editable?: boolean
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
                        placeholder={
                          paramObj.encrypted
                            ? 'Leave blank to keep current value'
                            : paramObj.default_value || undefined
                        }
                      />
                    </FormControl>
                    {paramObj.description && (
                      <p className="text-sm text-muted-foreground">
                        {paramObj.description}
                      </p>
                    )}
                    {paramObj.encrypted && (
                      <p className="text-xs text-muted-foreground">
                        Leave blank to keep current value
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
            disabled={updateServiceMut.isPending}
          >
            Cancel
          </Button>
          <Button
            type="submit"
            disabled={updateServiceMut.isPending || !form.formState.isValid}
          >
            {updateServiceMut.isPending ? 'Saving...' : 'Save Changes'}
          </Button>
        </div>
      </form>
    </Form>
  )
}
