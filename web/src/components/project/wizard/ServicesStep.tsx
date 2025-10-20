import { memo } from 'react'
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
  onServiceToggle: (serviceId: number) => void
  onCreateService: (serviceType: ServiceTypeRoute) => void
}

export const ServicesStep = memo(function ServicesStep({
  existingServices,
  newlyCreatedServiceIds,
  onServiceToggle,
  onCreateService,
}: ServicesStepProps) {
  const form = useFormContext()

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
                        onClick={() => onServiceToggle(service.id)}
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
              {SERVICE_TYPES.map((type) => (
                <DropdownMenuItem
                  key={type.id}
                  onClick={() => onCreateService(type.id)}
                  className="flex items-start gap-3 py-3"
                >
                  <ServiceLogo service={type.id} />
                  <div className="flex flex-col">
                    <span className="font-medium">{type.name}</span>
                    <span className="text-xs text-muted-foreground">
                      {type.description}
                    </span>
                  </div>
                </DropdownMenuItem>
              ))}
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
