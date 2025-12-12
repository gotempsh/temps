import {
  deleteProvider,
  listProviders,
  testProviderConnection,
  type DnsProviderResponse,
} from '@/api/client'
import { EmptyPlaceholder } from '@/components/EmptyPlaceholder'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  CheckCircle2,
  Cloud,
  Globe,
  Loader2,
  MoreVertical,
  Plus,
  RefreshCw,
  TestTube2,
  Trash2,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

// AWS icon component
function AwsIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M6.763 10.036c0 .296.032.535.088.71.064.176.144.368.256.576.04.063.056.127.056.183 0 .08-.048.16-.152.24l-.503.335a.383.383 0 0 1-.208.072c-.08 0-.16-.04-.239-.112a2.47 2.47 0 0 1-.287-.375 6.18 6.18 0 0 1-.248-.471c-.622.734-1.405 1.101-2.347 1.101-.67 0-1.205-.191-1.596-.574-.391-.384-.59-.894-.59-1.533 0-.678.239-1.23.726-1.644.487-.415 1.133-.623 1.955-.623.272 0 .551.024.846.064.296.04.6.104.918.176v-.583c0-.607-.127-1.03-.375-1.277-.255-.248-.686-.367-1.3-.367-.28 0-.568.031-.863.103-.295.072-.583.16-.863.272a2.287 2.287 0 0 1-.28.104.488.488 0 0 1-.127.023c-.112 0-.168-.08-.168-.247v-.391c0-.128.016-.224.056-.28a.597.597 0 0 1 .224-.167c.279-.144.614-.264 1.005-.36a4.84 4.84 0 0 1 1.246-.151c.95 0 1.644.216 2.091.647.439.43.662 1.085.662 1.963v2.586zm-3.24 1.214c.263 0 .534-.048.822-.144.287-.096.543-.271.758-.51.128-.152.224-.32.272-.512.047-.191.08-.423.08-.694v-.335a6.66 6.66 0 0 0-.735-.136 6.02 6.02 0 0 0-.75-.048c-.535 0-.926.104-1.19.32-.263.215-.39.518-.39.917 0 .375.095.655.295.846.191.2.47.296.838.296zm6.41.862c-.144 0-.24-.024-.304-.08-.064-.048-.12-.16-.168-.311L7.586 5.55a1.398 1.398 0 0 1-.072-.32c0-.128.064-.2.191-.2h.783c.151 0 .255.025.31.08.065.048.113.16.16.312l1.342 5.284 1.245-5.284c.04-.16.088-.264.151-.312a.549.549 0 0 1 .32-.08h.638c.152 0 .256.025.32.08.063.048.12.16.151.312l1.261 5.348 1.381-5.348c.048-.16.104-.264.16-.312a.52.52 0 0 1 .311-.08h.743c.127 0 .2.065.2.2 0 .04-.009.08-.017.128a1.137 1.137 0 0 1-.056.2l-1.923 6.17c-.048.16-.104.263-.168.311a.51.51 0 0 1-.303.08h-.687c-.151 0-.255-.024-.32-.08-.063-.056-.119-.16-.15-.32l-1.238-5.148-1.23 5.14c-.04.16-.087.264-.15.32-.065.056-.177.08-.32.08zm10.256.215c-.415 0-.83-.048-1.229-.143-.399-.096-.71-.2-.918-.32-.128-.071-.215-.151-.247-.223a.563.563 0 0 1-.048-.224v-.407c0-.167.064-.247.183-.247.048 0 .096.008.144.024.048.016.12.048.2.08.271.12.566.215.878.279.319.064.63.096.95.096.502 0 .894-.088 1.165-.264a.86.86 0 0 0 .415-.758.777.777 0 0 0-.215-.559c-.144-.151-.415-.287-.806-.407l-1.157-.36c-.583-.183-1.014-.454-1.277-.813a1.902 1.902 0 0 1-.4-1.158c0-.335.073-.63.216-.886.144-.255.335-.479.575-.654.24-.184.51-.32.83-.415.32-.096.655-.136 1.006-.136.176 0 .359.008.535.032.183.024.35.056.518.088.16.04.312.08.455.127.144.048.256.096.336.144a.69.69 0 0 1 .24.2.43.43 0 0 1 .071.263v.375c0 .168-.064.256-.184.256a.83.83 0 0 1-.303-.096 3.652 3.652 0 0 0-1.532-.311c-.455 0-.815.071-1.062.223-.248.152-.375.383-.375.71 0 .224.08.416.24.567.159.152.454.304.877.44l1.134.358c.574.184.99.44 1.237.767.247.327.367.702.367 1.117 0 .343-.072.655-.207.926-.144.272-.336.511-.583.703-.248.2-.543.343-.886.447-.36.111-.734.167-1.142.167zM21.698 16.207c-2.626 1.94-6.442 2.969-9.722 2.969-4.598 0-8.74-1.7-11.87-4.526-.247-.223-.024-.527.27-.351 3.384 1.963 7.559 3.153 11.877 3.153 2.914 0 6.114-.607 9.06-1.852.439-.2.814.287.385.607zM22.792 14.961c-.336-.43-2.22-.207-3.074-.103-.255.032-.295-.192-.063-.36 1.5-1.053 3.967-.75 4.254-.399.287.36-.08 2.826-1.485 4.007-.216.184-.423.088-.327-.151.32-.79 1.03-2.57.695-2.994z" />
    </svg>
  )
}

// DigitalOcean icon component
function DigitalOceanIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M12.04 24v-4.78a7.22 7.22 0 0 0 0-14.44A7.23 7.23 0 0 0 4.71 12h4.82V7.17a4.89 4.89 0 1 1 2.51 9.09v4.78h.02-4.82v-3.64h-3.63v3.64H0v-3.64 3.64H0V12a12 12 0 1 1 12.04 12z" />
    </svg>
  )
}

// Google Cloud icon component
function GcpIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M12.19 2.38a9.344 9.344 0 0 0-9.234 6.893c.053-.02-.055.013 0 0-3.875 2.551-3.922 8.11-.247 10.941l.006-.007-.007.03a6.717 6.717 0 0 0 4.077 1.356h5.173l.03.03h5.192c6.687.053 9.376-8.605 3.835-12.35a9.365 9.365 0 0 0-8.825-6.893zM8.073 19.28a4.407 4.407 0 0 1-2.463-4.014c.013-.03.03-.042.03-.064v-.043c0-.263.264-1.26.264-1.26l.03-.03.01-.03a4.392 4.392 0 0 1 2.403-2.633l.026-.012v.02a2.643 2.643 0 0 1 .95-.187c.69 0 1.33.266 1.807.698a5.44 5.44 0 0 1 1.108 1.61 4.413 4.413 0 0 1-4.165 5.944zm8.12-2.065a2.643 2.643 0 0 1-.95.187c-.702 0-1.358-.276-1.83-.732l.004-.007a5.308 5.308 0 0 1-1.1-1.586 4.413 4.413 0 0 1 4.166-5.944 4.38 4.38 0 0 1 2.462 4.015v.042c0 .264-.264 1.26-.264 1.26l-.03.03-.01.03a4.404 4.404 0 0 1-2.448 2.704z" />
    </svg>
  )
}

// Azure icon component
function AzureIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M5.483 21.3H24L14.025 4.013l-3.038 8.347 5.836 6.938L5.483 21.3zM13.23 2.7L6.105 8.677 0 19.253h5.505v.014L13.23 2.7z" />
    </svg>
  )
}

// Helper function to get provider icon
function getProviderIcon(providerType: string) {
  switch (providerType.toLowerCase()) {
    case 'cloudflare':
      return <Cloud className="h-4 w-4 text-orange-500" />
    case 'route53':
      return <AwsIcon className="h-4 w-4 text-amber-600" />
    case 'gcp':
      return <GcpIcon className="h-4 w-4 text-blue-500" />
    case 'azure':
      return <AzureIcon className="h-4 w-4 text-sky-500" />
    case 'digitalocean':
      return <DigitalOceanIcon className="h-4 w-4 text-blue-600" />
    default:
      return <Globe className="h-4 w-4" />
  }
}

// Helper function to format provider type for display
function formatProviderType(type: string): string {
  switch (type.toLowerCase()) {
    case 'cloudflare':
      return 'Cloudflare'
    case 'namecheap':
      return 'Namecheap'
    case 'route53':
      return 'AWS Route 53'
    case 'gcp':
      return 'Google Cloud DNS'
    case 'azure':
      return 'Azure DNS'
    case 'digitalocean':
      return 'DigitalOcean'
    default:
      return type.charAt(0).toUpperCase() + type.slice(1)
  }
}

export function DnsProviders() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [providerToDelete, setProviderToDelete] =
    useState<DnsProviderResponse | null>(null)

  const {
    data: dnsProviders,
    isLoading,
    error,
    refetch,
  } = useQuery({
    queryKey: ['dnsProviders'],
    queryFn: async () => {
      const response = await listProviders()
      return response.data
    },
    retry: false,
  })

  const deleteProviderMut = useMutation({
    mutationFn: (id: number) => deleteProvider({ path: { id } }),
    onSuccess: () => {
      toast.success('DNS provider deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['dnsProviders'] })
      setProviderToDelete(null)
    },
    onError: (error: Error) => {
      toast.error('Failed to delete DNS provider', {
        description: error.message,
      })
    },
  })

  const testConnectionMut = useMutation({
    mutationFn: async (id: number) => {
      const response = await testProviderConnection({ path: { id } })
      return response.data
    },
    onSuccess: (result) => {
      if (result?.success) {
        toast.success('Connection test successful', {
          description: result.message,
        })
      } else {
        toast.error('Connection test failed', {
          description: result?.message,
        })
      }
    },
    onError: (error: Error) => {
      toast.error('Connection test failed', {
        description: error.message,
      })
    },
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'DNS Providers' }])
  }, [setBreadcrumbs])

  // Keyboard shortcut: N to add new DNS provider
  useKeyboardShortcut({ key: 'n', path: '/dns-providers/add' })

  usePageTitle('DNS Providers')

  const handleDeleteProvider = async () => {
    if (!providerToDelete) return
    deleteProviderMut.mutate(providerToDelete.id)
  }

  const handleTestConnection = (provider: DnsProviderResponse) => {
    testConnectionMut.mutate(provider.id)
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-6">
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-2xl font-bold">DNS Providers</h1>
            <p className="text-muted-foreground">
              Manage your DNS providers for automatic DNS record configuration
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => refetch()}>
              <RefreshCw className="mr-2 h-4 w-4" />
              Refresh
            </Button>
            <Button onClick={() => navigate('/dns-providers/add')}>
              <Plus className="mr-2 h-4 w-4" />
              Add DNS Provider
              <KbdBadge keys="N" className="ml-2" />
            </Button>
          </div>
        </div>

        {error ? (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Error</AlertTitle>
            <AlertDescription>
              Failed to load DNS providers. Please try again later or contact
              support if the issue persists.
            </AlertDescription>
          </Alert>
        ) : (
          <Card>
            <CardHeader>
              <CardTitle>Active Providers</CardTitle>
            </CardHeader>
            <CardContent>
              {isLoading ? (
                <div className="grid gap-4">
                  {Array.from({ length: 3 }).map((_, i) => (
                    <div
                      key={i}
                      className="p-4 border rounded-lg space-y-3 animate-pulse"
                    >
                      <div className="flex items-center justify-between">
                        <div className="h-5 w-48 bg-muted rounded" />
                        <div className="h-6 w-20 bg-muted rounded" />
                      </div>
                      <div className="grid grid-cols-2 gap-4">
                        <div className="space-y-2">
                          <div className="h-4 w-24 bg-muted rounded" />
                          <div className="h-4 w-32 bg-muted rounded" />
                        </div>
                        <div className="space-y-2">
                          <div className="h-4 w-24 bg-muted rounded" />
                          <div className="h-4 w-32 bg-muted rounded" />
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              ) : !dnsProviders?.length ? (
                <EmptyPlaceholder
                  icon={Globe}
                  title="No DNS providers found"
                  description="Get started by adding a DNS provider like Cloudflare or Namecheap to enable automatic DNS record management"
                >
                  <Button onClick={() => navigate('/dns-providers/add')}>
                    <Plus className="mr-2 h-4 w-4" />
                    Add DNS Provider
                  </Button>
                </EmptyPlaceholder>
              ) : (
                <div className="grid gap-4">
                  {dnsProviders.map((provider) => (
                    <div
                      key={provider.id}
                      className="group relative p-4 border rounded-lg transition-colors hover:bg-muted/50 cursor-pointer"
                      onClick={() =>
                        navigate(`/dns-providers/${provider.id}`)
                      }
                    >
                      <div className="flex flex-col sm:flex-row sm:items-center gap-4">
                        <div className="flex-1 min-w-0 space-y-1">
                          <div className="flex items-center gap-3">
                            {getProviderIcon(provider.provider_type)}
                            <span className="font-medium truncate">
                              {provider.name}
                            </span>
                            <Badge variant="outline">
                              {formatProviderType(provider.provider_type)}
                            </Badge>
                            {provider.is_active ? (
                              <Badge
                                variant="secondary"
                                className="flex items-center gap-1"
                              >
                                <CheckCircle2 className="h-3 w-3" />
                                Active
                              </Badge>
                            ) : (
                              <Badge
                                variant="destructive"
                                className="flex items-center gap-1"
                              >
                                <XCircle className="h-3 w-3" />
                                Inactive
                              </Badge>
                            )}
                          </div>
                          <div className="grid grid-cols-1 sm:flex sm:items-center gap-x-6 gap-y-1 text-sm text-muted-foreground">
                            {provider.description && (
                              <div className="flex items-center gap-2">
                                <span className="truncate max-w-[300px]">
                                  {provider.description}
                                </span>
                              </div>
                            )}
                            {provider.last_error && (
                              <div className="flex items-center gap-2 text-destructive">
                                <AlertCircle className="h-4 w-4" />
                                <span className="truncate max-w-[200px]">
                                  {provider.last_error}
                                </span>
                              </div>
                            )}
                            <div className="flex items-center gap-2">
                              <span>Created </span>
                              <TimeAgo
                                date={provider.created_at}
                                className=""
                              />
                            </div>
                          </div>
                        </div>
                        <div
                          className="flex items-center gap-2"
                          onClick={(e) => e.stopPropagation()}
                        >
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => handleTestConnection(provider)}
                            disabled={testConnectionMut.isPending}
                            className="gap-2"
                          >
                            {testConnectionMut.isPending ? (
                              <Loader2 className="h-4 w-4 animate-spin" />
                            ) : (
                              <TestTube2 className="h-4 w-4" />
                            )}
                            Test
                          </Button>
                          <DropdownMenu>
                            <DropdownMenuTrigger asChild>
                              <Button variant="ghost" size="icon">
                                <MoreVertical className="h-4 w-4" />
                              </Button>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent align="end">
                              <DropdownMenuItem
                                onClick={() =>
                                  navigate(`/dns-providers/${provider.id}`)
                                }
                              >
                                View Details
                              </DropdownMenuItem>
                              <DropdownMenuItem
                                onClick={() => handleTestConnection(provider)}
                              >
                                <TestTube2 className="h-4 w-4 mr-2" />
                                Test Connection
                              </DropdownMenuItem>
                              <DropdownMenuSeparator />
                              <DropdownMenuItem
                                className="text-destructive cursor-pointer"
                                onSelect={(e) => {
                                  e.preventDefault()
                                  setProviderToDelete(provider)
                                }}
                              >
                                <Trash2 className="h-4 w-4 mr-2" />
                                Delete Provider
                              </DropdownMenuItem>
                            </DropdownMenuContent>
                          </DropdownMenu>
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </CardContent>
          </Card>
        )}
      </div>

      {/* Delete Confirmation Dialog */}
      <AlertDialog
        open={!!providerToDelete}
        onOpenChange={(open) => !open && setProviderToDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete DNS Provider</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to delete &quot;{providerToDelete?.name}
              &quot;? This action cannot be undone and will remove all
              associated managed domains.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel onClick={() => setProviderToDelete(null)}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleteProviderMut.isPending}
              onClick={handleDeleteProvider}
            >
              {deleteProviderMut.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Deleting...
                </>
              ) : (
                'Delete Provider'
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
