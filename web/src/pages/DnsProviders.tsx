import {
  deleteDnsProvider as deleteProvider,
  listDnsProviders as listProviders,
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
import { CreateActionButton } from '@/components/ui/create-action-button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  CheckCircle2,
  ChevronRight,
  Globe,
  Loader2,
  MoreVertical,
  Plus,
  RefreshCw,
  TestTube2,
  Trash2,
  XCircle,
} from 'lucide-react'
import { getDnsProviderIcon } from '@/components/icons/DnsProviderIcons'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

// Helper function to get provider icon
function getProviderIcon(providerType: string) {
  return getDnsProviderIcon(providerType, 'h-4 w-4')
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
      <div className="space-y-6 p-4 sm:p-6">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <h1 className="text-xl font-bold sm:text-2xl">DNS Providers</h1>
            <p className="text-sm text-muted-foreground sm:text-base">
              Manage your DNS providers for automatic DNS record configuration
            </p>
          </div>
          <div className="flex items-center gap-2 self-start sm:self-auto">
            <Button
              variant="outline"
              size="icon"
              onClick={() => refetch()}
              aria-label="Refresh"
              title="Refresh"
            >
              <RefreshCw className="h-4 w-4" />
            </Button>
            <CreateActionButton
              size="sm"
              onClick={() => navigate('/dns-providers/add')}
              label="Add DNS Provider"
            />
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
        ) : isLoading ? (
          <div className="divide-y rounded-lg border">
            {Array.from({ length: 3 }).map((_, i) => (
              <div
                key={i}
                className="flex items-center gap-4 px-4 py-3 animate-pulse"
              >
                <div className="size-9 shrink-0 rounded-md bg-muted" />
                <div className="flex-1 min-w-0 space-y-1.5">
                  <div className="h-4 w-48 bg-muted rounded" />
                  <div className="h-3 w-64 bg-muted rounded" />
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
          <div className="overflow-hidden rounded-lg border">
            <ul role="list" className="divide-y">
              {dnsProviders.map((provider) => (
                <li
                  key={provider.id}
                  role="button"
                  tabIndex={0}
                  onClick={() => navigate(`/dns-providers/${provider.id}`)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault()
                      navigate(`/dns-providers/${provider.id}`)
                    }
                  }}
                  className="flex cursor-pointer items-center gap-3 px-3 py-3 transition-colors hover:bg-muted/40 focus:bg-muted/40 focus:outline-none sm:gap-4 sm:px-4"
                >
                  <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted">
                    {getProviderIcon(provider.provider_type)}
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <p className="truncate text-sm font-medium">
                        {provider.name}
                      </p>
                      <Badge variant="secondary" className="text-xs">
                        {formatProviderType(provider.provider_type)}
                      </Badge>
                      {provider.is_active ? (
                        <Badge
                          variant="outline"
                          className="flex items-center gap-1 text-xs"
                        >
                          <CheckCircle2 className="h-3 w-3" />
                          Active
                        </Badge>
                      ) : (
                        <Badge
                          variant="destructive"
                          className="flex items-center gap-1 text-xs"
                        >
                          <XCircle className="h-3 w-3" />
                          Inactive
                        </Badge>
                      )}
                    </div>
                    {provider.last_error ? (
                      <p className="mt-0.5 flex items-center gap-1 truncate text-xs text-destructive">
                        <AlertCircle className="h-3.5 w-3.5 shrink-0" />
                        <span className="truncate">{provider.last_error}</span>
                      </p>
                    ) : (
                      <p className="mt-0.5 truncate text-xs text-muted-foreground">
                        {provider.description
                          ? `${provider.description} · `
                          : ''}
                        created <TimeAgo date={provider.created_at} />
                      </p>
                    )}
                  </div>
                  <div
                    className="flex shrink-0 items-center gap-1 sm:gap-2"
                    onClick={(e) => e.stopPropagation()}
                    onPointerDown={(e) => e.stopPropagation()}
                  >
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => handleTestConnection(provider)}
                      disabled={testConnectionMut.isPending}
                      className="hidden gap-2 sm:inline-flex"
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
                        <Button variant="ghost" size="icon" className="h-8 w-8">
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
                  <ChevronRight className="hidden size-4 shrink-0 text-muted-foreground/50 sm:block" />
                </li>
              ))}
            </ul>
          </div>
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
