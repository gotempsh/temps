import { listServicesOptions } from '@/api/client/@tanstack/react-query.gen'
import { ExternalServiceInfo } from '@/api/client/types.gen'
import { CreateServiceButton } from '@/components/storage/CreateServiceButton'
import { DeleteServiceButton } from '@/components/storage/DeleteServiceButton'
import { EditServiceDialog } from '@/components/storage/EditServiceDialog'
import { ImportServiceButton } from '@/components/storage/ImportServiceButton'
import { PlatformServices } from '@/components/storage/PlatformServices'
import EmptyStateStorage from '@/components/storage/EmptyStateStorage'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, Database, HardDrive, Pencil, RefreshCcw } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { TimeAgo } from '@/components/utils/TimeAgo'

export function Storage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [selectedService, setSelectedService] = useState<ExternalServiceInfo | null>(null)

  // Get active tab from URL or default to 'external'
  const activeTab = searchParams.get('tab') || 'external'

  const handleTabChange = (value: string) => {
    setSearchParams({ tab: value })
  }

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

  // Render external services content based on loading/error/empty state
  const renderExternalServicesContent = () => {
    if (isLoading) {
      return (
        <div className="space-y-4">
          <div className="flex items-center justify-end">
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
      )
    }

    if (error) {
      return (
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
      )
    }

    if (!services?.length) {
      return <EmptyStateStorage />
    }

    return (
      <>
        <div className="flex items-center justify-end mb-4">
          <div className="flex items-center gap-2">
            <ImportServiceButton onSuccess={() => refetch()} />
            <CreateServiceButton onSuccess={() => refetch()} />
          </div>
        </div>

        <div className="grid gap-4">
          {services.map((service) => (
            <Card
              key={service.id}
              className="transition-colors hover:bg-muted/50"
            >
              <CardHeader>
                <div className="flex items-center justify-between">
                  <div
                    className="flex-1 cursor-pointer space-y-1"
                    onClick={() => navigate(`/storage/${service.id}`)}
                  >
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
                  <div className="flex items-center gap-2">
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={(e) => {
                        e.stopPropagation()
                        setSelectedService(service)
                        setIsEditDialogOpen(true)
                      }}
                      className="h-8 w-8"
                    >
                      <Pencil className="h-4 w-4" />
                    </Button>
                    <DeleteServiceButton
                      serviceId={service.id}
                      serviceName={service.name}
                      onSuccess={() => refetch()}
                    />
                  </div>
                </div>
              </CardHeader>
              <CardContent
                onClick={() => navigate(`/storage/${service.id}`)}
                className="cursor-pointer"
              >
                <div className="flex items-center text-sm text-muted-foreground">
                  View details
                  <ArrowRight className="ml-1 h-4 w-4" />
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      </>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-4 space-y-6 md:p-6">
        <div className="flex items-center justify-between">
          <h1 className="text-xl font-semibold sm:text-2xl">Storage</h1>
        </div>

        <Tabs value={activeTab} onValueChange={handleTabChange} className="space-y-6">
          <TabsList>
            <TabsTrigger value="platform" className="gap-2">
              <Database className="h-4 w-4" />
              Platform Services
            </TabsTrigger>
            <TabsTrigger value="external" className="gap-2">
              <HardDrive className="h-4 w-4" />
              External Services
            </TabsTrigger>
          </TabsList>

          <TabsContent value="platform" className="space-y-6">
            <PlatformServices />
          </TabsContent>

          <TabsContent value="external" className="space-y-6">
            {renderExternalServicesContent()}
          </TabsContent>
        </Tabs>

        {selectedService && (
          <EditServiceDialog
            open={isEditDialogOpen}
            onOpenChange={(open) => {
              setIsEditDialogOpen(open)
              if (!open) {
                setSelectedService(null)
              }
            }}
            service={selectedService}
            onSuccess={() => {
              setIsEditDialogOpen(false)
              setSelectedService(null)
              refetch()
            }}
          />
        )}
      </div>
    </div>
  )
}
