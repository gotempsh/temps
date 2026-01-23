import { memo, useCallback } from 'react'
import { useFormContext } from 'react-hook-form'
import { FormField, FormItem, FormMessage } from '@/components/ui/form'
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { ServiceLogo } from '@/components/ui/service-logo'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Plus, ChevronDown } from 'lucide-react'
import { format } from 'date-fns'
import { ServiceTypeRoute } from '@/api/client/types.gen'
import { cn } from '@/lib/utils'
import { toast } from 'sonner'

const SERVICE_TYPES = [
  {
    id: 'postgres' as ServiceTypeRoute,
    name: 'PostgreSQL',
    description: 'Reliable Relational Database',
  },
  {
    id: 'redis' as ServiceTypeRoute,
    name: 'Redis',
    description: 'In-Memory Data Store',
  },
  { id: 's3' as ServiceTypeRoute, name: 'S3', description: 'Object Storage' },
  {
    id: 'libsql' as ServiceTypeRoute,
    name: 'LibSQL',
    description: 'SQLite-compatible Database',
  },
]

interface ServicesStepProps {
  existingServices?: any[]
  newlyCreatedServiceIds: number[]
  newlyCreatedServiceTypes: ServiceTypeRoute[]
  onServiceToggle: (serviceId: number) => void
  onCreateService: (serviceType: ServiceTypeRoute) => void
}

export const ServicesStep = memo(function ServicesStep({
  existingServices,
  newlyCreatedServiceIds,
  newlyCreatedServiceTypes,
  onServiceToggle,
  onCreateService,
}: ServicesStepProps) {
  const form = useFormContext()

  // Get the service types that are already selected (either existing or newly created)
  const getSelectedServiceTypes = useCallback((): Set<string> => {
    const currentServices = form.getValues('storageServices') || []
    const selectedTypes = new Set<string>()

    // Add types from selected existing services
    currentServices.forEach((serviceId: number) => {
      const service = existingServices?.find((s: any) => s.id === serviceId)
      if (service) {
        selectedTypes.add(service.service_type)
      }
    })

    // Add types from newly created services
    newlyCreatedServiceTypes.forEach((serviceType) => {
      selectedTypes.add(serviceType)
    })

    return selectedTypes
  }, [form, existingServices, newlyCreatedServiceTypes])

  // Handle service toggle with type collision check
  const handleServiceToggleWithValidation = useCallback(
    (serviceId: number) => {
      const currentServices = form.getValues('storageServices') || []
      const isSelected = currentServices.includes(serviceId)

      // If trying to select (not deselect), check for type collision
      if (!isSelected) {
        const serviceToAdd = existingServices?.find(
          (s: any) => s.id === serviceId
        )
        if (serviceToAdd) {
          const selectedTypes = getSelectedServiceTypes()
          if (selectedTypes.has(serviceToAdd.service_type)) {
            toast.error(
              `A ${serviceToAdd.service_type} service is already selected`,
              {
                description:
                  'Only one service of each type can be linked to a project to avoid environment variable conflicts.',
              }
            )
            return
          }
        }
      }

      onServiceToggle(serviceId)
    },
    [form, existingServices, getSelectedServiceTypes, onServiceToggle]
  )

  return (
    <div className="space-y-6">
      {/* Existing Services */}
      {existingServices && existingServices.length > 0 && (
        <div>
          <h4 className="font-medium mb-3">Existing Services</h4>
          <FormField
            control={form.control}
            name="storageServices"
            render={({ field }) => (
              <FormItem>
                <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                  {existingServices.map((service: any) => {
                    const isSelected = field.value?.includes(service.id)
                    return (
                      <Card
                        key={service.id}
                        className={`cursor-pointer transition-colors hover:bg-muted/50 ${
                          isSelected ? 'ring-2 ring-primary' : ''
                        }`}
                        onClick={() =>
                          handleServiceToggleWithValidation(service.id)
                        }
                      >
                        <CardHeader className="pb-2">
                          <div className="flex items-center justify-between">
                            <div className="space-y-1">
                              <CardTitle className="flex items-center gap-2 text-sm">
                                <ServiceLogo service={service.service_type} />
                                {service.name}
                              </CardTitle>
                              <CardDescription className="text-xs">
                                {service.service_type} • Created{' '}
                                {format(
                                  new Date(service.created_at),
                                  'MMM d, yyyy'
                                )}
                              </CardDescription>
                            </div>
                            <Checkbox checked={isSelected} />
                          </div>
                        </CardHeader>
                      </Card>
                    )
                  })}
                </div>
                <FormMessage />
              </FormItem>
            )}
          />
        </div>
      )}

      {/* New Services */}
      <div>
        <div className="flex items-center justify-between mb-3">
          <h4 className="font-medium">Create New Services</h4>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="sm">
                <Plus className="h-4 w-4 mr-2" />
                Add Service
                <ChevronDown className="h-4 w-4 ml-1" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-[240px]">
              {SERVICE_TYPES.map((type) => {
                const selectedTypes = getSelectedServiceTypes()
                const isTypeAlreadySelected = selectedTypes.has(type.id)
                return (
                  <DropdownMenuItem
                    key={type.id}
                    onClick={() => {
                      if (isTypeAlreadySelected) {
                        toast.error(
                          `A ${type.name} service is already selected`,
                          {
                            description:
                              'Only one service of each type can be linked to a project.',
                          }
                        )
                        return
                      }
                      onCreateService(type.id)
                    }}
                    className={cn(
                      'flex items-start gap-3 py-3',
                      isTypeAlreadySelected && 'opacity-50 cursor-not-allowed'
                    )}
                  >
                    <ServiceLogo service={type.id} />
                    <div className="flex flex-col">
                      <span className="font-medium">
                        {type.name}
                        {isTypeAlreadySelected && (
                          <span className="text-xs text-muted-foreground ml-2">
                            (already selected)
                          </span>
                        )}
                      </span>
                      <span className="text-xs text-muted-foreground">
                        {type.description}
                      </span>
                    </div>
                  </DropdownMenuItem>
                )
              })}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>

        {newlyCreatedServiceIds.length > 0 && (
          <div className="mt-3">
            <p className="text-sm text-muted-foreground mb-2">
              {newlyCreatedServiceIds.length} new service
              {newlyCreatedServiceIds.length > 1 ? 's' : ''} will be created
              with this project
            </p>
          </div>
        )}
      </div>
    </div>
  )
})
