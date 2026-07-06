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
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { AlertTriangle, ChevronDown, ChevronRight } from 'lucide-react'
import { customAlphabet } from 'nanoid'
import { useEffect, useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import * as z from 'zod'

/**
 * Parameter names that get tucked into the "Advanced" collapsible by
 * default. Heroku/Render users were confused by `database`, `username`,
 * and `password` showing up at the top — Temps creates a per-project
 * `<project>_<env>` database for each linked project automatically, so
 * the default admin credentials are rarely what the user needs to edit.
 */
const ADVANCED_PARAM_NAMES = new Set([
  'database',
  'username',
  'password',
  'access_key',
  'secret_key',
])

/** Service types that support WAL-G streaming backups */
const WALG_SERVICE_TYPES = ['postgres', 'redis', 'mongodb']

// Create a custom nanoid with lowercase alphanumeric characters
const generateId = customAlphabet('0123456789abcdefghijklmnopqrstuvwxyz', 4)

/**
 * Shows a warning when the user selects a Docker image without WAL-G support
 * for a database service type that supports streaming backups.
 */
function BackupWarning({
  control,
  serviceType,
}: {
  control: any
  serviceType: string
}) {
  const dockerImage = useWatch({
    control,
    name: 'parameters.docker_image',
  })

  if (!WALG_SERVICE_TYPES.includes(serviceType)) return null

  // Default images (gotempsh/*) include WAL-G — no warning needed
  const image = (dockerImage as string) || ''
  if (!image || image.includes('gotempsh/')) return null

  return (
    <div className="rounded-lg border border-amber-500/20 bg-amber-500/10 p-3 flex gap-2">
      <AlertTriangle className="h-4 w-4 text-amber-600 dark:text-amber-400 mt-0.5 flex-shrink-0" />
      <div className="space-y-1">
        <p className="text-sm font-medium text-amber-800 dark:text-amber-200">
          Atomic backups only
        </p>
        <p className="text-xs text-amber-700 dark:text-amber-300">
          This image does not include WAL-G. Backups will buffer the entire
          database in memory before uploading to S3. For large databases
          this can cause out-of-memory failures and service interruptions.
          Use the default image or a <code className="font-mono">gotempsh/</code> image
          for streaming backups with constant memory usage.
        </p>
      </div>
    </div>
  )
}

interface CreateServiceFormProps {
  serviceType: ServiceTypeRoute
  onCancel: () => void
  onSuccess: (data: CreateServiceResponse) => void
}

type ParamFieldObj = {
  name: string
  required?: boolean
  encrypted?: boolean
  validation_pattern?: string
  default_value?: string
  description?: string
  type?: string
  enum_values?: string[]
}

function ParamField({
  paramObj,
  control,
}: {
  paramObj: ParamFieldObj
  control: any
}) {
  return (
    <FormField
      control={control}
      name={`parameters.${paramObj.name}`}
      render={({ field }) => (
        <FormItem>
          <FormLabel>
            {paramObj.name}
            {paramObj.required && <span className="text-destructive">*</span>}
          </FormLabel>
          <FormControl>
            {paramObj.enum_values && paramObj.enum_values.length > 0 ? (
              <Select
                value={(field.value as string) || paramObj.default_value}
                onValueChange={field.onChange}
              >
                <SelectTrigger>
                  <SelectValue placeholder={paramObj.default_value || 'Select value'} />
                </SelectTrigger>
                <SelectContent>
                  {paramObj.enum_values.map((value) => (
                    <SelectItem key={value} value={value}>
                      {value}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
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
            )}
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
  const [isAdvancedOpen, setIsAdvancedOpen] = useState(false)

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
        enum_values: Array.isArray(prop.enum) ? prop.enum : undefined,
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
          (() => {
            type ParamObj = {
              name: string
              required?: boolean
              encrypted?: boolean
              validation_pattern?: string
              default_value?: string
              description?: string
              type?: string
              enum_values?: string[]
            }
            const valid = (parameters as unknown[]).filter(
              (p): p is ParamObj =>
                !!p && typeof p === 'object' && 'name' in (p as object),
            )
            const basic = valid.filter(
              (p) => !ADVANCED_PARAM_NAMES.has(p.name.toLowerCase()),
            )
            const advanced = valid.filter((p) =>
              ADVANCED_PARAM_NAMES.has(p.name.toLowerCase()),
            )
            return (
              <>
                {basic.map((paramObj) => (
                  <ParamField
                    key={paramObj.name}
                    paramObj={paramObj}
                    control={form.control}
                  />
                ))}
                {advanced.length > 0 && (
                  <Collapsible
                    open={isAdvancedOpen}
                    onOpenChange={setIsAdvancedOpen}
                  >
                    <CollapsibleTrigger asChild>
                      <button
                        type="button"
                        className="flex items-center gap-1.5 text-sm font-medium text-muted-foreground hover:text-foreground"
                      >
                        {isAdvancedOpen ? (
                          <ChevronDown className="h-4 w-4" />
                        ) : (
                          <ChevronRight className="h-4 w-4" />
                        )}
                        Advanced configuration
                        <span className="text-xs text-muted-foreground/70">
                          (default DB / credentials — Temps auto-creates a
                          per-project database for each linked project)
                        </span>
                      </button>
                    </CollapsibleTrigger>
                    <CollapsibleContent className="space-y-4 pt-2">
                      {advanced.map((paramObj) => (
                        <ParamField
                          key={paramObj.name}
                          paramObj={paramObj}
                          control={form.control}
                        />
                      ))}
                    </CollapsibleContent>
                  </Collapsible>
                )}
              </>
            )
          })()}

        <BackupWarning control={form.control} serviceType={serviceType} />

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
