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
import { FeedbackAlert } from '@/components/ui/feedback-alert'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useFeedback } from '@/hooks/useFeedback'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ChevronRight,
  EllipsisVertical,
  GitBranch,
  GitFork,
  GithubIcon,
  Globe,
  Loader2,
  Plus,
  RefreshCw,
  Trash2,
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
    // NOTE: the success/error/loading toasts are owned by the `toast.promise`
    // wrapper in `handleConfirmDeleteProvider`. Don't toast here too, or every
    // removal shows two identical "removed successfully" toasts.
    onSuccess: () => {
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

      // If provider can be deleted, proceed with deletion. Await the mutation
      // itself (which rejects on failure) so the line below only runs on
      // success and the catch handles errors; `toast.promise` drives the
      // loading/success/error toasts off the same promise.
      const deletion = deleteProviderMut.mutateAsync({
        path: { provider_id: providerToDelete.id },
      })
      toast.promise(deletion, {
        loading: 'Removing Git provider...',
        success: 'Git provider removed successfully',
        error: 'Failed to remove provider',
      })
      await deletion

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
      <div className="space-y-6 p-4 sm:p-6">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <h1 className="text-xl font-bold sm:text-2xl">Git Providers</h1>
            <p className="text-sm text-muted-foreground sm:text-base">
              Manage your Git providers for repository access and deployments
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
              onClick={() => navigate('/git-providers/add')}
              label="Add Git Provider"
            />
          </div>
        </div>

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
        ) : !gitProviders?.length ? (
          <EmptyPlaceholder
            icon={GitBranch}
            title="No git providers found"
            description="Get started by setting up a Git provider like GitHub or GitLab"
          >
            <Button onClick={() => navigate('/git-providers/add')}>
              <Plus className="mr-2 h-4 w-4" />
              Add Git Provider
            </Button>
          </EmptyPlaceholder>
        ) : (
          (() => {
            const goToDetail = (id: number) =>
              navigate(`/git-providers/${id}`)

            const ProviderIcon = ({
              provider,
              className = 'size-4 text-muted-foreground',
            }: {
              provider: ProviderResponse
              className?: string
            }) =>
              provider.provider_type === 'github' ? (
                <GithubIcon className={className} />
              ) : provider.provider_type === 'gitea' ? (
                <GitFork className={className} />
              ) : provider.provider_type === 'generic' ? (
                <Globe className={className} />
              ) : (
                <GitBranch className={className} />
              )

            const ActionsMenu = ({ provider }: { provider: ProviderResponse }) => (
              <div
                className="flex shrink-0 items-center gap-1 sm:gap-2"
                onClick={(e) => e.stopPropagation()}
                onPointerDown={(e) => e.stopPropagation()}
              >
                {isGitHubApp(provider) && (
                  <Button
                    variant={provider.is_active ? 'outline' : 'default'}
                    size="sm"
                    onClick={() => handleInstallGitHubApp(provider)}
                    className="gap-2 hidden lg:inline-flex"
                  >
                    <GithubIcon className="h-4 w-4" />
                    Install GitHub App
                  </Button>
                )}
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button variant="ghost" size="icon" className="h-8 w-8">
                      <EllipsisVertical className="h-4 w-4" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    {isGitHubApp(provider) && (
                      <>
                        <DropdownMenuItem
                          onSelect={(e) => {
                            e.preventDefault()
                            handleInstallGitHubApp(provider)
                          }}
                        >
                          <GithubIcon className="h-4 w-4 mr-2" />
                          Install GitHub App
                        </DropdownMenuItem>
                        <DropdownMenuSeparator />
                      </>
                    )}
                    {/* Just set state here — the confirm dialog is rendered
                        once, outside the dropdown (see below). Nesting an
                        AlertDialog inside the menu unmounts it when the menu
                        closes, so the dialog never opened and the click felt
                        dead. */}
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
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            )

            return (
              <div className="overflow-hidden rounded-lg border">
                <ul role="list" className="divide-y">
                  {gitProviders.map((provider: ProviderResponse) => (
                    <li
                      key={provider.id}
                      role="button"
                      tabIndex={0}
                      onClick={() => goToDetail(provider.id)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter' || e.key === ' ') {
                          e.preventDefault()
                          goToDetail(provider.id)
                        }
                      }}
                      className="flex cursor-pointer items-center gap-3 px-3 py-3 sm:gap-4 sm:px-4 hover:bg-muted/40 transition-colors focus:outline-none focus:bg-muted/40"
                    >
                      <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted">
                        <ProviderIcon provider={provider} />
                      </div>
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2 flex-wrap">
                          <p className="truncate text-sm font-medium">
                            {provider.name}
                          </p>
                          <Badge variant="secondary" className="font-mono text-xs">
                            {provider.provider_type}
                          </Badge>
                          {!provider.is_active && (
                            <Badge variant="destructive" className="text-xs">
                              Inactive
                            </Badge>
                          )}
                          {provider.is_default && (
                            <Badge variant="outline" className="text-xs">
                              Default
                            </Badge>
                          )}
                        </div>
                        <p className="mt-0.5 truncate text-xs text-muted-foreground">
                          {provider.auth_method}
                          {provider.base_url ? ` · ${provider.base_url}` : ''}
                          {' · created '}
                          <TimeAgo date={provider.created_at} />
                        </p>
                      </div>
                      <ActionsMenu provider={provider} />
                      <ChevronRight className="hidden size-4 shrink-0 text-muted-foreground/50 sm:block" />
                    </li>
                  ))}
                </ul>
              </div>
            )
          })()
        )}
      </div>

      {/* Single controlled confirm dialog, rendered once at the page level so
          it survives the dropdown closing. Driven by `providerToDelete`. */}
      <AlertDialog
        open={!!providerToDelete}
        onOpenChange={(open) => {
          if (!open) setProviderToDelete(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Remove Git Provider</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to remove &quot;{providerToDelete?.name}
              &quot;? This action cannot be undone and will remove all
              associated connections and repositories.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel onClick={() => setProviderToDelete(null)}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleteProviderMut.isPending}
              // Don't let Radix auto-close the dialog on click — we close it
              // ourselves once the async delete (and its safety check) resolves.
              onClick={(e) => {
                e.preventDefault()
                handleConfirmDeleteProvider()
              }}
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
    </div>
  )
}
