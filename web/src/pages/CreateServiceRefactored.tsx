import {
  createServiceMutation,
  getProviderMetadataOptions,
  getServiceTypeParametersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ServiceTypeRoute } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { DynamicForm } from '@/components/forms/DynamicForm'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useMutation, useQuery } from '@tanstack/react-query'
import { customAlphabet } from 'nanoid'
import { ArrowLeft } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router-dom'
import { toast } from 'sonner'

// Create a custom nanoid with lowercase alphanumeric characters
const generateId = customAlphabet('0123456789abcdefghijklmnopqrstuvwxyz', 4)

export function CreateService() {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const serviceType = searchParams.get('type') as ServiceTypeRoute | null
  const { setBreadcrumbs } = useBreadcrumbs()

  const defaultName = useMemo(
    () => (serviceType ? `${serviceType}-${generateId()}` : ''),
    [serviceType]
  )

  const [serviceName, setServiceName] = useState(defaultName)

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

  // Fetch parameters for the selected service type
  const { data: parameters, isLoading: isLoadingParameters } = useQuery({
    ...getServiceTypeParametersOptions({
      path: {
        service_type: serviceType || '',
      },
    }),
    enabled: !!serviceType,
  })

  const createServiceMut = useMutation({
    ...createServiceMutation(),
    meta: {
      errorTitle: 'Failed to create service',
    },
    onSuccess: (data) => {
      toast.success('Service created successfully')
      navigate(`/storage/${data.id}`)
    },
  })

  const handleSubmit = async (parameterValues: Record<string, string>) => {
    if (!serviceName.trim()) {
      toast.error('Service name is required')
      return
    }

    await createServiceMut.mutateAsync({
      body: {
        service_type: serviceType as ServiceTypeRoute,
        name: serviceName,
        parameters: parameterValues,
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

  if (!parameters) {
    return null
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

        {/* Service Name Field */}
        <div className="space-y-2">
          <Label htmlFor="serviceName">
            Service Name
            <span className="text-destructive ml-1">*</span>
          </Label>
          <Input
            id="serviceName"
            value={serviceName}
            onChange={(e) => setServiceName(e.target.value)}
            placeholder={`my-${serviceType}`}
          />
          <p className="text-sm text-muted-foreground">
            A unique name to identify this service
          </p>
        </div>

        {/* Dynamic Form for Parameters */}
        <DynamicForm
          parameters={parameters}
          onSubmit={handleSubmit}
          onCancel={() => navigate('/storage')}
          submitText="Create Service"
          isSubmitting={createServiceMut.isPending}
        />
      </div>
    </div>
  )
}
