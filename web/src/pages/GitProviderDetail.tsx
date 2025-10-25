import {
  getGitProviderOptions,
  listConnectionsOptions,
  syncRepositoriesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { ProviderResponse } from '@/api/client/types.gen'
import { ConnectionsTable } from '@/components/git/ConnectionsTable'
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
import { FeedbackAlert } from '@/components/ui/feedback-alert'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useFeedback } from '@/hooks/useFeedback'
import { usePageTitle } from '@/hooks/usePageTitle'

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  Calendar,
  CheckCircle2,
  Database,
  ExternalLink,
  GitBranch,
  GithubIcon,
  Globe,
  Key,
  RefreshCw,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

// Helper function to check if provider is GitHub App
const isGitHubApp = (provider: ProviderResponse) =>
  provider.provider_type === 'github' &&
  (provider.auth_method === 'app' || provider.auth_method === 'github_app')

// Helper function to check if provider is GitLab OAuth
const isGitLabOAuth = (provider: ProviderResponse) =>
  provider.provider_type === 'gitlab' && provider.auth_method === 'oauth'

export default function GitProviderDetail() {
  const navigate = useNavigate()
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { feedback, showSuccess, showError, clearFeedback } = useFeedback()
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)
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
  })

  const syncMutation = useMutation({
    ...syncRepositoriesMutation(),
    meta: {
      errorTitle: 'Failed to sync repositories',
    },
    onSuccess: () => {
      showSuccess('Repositories synced successfully!')
      refetchConnections()
      queryClient.invalidateQueries({ queryKey: ['listConnections'] })
    },
  })

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
        { label: 'Git Providers', href: '/git-sources' },
        { label: provider.name },
      ])
    }
  }, [provider, setBreadcrumbs])

  usePageTitle(provider ? `${provider.name} - Git Provider` : 'Git Provider')

  if (isLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="space-y-6 p-6">
          <div className="flex items-center gap-3">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => navigate('/git-sources')}
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
        <div className="space-y-6 p-6">
          <div className="flex items-center gap-3">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => navigate('/git-sources')}
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
      <div className="space-y-6 p-6">
        {/* Header */}
        <div className="flex items-start justify-between">
          <div className="flex items-center gap-3">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => navigate('/git-sources')}
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <div className="space-y-1">
              <div className="flex items-center gap-3">
                {getProviderIcon()}
                <h1 className="text-2xl font-bold">{provider.name}</h1>
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
              <p className="text-muted-foreground">
                {getProviderDisplayName()} provider using{' '}
                {getAuthMethodDisplayName()}
              </p>
            </div>
          </div>
          <div className="flex items-center gap-2">
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
                Authorize GitLab
              </Button>
            )}
          </div>
        </div>

        {/* Feedback Alert */}
        <FeedbackAlert feedback={feedback} onDismiss={clearFeedback} />

        {/* Provider Details */}
        <div className="grid gap-6 md:grid-cols-2">
          {/* Basic Information */}
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Activity className="h-5 w-5" />
                Provider Information
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="grid gap-3">
                <div className="flex justify-between items-center">
                  <Label className="text-sm font-medium">Provider Type</Label>
                  <span className="text-sm">{getProviderDisplayName()}</span>
                </div>
                <Separator />
                <div className="flex justify-between items-center">
                  <Label className="text-sm font-medium">
                    Authentication Method
                  </Label>
                  <span className="text-sm">{getAuthMethodDisplayName()}</span>
                </div>
                <Separator />
                <div className="flex justify-between items-center">
                  <Label className="text-sm font-medium">Status</Label>
                  {provider.is_active ? (
                    <Badge variant="secondary" className="text-xs">
                      Active
                    </Badge>
                  ) : (
                    <Badge variant="destructive" className="text-xs">
                      Inactive
                    </Badge>
                  )}
                </div>
                <Separator />
                {provider.base_url && (
                  <>
                    <div className="flex justify-between items-center gap-3">
                      <Label className="text-sm font-medium">Base URL</Label>
                      <div className="flex items-center gap-2 text-sm min-w-0">
                        <Globe className="h-3 w-3 flex-shrink-0" />
                        <span className="break-all">{provider.base_url}</span>
                        <CopyButton
                          value={provider.base_url}
                          className="h-7 w-7 p-0 hover:bg-accent hover:text-accent-foreground rounded-md flex-shrink-0"
                        />
                      </div>
                    </div>
                    <Separator />
                  </>
                )}
                <div className="flex justify-between items-center">
                  <Label className="text-sm font-medium">
                    Default Provider
                  </Label>
                  {provider.is_default ? (
                    <Badge variant="outline" className="text-xs">
                      Yes
                    </Badge>
                  ) : (
                    <span className="text-sm text-muted-foreground">No</span>
                  )}
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Timestamps */}
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Calendar className="h-5 w-5" />
                Timeline
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="space-y-3">
                <div className="flex justify-between items-center">
                  <Label className="text-sm font-medium">Created</Label>
                  <TimeAgo date={provider.created_at} className="text-sm" />
                </div>
                <Separator />
                <div className="flex justify-between items-center">
                  <Label className="text-sm font-medium">Last Updated</Label>
                  <TimeAgo date={provider.updated_at} className="text-sm" />
                </div>
              </div>
            </CardContent>
          </Card>
        </div>

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

        {/* Git Connections Table */}
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div>
                <CardTitle className="flex items-center gap-2">
                  <Database className="h-5 w-5" />
                  Git Connections
                </CardTitle>
                <CardDescription>
                  All Git connections associated with this provider
                </CardDescription>
              </div>
              {connections && connections.length > 0 && (
                <>
                  {isGitHubApp(provider) && (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => handleInstallGitHubApp(provider)}
                      className="gap-2"
                    >
                      <ExternalLink className="h-4 w-4" />
                      Install GitHub App
                    </Button>
                  )}
                  {isGitLabOAuth(provider) && (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={handleAuthorize}
                      className="gap-2"
                    >
                      <ExternalLink className="h-4 w-4" />
                      Authorize GitLab
                    </Button>
                  )}
                </>
              )}
            </div>
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
                    Authorize GitLab
                  </Button>
                )}
              </div>
            ) : (
              <ConnectionsTable
                connections={connections}
                provider={provider}
                onSyncRepository={handleSyncRepositories}
                onAuthorize={handleAuthorize}
                isSyncing={syncMutation.isPending}
                onConnectionDeleted={refetchConnections}
              />
            )}
          </CardContent>
        </Card>

        {/* Danger Zone */}
        <Card className="border-destructive">
          <CardHeader>
            <CardTitle className="text-destructive">Danger Zone</CardTitle>
            <CardDescription>
              These actions cannot be undone. Please be careful.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Button
              variant="destructive"
              onClick={() => setShowDeleteDialog(true)}
              disabled
            >
              Delete Provider
            </Button>
            <p className="text-xs text-muted-foreground mt-2">
              Provider deletion is not yet available
            </p>
          </CardContent>
        </Card>
      </div>

      {/* Delete Confirmation Dialog */}
      <Dialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete Git Provider</DialogTitle>
            <DialogDescription>
              Are you sure you want to delete this git provider? This action
              cannot be undone.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowDeleteDialog(false)}
            >
              Cancel
            </Button>
            <Button variant="destructive" disabled>
              Delete Provider
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
