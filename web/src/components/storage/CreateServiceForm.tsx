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
  const { data: parameters, isLoading: isLoadingParameters } = useQuery({
    ...getServiceTypeParametersOptions({
      path: {
        service_type: serviceType,
      },
    }),
  })

  // Dynamically create the form schema based on parameters
  const formSchema = useMemo(
    () =>
      z.object({
        name: z.string().min(1, 'Name is required'),
        service_type: z.string(),
        parameters: z.record(z.string(), z.string()),
      }),
    []
  )

  type FormValues = z.infer<typeof formSchema>

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    mode: 'onSubmit',
    defaultValues: {
      name: defaultName,
      service_type: serviceType,
      parameters: {},
    },
  })

  // Set default values for parameters when they are loaded
  useEffect(() => {
    if (parameters) {
      const defaultParameters = parameters.reduce<Record<string, string>>(
        (acc, param) => {
          acc[param.name] = param.default_value || ''
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
    await createServiceMut.mutateAsync({
      body: {
        service_type: values.service_type as ServiceTypeRoute,
        name: values.name,
        parameters: values.parameters as Record<string, string>,
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

        {parameters?.map((param) => (
          <FormField
            key={param.name}
            control={form.control}
            name={`parameters.${param.name}`}
            render={({ field }) => (
              <FormItem>
                <FormLabel>
                  {param.name}
                  {param.required && (
                    <span className="text-destructive">*</span>
                  )}
                </FormLabel>
                <FormControl>
                  <Input
                    {...field}
                    value={field.value as string}
                    type={param.encrypted ? 'password' : 'text'}
                    required={param.required}
                    pattern={param.validation_pattern || undefined}
                    placeholder={param.default_value || undefined}
                  />
                </FormControl>
                {param.description && (
                  <p className="text-sm text-muted-foreground">
                    {param.description}
                  </p>
                )}
                <FormMessage />
              </FormItem>
            )}
          />
        ))}

        <div className="flex justify-end space-x-2">
          <Button
            type="button"
            variant="outline"
            onClick={onCancel}
            disabled={createServiceMut.isPending}
          >
            Cancel
          </Button>
          <Button type="submit" disabled={createServiceMut.isPending}>
            {createServiceMut.isPending ? 'Creating...' : 'Create Service'}
          </Button>
        </div>
      </form>
    </Form>
  )
}
