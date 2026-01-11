import {
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
import { getServiceTypeWithFallback } from '@/lib/service-type-detector'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { customAlphabet } from 'nanoid'
import { useEffect, useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import * as z from 'zod'

const generateId = customAlphabet('0123456789abcdefghijklmnopqrstuvwxyz', 4)

interface ImportServiceFormProps {
  container: AvailableContainerInfo
  onCancel: () => void
  onSuccess: () => void
}

export function ImportServiceForm({
  container,
  onCancel,
  onSuccess,
}: ImportServiceFormProps) {
  const [selectedServiceType, setSelectedServiceType] = useState<string | null>(
    null
  )

  // Fetch available service types
  const { data: serviceTypes } = useQuery({
    ...getServiceTypesOptions(),
  })

  // Auto-detect service type from container image (only if not already set)
  useEffect(() => {
    if (!selectedServiceType && container.image) {
      const detectedType = getServiceTypeWithFallback(
        container.service_type,
        container.image
      )
      if (detectedType) {
        setSelectedServiceType(detectedType)
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [container.image, container.service_type])

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
  const parameters = useMemo((): Array<{
    name: string
    description?: string
    default_value?: string
    required?: boolean
    encrypted?: boolean
    validation_pattern?: string
    type?: string
  }> | undefined => {
    if (!parametersResponse) return undefined

    if (Array.isArray(parametersResponse)) return parametersResponse

    if (
      typeof parametersResponse === 'object' &&
      parametersResponse !== null &&
      'parameters' in parametersResponse
    ) {
      const params = (parametersResponse as { parameters: unknown }).parameters
      if (Array.isArray(params)) return params
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
      name: `${container.container_name}-${generateId()}`,
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
  }, [parameters])

  const importServiceMut = useMutation({
    ...importExternalServiceMutation(),
    meta: {
      errorTitle: 'Failed to import service',
    },
    onSuccess: () => {
      toast.success('Service imported successfully')
      onSuccess()
    },
    onError: (error) => {
      toast.error(error?.message || 'Failed to import service')
    },
  })

  const onSubmit = async (values: FormValues) => {
    if (!selectedServiceType) {
      toast.error('Please select a service type')
      return
    }
    await importServiceMut.mutateAsync({
      body: {
        container_id: container.container_id,
        name: values.name,
        parameters: values.parameters || {},
        service_type: selectedServiceType as any,
      },
    })
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
        {/* Service Type Selection */}
        <FormItem>
          <FormLabel>Service Type</FormLabel>
          <Select value={selectedServiceType || ''} onValueChange={setSelectedServiceType}>
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
          <p className="text-xs text-muted-foreground mt-1">
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
          <div className="text-sm text-muted-foreground">
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
            const isRequired = (param as { required?: boolean }).required || false

            return (
              <FormField
                key={paramName}
                control={form.control}
                name={`parameters.${paramName}`}
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>
                      {paramName}
                      {isRequired && <span className="text-destructive"> *</span>}
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
            onClick={onCancel}
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
  )
}
