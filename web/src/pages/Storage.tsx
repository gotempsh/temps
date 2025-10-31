import { listServicesOptions } from '@/api/client/@tanstack/react-query.gen'
import { ServiceType } from '@/api/client/types.gen'
import { CreateServiceButton } from '@/components/storage/CreateServiceButton'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'
import { DeleteServiceButton } from '@/components/storage/DeleteServiceButton'
import EmptyStateStorage from '@/components/storage/EmptyStateStorage'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, RefreshCcw } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { TimeAgo } from '@/components/utils/TimeAgo'

export function Storage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)
  const [selectedServiceType, setSelectedServiceType] =
    useState<ServiceType | null>(null)

  const {
    data: services,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...listServicesOptions(),
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Storage', href: '/storage' }])
  }, [setBreadcrumbs])

  // Keyboard shortcut: N to create new service (navigate to create page)
  useKeyboardShortcut({ key: 'n', path: '/storage/create' })

  usePageTitle('Storage')

  if (isLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6">
          <div className="flex items-center justify-between">
            <div className="h-8 w-32 bg-muted rounded animate-pulse" />
            <div className="h-9 w-24 bg-muted rounded animate-pulse" />
          </div>
          <div className="grid gap-4">
            {[...Array(3)].map((_, i) => (
              <Card key={i}>
                <CardHeader>
                  <div className="flex items-center justify-between">
                    <div className="space-y-2">
                      <div className="h-5 w-40 bg-muted rounded animate-pulse" />
                      <div className="h-4 w-24 bg-muted rounded animate-pulse" />
                    </div>
                    <div className="h-8 w-20 bg-muted rounded animate-pulse" />
                  </div>
                </CardHeader>
                <CardContent>
                  <div className="h-4 w-full bg-muted rounded animate-pulse" />
                </CardContent>
              </Card>
            ))}
          </div>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6">
          <div className="flex flex-col items-center justify-center py-12 text-center">
            <p className="text-sm text-muted-foreground mb-4">
              Failed to load services
            </p>
            <Button
              variant="outline"
              onClick={() => refetch()}
              className="gap-2"
            >
              <RefreshCcw className="h-4 w-4" />
              Try again
            </Button>
          </div>
        </div>
      </div>
    )
  }

  if (!services?.length) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6">
          <EmptyStateStorage
            onCreateClick={(serviceType) => {
              setSelectedServiceType(serviceType)
              setIsCreateDialogOpen(true)
            }}
          />
        </div>

        <CreateServiceDialog
          open={isCreateDialogOpen && !!selectedServiceType}
          onOpenChange={(open) => {
            setIsCreateDialogOpen(open)
            if (!open) {
              setSelectedServiceType(null)
            }
          }}
          onSuccess={() => {
            setIsCreateDialogOpen(false)
            setSelectedServiceType(null)
            refetch()
          }}
          serviceType={selectedServiceType!}
        />
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-4 space-y-6 md:p-6">
        <div className="flex items-center justify-between">
          <h1 className="text-xl font-semibold sm:text-2xl">Storage</h1>
          <CreateServiceButton onSuccess={() => refetch()} />
        </div>

        <div className="grid gap-4">
          {services.map((service) => (
            <Card
              key={service.id}
              onClick={() => navigate(`/storage/${service.id}`)}
              className="cursor-pointer transition-colors hover:bg-muted/50"
            >
              <CardHeader>
                <div className="flex items-center justify-between">
                  <div className="space-y-1">
                    <CardTitle className="flex items-center gap-2">
                      <ServiceLogo service={service.service_type} />
                      {service.name}
                    </CardTitle>
                    <CardDescription className="flex items-center gap-2">
                      <span>{service.service_type}</span>
                      <span>•</span>
                      <span>
                        Created <TimeAgo date={service.created_at} />
                      </span>
                    </CardDescription>
                  </div>
                  <DeleteServiceButton
                    serviceId={service.id}
                    serviceName={service.name}
                    onSuccess={() => refetch()}
                  />
                </div>
              </CardHeader>
              <CardContent>
                <div className="flex items-center text-sm text-muted-foreground">
                  View details
                  <ArrowRight className="ml-1 h-4 w-4" />
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      </div>
    </div>
  )
}
