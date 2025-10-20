import {
  deleteProviderSafelyMutation,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { checkProviderDeletionSafety } from '@/api/client/sdk.gen'
import { ProviderResponse } from '@/api/client/types.gen'
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
  AlertDialogTrigger,
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
import { FeedbackAlert } from '@/components/ui/feedback-alert'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useFeedback } from '@/hooks/useFeedback'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  CheckCircle2,
  GitBranch,
  GithubIcon,
  Link as LinkIcon,
  Loader2,
  MoreVertical,
  Plus,
  RefreshCw,
  Trash2,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

// Helper function to check if provider is GitHub App
const isGitHubApp = (provider: ProviderResponse) =>
  provider.provider_type === 'github' &&
  (provider.auth_method === 'app' || provider.auth_method === 'github_app')

export function GitSources() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const { feedback, showSuccess, clearFeedback } = useFeedback()
  const queryClient = useQueryClient()
  const [providerToDelete, setProviderToDelete] =
    useState<ProviderResponse | null>(null)

  const {
    data: gitProviders,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...listGitProvidersOptions({}),
    retry: false,
  })

  const deleteProviderMut = useMutation({
    ...deleteProviderSafelyMutation(),
    meta: {
      errorTitle: 'Failed to remove git provider',
    },
    onSuccess: () => {
      toast.success('Git provider removed successfully')
      refetch()
      setProviderToDelete(null)
    },
  })

  const handleInstallGitHubApp = (provider: ProviderResponse) => {
    // For GitHub App providers, construct the installation URL directly
    if (isGitHubApp(provider)) {
      // Extract GitHub App URL from provider name or use default GitHub
      const baseUrl = provider.base_url || 'https://github.com'

      // Open GitHub App installation page in new tab
      const installUrl = `${baseUrl}/installations/new`
      window.open(installUrl, '_blank', 'noopener,noreferrer')

      showSuccess('Opening GitHub App installation in new tab')
    }
  }

  const handleConfirmDeleteProvider = async () => {
    if (!providerToDelete) return

    try {
      // First check if the provider can be deleted
      const checkResult = await checkProviderDeletionSafety({
        path: { provider_id: providerToDelete.id },
      })
      if (checkResult.error) {
        toast.error('Failed to check provider', {
          description: (checkResult.error as any).detail,
          duration: 6000,
        })
        setProviderToDelete(null)
        return
      }
      const checkResultData = checkResult.data
      if (!checkResultData) {
        toast.error('Failed to check provider', {
          description: 'An unexpected error occurred',
          duration: 6000,
        })
        setProviderToDelete(null)
        return
      }
      // If provi	der cannot be deleted, show error and return
      if (!checkResultData.can_delete) {
        toast.error('Cannot delete provider', {
          description: checkResultData.message,
          duration: 6000,
        })
        setProviderToDelete(null)
        return
      }

      // If provider can be deleted, proceed with deletion
      await toast.promise(
        deleteProviderMut.mutateAsync({
          path: { provider_id: providerToDelete.id },
        }),
        {
          loading: 'Removing Git provider...',
          success: 'Git provider removed successfully',
          error: 'Failed to remove provider',
        }
      )

      // Refresh the provider list after successful deletion
      queryClient.invalidateQueries({ queryKey: ['listGitProviders'] })
    } catch (error) {
      // Handle any errors that occur during the check
      toast.error('Failed to check provider', {
        description:
          error instanceof Error
            ? error.message
            : 'An unexpected error occurred',
      })
    } finally {
      setProviderToDelete(null)
    }
  }

  useEffect(() => {
    setBreadcrumbs([{ label: 'Git Providers' }])
  }, [setBreadcrumbs])

  usePageTitle('Git Providers')

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-6">
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-2xl font-bold">Git Providers</h1>
            <p className="text-muted-foreground">
              Manage your Git providers for repository access and deployments
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => refetch()}>
              <RefreshCw className="mr-2 h-4 w-4" />
              Refresh
            </Button>
            <Button onClick={() => navigate('/git-sources/add')}>
              <Plus className="mr-2 h-4 w-4" />
              Add Git Provider
            </Button>
          </div>
        </div>

        {/* Feedback Alert */}
        <FeedbackAlert feedback={feedback} onDismiss={clearFeedback} />

        {error ? (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Error</AlertTitle>
            <AlertDescription>
              Failed to load Git providers. Please try again later or contact
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
              ) : !gitProviders?.length ? (
                <EmptyPlaceholder
                  icon={GitBranch}
                  title="No git providers found"
                  description="Get started by setting up a Git provider like GitHub or GitLab"
                >
                  <Button onClick={() => navigate('/git-sources/add')}>
                    <Plus className="mr-2 h-4 w-4" />
                    Add Git Provider
                  </Button>
                </EmptyPlaceholder>
              ) : (
                <div className="grid gap-4">
                  {gitProviders.map((provider: ProviderResponse) => (
                    <div
                      key={provider.id}
                      className="group relative p-4 border rounded-lg transition-colors hover:bg-muted/50 cursor-pointer"
                      onClick={() => navigate(`/git-providers/${provider.id}`)}
                    >
                      <div className="flex flex-col sm:flex-row sm:items-center gap-4">
                        <div className="flex-1 min-w-0 space-y-1">
                          <div className="flex items-center gap-3">
                            {provider.provider_type === 'github' ? (
                              <GithubIcon className="h-4 w-4" />
                            ) : (
                              <GitBranch className="h-4 w-4" />
                            )}
                            <span className="font-medium truncate">
                              {provider.name}
                            </span>
                            <Badge variant="outline">
                              {provider.provider_type.charAt(0).toUpperCase() +
                                provider.provider_type.slice(1)}
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
                            {provider.is_default && (
                              <Badge
                                variant="outline"
                                className="flex items-center gap-1"
                              >
                                <span>Default</span>
                              </Badge>
                            )}
                          </div>
                          <div className="grid grid-cols-1 sm:flex sm:items-center gap-x-6 gap-y-1 text-sm text-muted-foreground">
                            <div className="flex items-center gap-2">
                              <span>Method: {provider.auth_method}</span>
                            </div>
                            {provider.base_url && (
                              <div className="flex items-center gap-2">
                                <LinkIcon className="h-4 w-4" />
                                <span className="truncate">
                                  {provider.base_url}
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
                          {isGitHubApp(provider) && (
                            <Button
                              variant={
                                provider.is_active ? 'outline' : 'default'
                              }
                              size="sm"
                              onClick={() => handleInstallGitHubApp(provider)}
                              className="gap-2"
                            >
                              <GithubIcon className="h-4 w-4" />
                              Install GitHub App
                            </Button>
                          )}
                          <DropdownMenu>
                            <DropdownMenuTrigger asChild>
                              <Button variant="ghost" size="icon">
                                <MoreVertical className="h-4 w-4" />
                              </Button>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent align="end">
                              {isGitHubApp(provider) && (
                                <DropdownMenuItem
                                  onClick={() =>
                                    handleInstallGitHubApp(provider)
                                  }
                                >
                                  <GithubIcon className="h-4 w-4 mr-2" />
                                  Install GitHub App
                                </DropdownMenuItem>
                              )}
                              {isGitHubApp(provider) && (
                                <DropdownMenuSeparator />
                              )}
                              <AlertDialog>
                                <AlertDialogTrigger asChild>
                                  <DropdownMenuItem
                                    className="text-destructive cursor-pointer"
                                    onSelect={(e) => {
                                      e.preventDefault()
                                      setProviderToDelete(provider)
                                    }}
                                  >
                                    <Trash2 className="h-4 w-4 mr-2" />
                                    Remove Provider
                                  </DropdownMenuItem>
                                </AlertDialogTrigger>
                                <AlertDialogContent>
                                  <AlertDialogHeader>
                                    <AlertDialogTitle>
                                      Remove Git Provider
                                    </AlertDialogTitle>
                                    <AlertDialogDescription>
                                      Are you sure you want to remove &quot;
                                      {provider.name}&quot;? This action cannot
                                      be undone and will remove all associated
                                      connections and repositories.
                                    </AlertDialogDescription>
                                  </AlertDialogHeader>
                                  <AlertDialogFooter>
                                    <AlertDialogCancel
                                      onClick={() => {
                                        setProviderToDelete(null)
                                      }}
                                    >
                                      Cancel
                                    </AlertDialogCancel>
                                    <AlertDialogAction
                                      className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
                                      disabled={deleteProviderMut.isPending}
                                      onClick={handleConfirmDeleteProvider}
                                    >
                                      {deleteProviderMut.isPending ? (
                                        <>
                                          <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                          Removing...
                                        </>
                                      ) : (
                                        'Remove Provider'
                                      )}
                                    </AlertDialogAction>
                                  </AlertDialogFooter>
                                </AlertDialogContent>
                              </AlertDialog>
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
    </div>
  )
}
