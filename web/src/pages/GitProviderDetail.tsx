import {
  deleteGitProviderMutation,
  getGitProviderOptions,
  listConnectionsOptions,
  listConnectionsQueryKey,
  syncRepositoriesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { ProviderResponse } from '@/api/client/types.gen'
import { ConnectionsCompactList } from '@/components/git/ConnectionsCompactList'
import {
  CredentialsEditorDialog,
  providerHasEditableCredentials,
} from '@/components/git-providers/CredentialsEditorDialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { FeedbackAlert } from '@/components/ui/feedback-alert'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useFeedback } from '@/hooks/useFeedback'
import { usePageTitle } from '@/hooks/usePageTitle'

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  Database,
  EllipsisVertical,
  ExternalLink,
  GitBranch,
  GithubIcon,
  Globe,
  Key,
  RefreshCw,
  Trash2,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import { isGitHubApp, isGitLabOAuth } from '@/lib/provider'

export default function GitProviderDetail() {
  const navigate = useNavigate()
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { feedback, showSuccess, showError, clearFeedback } = useFeedback()
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)
  const [showCredentialsDialog, setShowCredentialsDialog] = useState(false)
  const queryClient = useQueryClient()

  const providerId = parseInt(id || '0', 10)

  const {
    data: provider,
    isLoading,
    error,
  } = useQuery({
    ...getGitProviderOptions({ path: { provider_id: providerId } }),
    retry: false,
    enabled: !!id && !isNaN(providerId),
  })

  const {
    data: connections,
    isLoading: connectionsLoading,
    refetch: refetchConnections,
  } = useQuery({
    ...listConnectionsOptions({}),
    retry: false,
    enabled: !!provider,
    select: (data) =>
      data?.connections?.filter(
        (connection) => connection.provider_id === providerId
      ) || [],
    // Poll every 2s while any connection under this provider is syncing so
    // the running repo count + "Syncing" badge advance live. Polling stops
    // automatically once no connection reports `syncing=true`, keeping idle
    // tabs quiet.
    refetchInterval: (query) => {
      const anySyncing = query.state.data?.connections?.some(
        (c) => c.provider_id === providerId && c.syncing,
      )
      return anySyncing ? 2000 : false
    },
    refetchIntervalInBackground: false,
  })

  const syncMutation = useMutation({
    ...syncRepositoriesMutation(),
    meta: {
      errorTitle: 'Failed to sync repositories',
    },
    // The server flips `syncing=true` at the very start of the request, so a
    // refetch kicked off the moment we fire the mutation will see the row
    // already in its syncing state. Without this the user saw nothing change
    // until the full sync finished (potentially minutes on 20k-repo orgs).
    onMutate: () => {
      queryClient.invalidateQueries({ queryKey: listConnectionsQueryKey({}) })
    },
    onSuccess: () => {
      // 202 = sync started, not finished. The background task updates
      // `syncing` / `synced_repository_count` on the connection row as it
      // progresses; the periodic refetch on this page surfaces that.
      showSuccess('Repository sync started')
      refetchConnections()
      queryClient.invalidateQueries({ queryKey: listConnectionsQueryKey({}) })
    },
    onError: (error: any) => {
      // The backend's drop-guard resets `syncing=false` on failure too, so
      // refresh the list to clear any stale "syncing" spinner.
      queryClient.invalidateQueries({ queryKey: listConnectionsQueryKey({}) })

      // Surface the failure. RFC 7807 Problem Details puts the human
      // message in `detail`; fall back to `title` then `message`.
      const detail =
        error?.detail || error?.title || error?.message || 'Unknown error'
      showError(`Failed to start repository sync: ${detail}`)
    },
  })

  const deleteMutation = useMutation({
    ...deleteGitProviderMutation(),
    // Failures surface via the global mutation error handler in App.tsx,
    // which renders Problem Details as a toast: errorTitle becomes the
    // toast title and the API's `detail` becomes the description (e.g.
    // "Cannot delete provider X because it has N connection(s)").
    meta: { errorTitle: 'Failed to delete provider' },
    onSuccess: () => {
      toast.success('Git provider deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['listGitProviders'] })
      queryClient.invalidateQueries({ queryKey: ['listConnections'] })
      navigate('/git-providers')
    },
    onSettled: () => setShowDeleteDialog(false),
  })

  const handleDelete = () => {
    if (!provider) return
    deleteMutation.mutate({ path: { provider_id: provider.id } })
  }

  const handleSyncRepositories = (connectionId: number) => {
    syncMutation.mutate({
      path: { connection_id: connectionId },
    })
  }

  const handleAuthorize = async () => {
    if (!provider) return

    try {
      // Call the OAuth authorize endpoint which will redirect to the OAuth provider
      const url = `/api/git-providers/${provider.id}/oauth/authorize`
      window.open(url, '_blank', 'noopener,noreferrer')
      showSuccess('Opening authorization page...')
    } catch (error: any) {
      showError(
        `Failed to start authorization: ${error?.message || 'Unknown error'}`
      )
    }
  }

  const handleInstallGitHubApp = (provider: ProviderResponse) => {
    // For GitHub App providers, construct the installation URL directly
    if (isGitHubApp(provider)) {
      // Extract GitHub App URL from provider name or use default GitHub
      const baseUrl = provider.base_url
      if (!baseUrl) {
        toast.error('Base URL is not set')
        return
      }

      // Open GitHub App installation page in new tab
      const installUrl = `${baseUrl}/installations/new`
      window.open(installUrl, '_blank', 'noopener,noreferrer')

      showSuccess('Opening GitHub App installation in new tab')
    }
  }

  useEffect(() => {
    if (provider) {
      setBreadcrumbs([
        { label: 'Git Providers', href: '/git-providers' },
        { label: provider.name },
      ])
    }
  }, [provider, setBreadcrumbs])

  // Detect GitHub App creation from query parameter
  useEffect(() => {
    const searchParams = new URLSearchParams(window.location.search)
    if (searchParams.has('github_app_created')) {
      // Show success message
      toast.success('GitHub App created successfully!', {
        description: 'You can now install it to connect your repositories.',
        duration: 5000,
      })

      // Clean up the query param from the URL
      window.history.replaceState({}, '', window.location.pathname)
    }
  }, [])

  usePageTitle(provider ? `${provider.name} - Git Provider` : 'Git Provider')

  if (isLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="space-y-6 p-4 sm:p-6">
          <div className="flex items-center gap-3">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => navigate('/git-providers')}
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <div className="h-8 w-48 bg-muted rounded animate-pulse" />
          </div>
          <div className="grid gap-6">
            <div className="h-32 bg-muted rounded animate-pulse" />
            <div className="h-24 bg-muted rounded animate-pulse" />
          </div>
        </div>
      </div>
    )
  }

  if (error || !provider) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="space-y-6 p-4 sm:p-6">
          <div className="flex items-center gap-3">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => navigate('/git-providers')}
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <h1 className="text-2xl font-bold">Git Provider Not Found</h1>
          </div>
          <Alert variant="destructive">
            <AlertTriangle className="h-4 w-4" />
            <AlertDescription>
              The git provider you&apos;re looking for doesn&apos;t exist or you
              don&apos;t have access to it.
            </AlertDescription>
          </Alert>
        </div>
      </div>
    )
  }

  const getProviderIcon = () => {
    switch (provider.provider_type) {
      case 'github':
        return <GithubIcon className="h-6 w-6" />
      default:
        return <GitBranch className="h-6 w-6" />
    }
  }

  const getProviderDisplayName = () => {
    return (
      provider.provider_type.charAt(0).toUpperCase() +
      provider.provider_type.slice(1)
    )
  }

  const getAuthMethodDisplayName = () => {
    switch (provider.auth_method) {
      case 'app':
      case 'github_app':
        return 'GitHub App'
      case 'oauth':
        return 'OAuth'
      case 'token':
        return 'Personal Access Token'
      default:
        return (
          provider.auth_method.charAt(0).toUpperCase() +
          provider.auth_method.slice(1)
        )
    }
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-4 sm:p-6">
        {/* Header */}
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="flex items-start gap-3 min-w-0">
            <Button
              variant="ghost"
              size="sm"
              className="shrink-0"
              onClick={() => navigate('/git-providers')}
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <div className="space-y-1 min-w-0">
              <div className="flex flex-wrap items-center gap-2 sm:gap-3">
                {getProviderIcon()}
                <h1 className="text-xl sm:text-2xl font-bold truncate">{provider.name}</h1>
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
                  <Badge variant="outline">Default</Badge>
                )}
              </div>
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-muted-foreground">
                <span>
                  {getProviderDisplayName()} provider using{' '}
                  {getAuthMethodDisplayName()}
                </span>
                {provider.base_url && (
                  <span className="flex items-center gap-1 text-xs">
                    <Globe className="h-3 w-3" />
                    <span className="font-mono truncate max-w-[240px]">
                      {provider.base_url}
                    </span>
                    <CopyButton
                      value={provider.base_url}
                      className="h-6 w-6 p-0"
                    />
                  </span>
                )}
                <span className="text-xs">
                  Created <TimeAgo date={provider.created_at} />
                </span>
                <span className="text-xs">
                  Updated <TimeAgo date={provider.updated_at} />
                </span>
              </div>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {isGitHubApp(provider) && (
              <Button
                onClick={() => handleInstallGitHubApp(provider)}
                className="gap-2"
              >
                <ExternalLink className="h-4 w-4" />
                Install GitHub App
              </Button>
            )}
            {isGitLabOAuth(provider) && (
              <Button onClick={handleAuthorize} className="gap-2">
                <ExternalLink className="h-4 w-4" />
                Authorize
              </Button>
            )}
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" className="h-9 w-9">
                  <EllipsisVertical className="h-4 w-4" />
                  <span className="sr-only">Provider actions</span>
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                {providerHasEditableCredentials(provider) && (
                  <DropdownMenuItem
                    onSelect={(e) => {
                      e.preventDefault()
                      setShowCredentialsDialog(true)
                    }}
                  >
                    <Key className="mr-2 h-4 w-4" />
                    Edit Credentials
                  </DropdownMenuItem>
                )}
                <DropdownMenuItem
                  className="text-destructive focus:text-destructive"
                  onSelect={(e) => {
                    e.preventDefault()
                    setShowDeleteDialog(true)
                  }}
                >
                  <Trash2 className="mr-2 h-4 w-4" />
                  Delete Provider
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>

        {/* Feedback Alert */}
        <FeedbackAlert feedback={feedback} onDismiss={clearFeedback} />


        {/* GitHub App Instructions - Only show if no connections */}
        {isGitHubApp(provider) &&
          (!connections || connections.length === 0) && (
            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <GithubIcon className="h-5 w-5" />
                  GitHub App Setup
                </CardTitle>
                <CardDescription>
                  This provider uses GitHub App authentication for enhanced
                  security and features.
                </CardDescription>
              </CardHeader>
              <CardContent className="space-y-4">
                <div className="rounded-lg border bg-muted/30 p-4">
                  <h4 className="font-medium mb-2">Installation Required</h4>
                  <p className="text-sm text-muted-foreground mb-3">
                    To use this GitHub provider, you need to install the GitHub
                    App in your GitHub account or organization.
                  </p>
                  <Button
                    onClick={() => handleInstallGitHubApp(provider)}
                    className="gap-2"
                  >
                    <ExternalLink className="h-4 w-4" />
                    Install GitHub App
                  </Button>
                </div>
              </CardContent>
            </Card>
          )}

        {/* Security Notice for PAT */}
        {provider.auth_method === 'token' && (
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Key className="h-5 w-5" />
                Personal Access Token
              </CardTitle>
              <CardDescription>
                This provider uses a Personal Access Token for authentication.
              </CardDescription>
            </CardHeader>
            <CardContent>
              <Alert>
                <AlertTriangle className="h-4 w-4" />
                <AlertDescription>
                  Personal Access Tokens are stored securely and encrypted. For
                  enhanced security and automatic deployments, consider using
                  GitHub App authentication instead.
                </AlertDescription>
              </Alert>
            </CardContent>
          </Card>
        )}

        {/* Git Connections */}
        <Card>
          <CardHeader className="flex flex-row items-center justify-between gap-2 py-3">
            <CardTitle className="flex items-center gap-2 text-sm font-semibold">
              <Database className="h-4 w-4 text-muted-foreground" />
              Connections
              {connections && connections.length > 0 && (
                <Badge variant="secondary" className="h-5 px-1.5 text-[10px]">
                  {connections.length}
                </Badge>
              )}
            </CardTitle>
          </CardHeader>
          <CardContent>
            {connectionsLoading ? (
              <div className="flex items-center justify-center py-8">
                <RefreshCw className="h-6 w-6 animate-spin" />
                <span className="ml-2">Loading connections...</span>
              </div>
            ) : !connections?.length ? (
              <div className="text-center py-8 text-muted-foreground">
                <Database className="h-12 w-12 mx-auto mb-4 opacity-50" />
                <p className="text-lg font-medium mb-2">No connections found</p>
                <p className="text-sm mb-4">
                  There are no Git connections associated with this provider
                  yet.
                </p>
                {isGitHubApp(provider) && (
                  <Button
                    onClick={() => handleInstallGitHubApp(provider)}
                    className="gap-2"
                  >
                    <ExternalLink className="h-4 w-4" />
                    Install GitHub App
                  </Button>
                )}
                {isGitLabOAuth(provider) && (
                  <Button onClick={handleAuthorize} className="gap-2">
                    <ExternalLink className="h-4 w-4" />
                    Authorize
                  </Button>
                )}
              </div>
            ) : (
              <ConnectionsCompactList
                variant="single-line"
                connections={connections}
                provider={provider}
                onSyncRepository={handleSyncRepositories}
                isSyncing={syncMutation.isPending}
                onConnectionDeleted={refetchConnections}
              />
            )}
          </CardContent>
        </Card>
      </div>

      {/* Edit Credentials Dialog */}
      <CredentialsEditorDialog
        provider={provider}
        open={showCredentialsDialog}
        onOpenChange={setShowCredentialsDialog}
      />

      {/* Delete Confirmation Dialog */}
      <Dialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete Git Provider</DialogTitle>
            <DialogDescription>
              Are you sure you want to delete &quot;{provider.name}&quot;? This
              action cannot be undone. Providers with existing connections
              cannot be deleted — remove connections first.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowDeleteDialog(false)}
              disabled={deleteMutation.isPending}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleteMutation.isPending}
            >
              {deleteMutation.isPending ? (
                <>
                  <RefreshCw className="mr-2 h-4 w-4 animate-spin" />
                  Deleting...
                </>
              ) : (
                'Delete Provider'
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
