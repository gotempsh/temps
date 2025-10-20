import {
  createGithubPatProviderMutation,
  createGitlabOauthProviderMutation,
  createGitlabPatProviderMutation,
  listGitProvidersOptions,
  listDomainsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  ArrowRight,
  Check,
  CheckCircle2,
  Copy,
  ExternalLink,
  GitBranch,
  GithubIcon,
  Info,
  Key,
  Loader2,
  Lock,
  Settings,
  Shield,
  Sparkles,
  Users,
  Zap,
} from 'lucide-react'
import { useState, useEffect, useRef } from 'react'
import { toast } from 'sonner'
import { usePlatformCapabilities } from '@/hooks/usePlatformCapabilities'

type Step =
  | 'provider'
  | 'method'
  | 'configure-pat'
  | 'configure-gitlab-pat'
  | 'configure-gitlab-method'
  | 'configure-gitlab-app'
  | 'configure-gitlab-app-credentials'
  | 'success'
type Provider = 'github' | 'gitlab'
type Method = 'app' | 'pat' | 'existing-app' | 'gitlab-app' | 'gitlab-pat'

interface GitProviderFlowProps {
  onSuccess?: () => void
  onCancel?: () => void
  className?: string
  initialStep?: Step
  mode?: 'onboarding' | 'settings' // Allow component to work in different contexts
}

export function GitProviderFlow({
  onSuccess,
  onCancel,
  className,
  initialStep = 'provider',
  mode = 'settings',
}: GitProviderFlowProps) {
  const queryClient = useQueryClient()
  const { isLocalMode: isLocal, hasPublicIP } = usePlatformCapabilities()
  // GitHub/GitLab Apps require a public URL for webhooks
  // Disable if running in local mode without a public IP
  const isLocalMode = isLocal() || !hasPublicIP()
  const [currentStep, setCurrentStep] = useState<Step>(initialStep)
  const [selectedProvider, setSelectedProvider] = useState<Provider | null>(
    null
  )
  const [selectedMethod, setSelectedMethod] = useState<Method | null>(null)
  const [patToken, setPatToken] = useState('')
  const [providerName, setProviderName] = useState('')
  const [gitlabBaseUrl, setGitlabBaseUrl] = useState('https://gitlab.com')
  const [isCreatingApp, setIsCreatingApp] = useState(false)
  const [useCustomUrl, setUseCustomUrl] = useState(false)
  const [customApiUrl, setCustomApiUrl] = useState('')
  const [copiedWebhook, setCopiedWebhook] = useState(false)
  const [copiedCallback, setCopiedCallback] = useState(false)
  const [gitlabClientId, setGitlabClientId] = useState('')
  const [gitlabClientSecret, setGitlabClientSecret] = useState('')
  const [gitlabAppName, setGitlabAppName] = useState('GitLab OAuth App')
  const [isPollingInstallations, setIsPollingInstallations] = useState(false)
  const pollingTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const previousProviderCountRef = useRef<number>(0)

  // Fetch existing git providers to check if there's already a GitHub app
  // Enable refetch interval when polling for installations
  const { data: gitProviders = [], refetch: refetchProviders } = useQuery({
    ...listGitProvidersOptions(),
    refetchInterval: isPollingInstallations ? 2000 : false, // Poll every 2s when active
  })

  // Fetch domains to check if there's a wildcard domain configured
  const { data: domainsData } = useQuery({
    ...listDomainsOptions({}),
    retry: false,
  })

  // Check if there's already a GitHub app provider
  const existingGitHubApp = gitProviders.find(
    (provider) =>
      provider.provider_type === 'github' &&
      provider.auth_method === 'github_app'
  )

  const domain = 'github.com'

  // Check if there's an active wildcard domain and generate external URL
  const wildcardDomain = domainsData?.domains?.find(
    (d) => d.domain.startsWith('*.') && d.status === 'active'
  )
  const externalUrl = wildcardDomain
    ? `https://temps.${wildcardDomain.domain.replace('*.', '')}`
    : null

  // Cleanup polling on unmount or when polling stops
  useEffect(() => {
    return () => {
      if (pollingTimeoutRef.current) {
        clearTimeout(pollingTimeoutRef.current)
      }
    }
  }, [])

  // Stop polling after 60 seconds (timeout)
  useEffect(() => {
    if (isPollingInstallations) {
      pollingTimeoutRef.current = setTimeout(() => {
        setIsPollingInstallations(false)
        toast.info('Stopped waiting for installation', {
          description: 'You can manually refresh if needed',
        })
      }, 60000) // 60 seconds timeout

      return () => {
        if (pollingTimeoutRef.current) {
          clearTimeout(pollingTimeoutRef.current)
        }
      }
    }
  }, [isPollingInstallations])

  // Detect when a new GitHub App provider is created and show installation option
  useEffect(() => {
    if (!gitProviders || gitProviders.length === 0) {
      previousProviderCountRef.current = 0
      return
    }

    const currentCount = gitProviders.length
    const previousCount = previousProviderCountRef.current

    // If we're polling and a new provider appeared
    if (isPollingInstallations && currentCount > previousCount) {
      const newGitHubApp = gitProviders.find(
        (provider) =>
          provider.provider_type === 'github' &&
          provider.auth_method === 'github_app'
      )

      if (newGitHubApp) {
        // Use queueMicrotask to defer state updates and avoid cascading renders
        queueMicrotask(() => {
          // Stop polling
          setIsPollingInstallations(false)

          // Show success message
          toast.success('GitHub App created successfully!', {
            description: 'Now install it to connect your repositories',
          })

          // Switch to method selection to show the "Use Existing GitHub App" option
          setCurrentStep('method')
          setSelectedProvider('github')
          setSelectedMethod(null) // Reset method so user can choose
        })
      }
    }

    previousProviderCountRef.current = currentCount
  }, [gitProviders, isPollingInstallations])

  // Check environment for GitHub App compatibility
  const isLocalhost =
    window.location.hostname === 'localhost' ||
    window.location.hostname === '127.0.0.1' ||
    window.location.hostname.startsWith('192.168.')

  const isHttps =
    window.location.protocol === 'https:' ||
    (useCustomUrl && customApiUrl?.startsWith('https://'))
  const canCreateGitHubApp =
    isHttps ||
    (isLocalhost && useCustomUrl && customApiUrl?.startsWith('https://'))
  const httpWarningMessage = !isHttps
    ? 'GitHub Apps require HTTPS. Please use a secure connection or provide an HTTPS URL.'
    : ''

  const createGitHubPAT = useMutation({
    ...createGithubPatProviderMutation(),
    meta: {
      errorTitle: 'Failed to add GitHub provider',
    },
    onSuccess: async () => {
      toast.success('Git provider added successfully!')
      await queryClient.invalidateQueries({ queryKey: ['listGitProviders'] })
      await queryClient.invalidateQueries({ queryKey: ['listConnections'] })
      setCurrentStep('success')
      setTimeout(() => {
        onSuccess?.()
      }, 500)
    },
  })

  const createGitLabPAT = useMutation({
    ...createGitlabPatProviderMutation(),
    meta: {
      errorTitle: 'Failed to add GitLab provider',
    },
    onSuccess: async () => {
      toast.success('GitLab provider added successfully!')
      await queryClient.invalidateQueries({ queryKey: ['listGitProviders'] })
      await queryClient.invalidateQueries({ queryKey: ['listConnections'] })
      setCurrentStep('success')
      setTimeout(() => {
        onSuccess?.()
      }, 500)
    },
  })

  const createGitLabOAuth = useMutation({
    ...createGitlabOauthProviderMutation(),
    meta: {
      errorTitle: 'Failed to add GitLab OAuth provider',
    },
    onSuccess: async () => {
      toast.success('GitLab OAuth provider added successfully!')
      await queryClient.invalidateQueries({ queryKey: ['listGitProviders'] })
      await queryClient.invalidateQueries({ queryKey: ['listConnections'] })
      setCurrentStep('success')
      setTimeout(() => {
        onSuccess?.()
      }, 500)
    },
  })

  const handleCopyWebhook = () => {
    const url =
      useCustomUrl && customApiUrl
        ? `${customApiUrl}/webhook/git/github/events`
        : `${window.location.origin}/api/webhook/git/github/events`
    navigator.clipboard.writeText(url)
    setCopiedWebhook(true)
    setTimeout(() => setCopiedWebhook(false), 2000)
  }

  const handleCopyCallback = () => {
    const url =
      useCustomUrl && customApiUrl
        ? `${customApiUrl}/webhook/git/github/callback`
        : `${window.location.origin}/api/webhook/git/github/callback`
    navigator.clipboard.writeText(url)
    setCopiedCallback(true)
    setTimeout(() => setCopiedCallback(false), 2000)
  }

  const handleCreateGitHubAppManifest = () => {
    try {
      if (!canCreateGitHubApp) {
        toast.error('GitHub Apps require HTTPS', {
          description: httpWarningMessage,
          duration: 5000,
        })
        return
      }

      if (isLocalhost && !useCustomUrl) {
        toast.error(
          'Please use Manual Setup for localhost or provide a public HTTPS URL'
        )
        return
      }

      setIsCreatingApp(true)

      const appName = `temps-${Math.random().toString(36).substring(2, 8)}`
      const source = crypto.randomUUID()

      const baseUrl =
        useCustomUrl && customApiUrl
          ? customApiUrl
          : `${window.location.origin}`
      const appUrl = `${baseUrl}`
      const apiUrl = `${baseUrl}/api`

      const manifestData = {
        name: appName,
        url: appUrl,
        hook_attributes: {
          url: `${apiUrl}/webhook/git/github/events`,
          active: true,
        },
        redirect_url: `${apiUrl}/webhook/git/github/auth`,
        callback_urls: [
          `${apiUrl}/webhook/git/github/auth`,
          `${apiUrl}/webhook/git/github/callback`,
        ],
        description: 'Temps deployment platform',
        public: true,
        request_oauth_on_install: true,
        setup_url: `${apiUrl}/webhook/git/github/install`,
        default_permissions: {
          contents: 'write',
          metadata: 'read',
          emails: 'read',
          administration: 'write',
          pull_requests: 'write',
          members: 'read',
        },
        default_events: ['push', 'pull_request'],
      }

      const form = document.createElement('form')
      form.method = 'POST'
      form.action = `https://github.com/settings/apps/new?state=${source}`
      form.target = '_blank'

      const input = document.createElement('input')
      input.type = 'hidden'
      input.name = 'manifest'
      const manifestJson = JSON.stringify(manifestData)
      input.value = manifestJson

      form.appendChild(input)
      document.body.appendChild(form)

      form.submit()
      toast.success('Opening GitHub App creation page...', {
        description: 'Complete the setup in the new tab, then return here',
        duration: 5000,
      })

      setTimeout(() => {
        if (document.body.contains(form)) {
          document.body.removeChild(form)
        }
        setIsCreatingApp(false)

        // Start polling for new installations
        setIsPollingInstallations(true)
        toast.info('Watching for new installations...', {
          description: 'Will auto-detect when you complete the setup',
        })
      }, 100)
    } catch (error) {
      console.error('Error creating GitHub App:', error)
      setIsCreatingApp(false)

      if (error instanceof TypeError) {
        toast.error('Network error', {
          description:
            'Unable to connect to GitHub. Please check your internet connection.',
        })
      } else if (error instanceof DOMException) {
        toast.error('Browser blocked the popup', {
          description: 'Please allow popups for this site and try again.',
        })
      } else {
        toast.error('Failed to create GitHub App', {
          description:
            error instanceof Error
              ? error.message
              : 'An unexpected error occurred. Please try again.',
        })
      }
    }
  }

  const handleCreateGitLabAppManifest = () => {
    // GitLab doesn't support automatic app creation like GitHub
    // We need to show the user the information and open the settings page
    const gitlabUrl = gitlabBaseUrl || 'https://gitlab.com'
    window.open(`${gitlabUrl}/-/user_settings/applications`, '_blank')

    toast.success('Opening GitLab Applications page...', {
      description: 'Please fill in the form with the information shown below',
      duration: 5000,
    })
  }

  const handleExistingAppInstall = () => {
    if (!existingGitHubApp) {
      toast.error('No existing GitHub app found')
      return
    }

    const installUrl = `${existingGitHubApp.base_url}/installations/new`
    window.open(installUrl, '_blank')

    toast.success('Opening GitHub App installation...', {
      description: 'Complete the installation in the new tab, then return here',
      duration: 5000,
    })

    // Start polling for new installations
    setIsPollingInstallations(true)
    toast.info('Watching for new installations...', {
      description: 'Will auto-detect when you complete the installation',
    })

    setCurrentStep('success')
    setTimeout(() => {
      onSuccess?.()
    }, 1500)
  }

  const handleMethodSelect = (method: Method) => {
    setSelectedMethod(method)
    if (method === 'pat') {
      setCurrentStep('configure-pat')
    } else if (method === 'existing-app') {
      handleExistingAppInstall()
    } else if (method === 'app') {
      handleCreateGitHubAppManifest()
    } else if (method === 'gitlab-pat') {
      setCurrentStep('configure-gitlab-pat')
    } else if (method === 'gitlab-app') {
      setCurrentStep('configure-gitlab-app')
    }
  }

  const handleConfigureSubmit = async () => {
    if (!patToken) {
      toast.error('Please enter a personal access token')
      return
    }

    if (selectedProvider === 'github') {
      await createGitHubPAT.mutateAsync({
        body: {
          name: providerName || `GitHub PAT - ${domain}`,
          token: patToken,
        },
      })
    } else if (selectedProvider === 'gitlab') {
      await createGitLabPAT.mutateAsync({
        body: {
          name: providerName || `GitLab PAT`,
          token: patToken,
          base_url: gitlabBaseUrl || 'https://gitlab.com',
        },
      })
    }
  }

  const handleProviderSelect = (provider: Provider) => {
    setSelectedProvider(provider)
    if (provider === 'github') {
      setProviderName('GitHub')
      setCurrentStep('method')
    } else if (provider === 'gitlab') {
      setProviderName('GitLab')
      setCurrentStep('configure-gitlab-method')
    }
  }

  const handleGitLabOAuthSubmit = async () => {
    if (!gitlabClientId || !gitlabClientSecret) {
      toast.error('Please enter both Client ID and Client Secret')
      return
    }

    const baseUrl =
      useCustomUrl && customApiUrl ? customApiUrl : `${window.location.origin}`
    const redirectUri = `${baseUrl}/api/webhook/git/gitlab/auth`

    await createGitLabOAuth.mutateAsync({
      body: {
        name: gitlabAppName || 'GitLab OAuth App',
        base_url: gitlabBaseUrl || 'https://gitlab.com',
        client_id: gitlabClientId,
        client_secret: gitlabClientSecret,
        redirect_uri: redirectUri,
      },
    })
  }

  const handleBack = () => {
    if (currentStep === 'provider') {
      onCancel?.()
    } else if (currentStep === 'method') {
      setCurrentStep('provider')
      setSelectedMethod(null)
    } else if (currentStep === 'configure-pat') {
      setCurrentStep('method')
    } else if (currentStep === 'configure-gitlab-method') {
      setCurrentStep('provider')
      setSelectedMethod(null)
    } else if (currentStep === 'configure-gitlab-pat') {
      setCurrentStep('configure-gitlab-method')
    } else if (currentStep === 'configure-gitlab-app') {
      setCurrentStep('configure-gitlab-method')
    } else if (currentStep === 'configure-gitlab-app-credentials') {
      setCurrentStep('configure-gitlab-app')
    }
  }

  if (currentStep === 'success') {
    return (
      <div className={cn('space-y-6', className)}>
        <div className="text-center space-y-4 py-8">
          <div className="flex justify-center">
            <div className="h-20 w-20 rounded-full bg-green-100 dark:bg-green-900/20 flex items-center justify-center">
              <CheckCircle2 className="h-10 w-10 text-green-600 dark:text-green-400" />
            </div>
          </div>
          <div>
            <h3 className="text-xl sm:text-2xl font-semibold">
              Provider Added Successfully!
            </h3>
            <p className="text-sm sm:text-base text-muted-foreground mt-2">
              You can now select repositories from this provider
            </p>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className={cn('space-y-6', className)}>
      {/* Step 1: Select Provider */}
      {currentStep === 'provider' && (
        <>
          <div className="text-center space-y-2 px-2">
            <h2 className="text-xl sm:text-2xl font-bold">
              Choose Git Provider
            </h2>
            <p className="text-sm sm:text-base text-muted-foreground">
              Select the platform where your repositories are hosted
            </p>
          </div>

          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <Card
              className={cn(
                'cursor-pointer transition-all duration-200',
                selectedProvider === 'github' &&
                  'ring-2 ring-primary border-primary',
                selectedProvider !== 'github' &&
                  'hover:border-muted-foreground/50 hover:shadow-md'
              )}
              onClick={() => setSelectedProvider('github')}
            >
              <CardHeader className="pb-4">
                <div className="flex items-start justify-between">
                  <div className="flex items-center gap-3">
                    <div
                      className={cn(
                        'p-3 rounded-lg',
                        selectedProvider === 'github'
                          ? 'bg-primary/10'
                          : 'bg-muted'
                      )}
                    >
                      <GithubIcon className="h-6 w-6" />
                    </div>
                    <div>
                      <CardTitle className="text-lg flex items-center gap-2">
                        GitHub
                        {existingGitHubApp && (
                          <Badge
                            variant="outline"
                            className="text-xs bg-green-50 text-green-700 border-green-200"
                          >
                            App Ready
                          </Badge>
                        )}
                      </CardTitle>
                      <CardDescription className="mt-1">
                        Connect with GitHub.com
                      </CardDescription>
                    </div>
                  </div>
                  {selectedProvider === 'github' && (
                    <CheckCircle2 className="h-5 w-5 text-primary" />
                  )}
                </div>
              </CardHeader>
              <CardContent>
                <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
                  <div className="flex items-start gap-2 text-sm">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-500 flex-shrink-0 mt-0.5" />
                    <span className="text-muted-foreground text-xs">
                      Automatic deployments
                    </span>
                  </div>
                  <div className="flex items-start gap-2 text-sm">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-500 flex-shrink-0 mt-0.5" />
                    <span className="text-muted-foreground text-xs">
                      Private repositories
                    </span>
                  </div>
                  <div className="flex items-start gap-2 text-sm">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-500 flex-shrink-0 mt-0.5" />
                    <span className="text-muted-foreground text-xs">
                      Pull request previews
                    </span>
                  </div>
                  <div className="flex items-start gap-2 text-sm">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-500 flex-shrink-0 mt-0.5" />
                    <span className="text-muted-foreground text-xs">
                      GitHub Actions
                    </span>
                  </div>
                </div>
                <div className="mt-4 flex justify-end">
                  <Button
                    variant={
                      selectedProvider === 'github' ? 'default' : 'ghost'
                    }
                    className="gap-2"
                    onClick={(e) => {
                      e.stopPropagation()
                      handleProviderSelect('github')
                    }}
                  >
                    Select GitHub
                    <ArrowRight className="h-4 w-4" />
                  </Button>
                </div>
              </CardContent>
            </Card>

            <Card
              className={cn(
                'cursor-pointer transition-all duration-200',
                selectedProvider === 'gitlab' &&
                  'ring-2 ring-primary border-primary',
                selectedProvider !== 'gitlab' &&
                  'hover:border-muted-foreground/50 hover:shadow-md'
              )}
              onClick={() => setSelectedProvider('gitlab')}
            >
              <CardHeader className="pb-4">
                <div className="flex items-start justify-between">
                  <div className="flex items-center gap-3">
                    <div
                      className={cn(
                        'p-3 rounded-lg',
                        selectedProvider === 'gitlab'
                          ? 'bg-primary/10'
                          : 'bg-muted'
                      )}
                    >
                      <GitBranch className="h-6 w-6" />
                    </div>
                    <div>
                      <CardTitle className="text-lg">GitLab</CardTitle>
                      <CardDescription className="mt-1">
                        Connect with GitLab.com or self-hosted GitLab
                      </CardDescription>
                    </div>
                  </div>
                  {selectedProvider === 'gitlab' && (
                    <CheckCircle2 className="h-5 w-5 text-primary" />
                  )}
                </div>
              </CardHeader>
              <CardContent>
                <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
                  <div className="flex items-start gap-2 text-sm">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-500 flex-shrink-0 mt-0.5" />
                    <span className="text-muted-foreground text-xs">
                      Personal Access Tokens
                    </span>
                  </div>
                  <div className="flex items-start gap-2 text-sm">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-500 flex-shrink-0 mt-0.5" />
                    <span className="text-muted-foreground text-xs">
                      Private repositories
                    </span>
                  </div>
                  <div className="flex items-start gap-2 text-sm">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-500 flex-shrink-0 mt-0.5" />
                    <span className="text-muted-foreground text-xs">
                      Merge requests
                    </span>
                  </div>
                  <div className="flex items-start gap-2 text-sm">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-500 flex-shrink-0 mt-0.5" />
                    <span className="text-muted-foreground text-xs">
                      Self-hosted support
                    </span>
                  </div>
                </div>
                <div className="mt-4 flex justify-end">
                  <Button
                    variant={
                      selectedProvider === 'gitlab' ? 'default' : 'ghost'
                    }
                    className="gap-2"
                    onClick={(e) => {
                      e.stopPropagation()
                      handleProviderSelect('gitlab')
                    }}
                  >
                    Select GitLab
                    <ArrowRight className="h-4 w-4" />
                  </Button>
                </div>
              </CardContent>
            </Card>
          </div>

          {onCancel && (
            <div className="flex justify-end">
              <Button variant="outline" onClick={onCancel}>
                Cancel
              </Button>
            </div>
          )}
        </>
      )}

      {/* Step 2: Authentication Method */}
      {currentStep === 'method' && (
        <>
          <div className="text-center space-y-2 px-2">
            <h2 className="text-xl sm:text-2xl font-bold">
              Authentication Method
            </h2>
            <p className="text-sm sm:text-base text-muted-foreground">
              Choose how to authenticate with {domain}
            </p>
          </div>

          <div
            className={cn(
              'grid gap-4',
              existingGitHubApp
                ? 'grid-cols-1 md:grid-cols-2 xl:grid-cols-3'
                : 'grid-cols-1 md:grid-cols-2'
            )}
          >
            {existingGitHubApp && (
              <Card
                className={cn(
                  'cursor-pointer transition-all',
                  selectedMethod === 'existing-app' &&
                    'ring-2 ring-primary border-primary',
                  selectedMethod !== 'existing-app' &&
                    'hover:border-muted-foreground/50 hover:shadow-md'
                )}
                onClick={() => setSelectedMethod('existing-app')}
              >
                <CardHeader>
                  <div className="flex items-start justify-between">
                    <div className="flex items-start gap-3">
                      <div
                        className={cn(
                          'p-3 rounded-lg',
                          selectedMethod === 'existing-app'
                            ? 'bg-primary/10'
                            : 'bg-muted'
                        )}
                      >
                        <GithubIcon className="h-6 w-6" />
                      </div>
                      <div className="space-y-2">
                        <div>
                          <CardTitle className="text-lg flex items-center gap-2">
                            Use Existing GitHub App
                            <Badge
                              variant="outline"
                              className="text-xs bg-green-50 text-green-700 border-green-200"
                            >
                              Recommended
                            </Badge>
                          </CardTitle>
                          <CardDescription className="mt-1">
                            Install the existing &quot;{existingGitHubApp.name}&quot; app
                          </CardDescription>
                        </div>
                      </div>
                    </div>
                    {selectedMethod === 'existing-app' && (
                      <CheckCircle2 className="h-5 w-5 text-primary flex-shrink-0" />
                    )}
                  </div>
                </CardHeader>
                <CardContent className="space-y-4">
                  <div className="grid gap-3">
                    <div className="flex items-center gap-2 text-sm">
                      <Zap className="h-4 w-4 text-yellow-500" />
                      <span>Quick setup - app already configured</span>
                    </div>
                    <div className="flex items-center gap-2 text-sm">
                      <Users className="h-4 w-4 text-blue-500" />
                      <span>Organization-wide access</span>
                    </div>
                    <div className="flex items-center gap-2 text-sm">
                      <Shield className="h-4 w-4 text-green-500" />
                      <span>Automatic deployments on push</span>
                    </div>
                    <div className="flex items-center gap-2 text-sm">
                      <Lock className="h-4 w-4 text-purple-500" />
                      <span>Enhanced security</span>
                    </div>
                  </div>

                  <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/20">
                    <Sparkles className="h-4 w-4" />
                    <AlertDescription className="text-sm">
                      This GitHub App is already set up. Just install it to your
                      repositories!
                    </AlertDescription>
                  </Alert>

                  <div className="flex justify-end">
                    <Button
                      variant={
                        selectedMethod === 'existing-app' ? 'default' : 'ghost'
                      }
                      onClick={(e) => {
                        e.stopPropagation()
                        handleMethodSelect('existing-app')
                      }}
                    >
                      <GithubIcon className="mr-2 h-4 w-4" />
                      Install Existing App
                    </Button>
                  </div>
                </CardContent>
              </Card>
            )}

            {/* GitHub App option */}
            <Card
              className={cn(
                'transition-all',
                isLocalMode
                  ? 'opacity-50 cursor-not-allowed'
                  : 'cursor-pointer',
                selectedMethod === 'app' &&
                  !isLocalMode &&
                  'ring-2 ring-primary border-primary',
                selectedMethod !== 'app' &&
                  !isLocalMode &&
                  'hover:border-muted-foreground/50 hover:shadow-md'
              )}
              onClick={() => !isLocalMode && setSelectedMethod('app')}
            >
              <CardHeader>
                <div className="flex items-start justify-between">
                  <div className="flex items-start gap-3">
                    <div
                      className={cn(
                        'p-3 rounded-lg',
                        selectedMethod === 'app' ? 'bg-primary/10' : 'bg-muted'
                      )}
                    >
                      <Shield className="h-6 w-6" />
                    </div>
                    <div className="space-y-2">
                      <div>
                        <CardTitle className="text-lg flex items-center gap-2">
                          {existingGitHubApp
                            ? 'Create New GitHub App'
                            : 'Create GitHub App'}
                          {!existingGitHubApp && !isLocalMode && (
                            <Badge variant="secondary" className="text-xs">
                              Recommended
                            </Badge>
                          )}
                          {isLocalMode && (
                            <Badge variant="outline" className="text-xs">
                              Unavailable
                            </Badge>
                          )}
                        </CardTitle>
                        <CardDescription className="mt-1">
                          {isLocalMode
                            ? 'Requires public URL for webhooks - not available in local mode'
                            : existingGitHubApp
                              ? 'Create a new GitHub App with custom settings'
                              : 'Full integration with automatic deployments'}
                        </CardDescription>
                      </div>
                    </div>
                  </div>
                  {selectedMethod === 'app' && (
                    <CheckCircle2 className="h-5 w-5 text-primary flex-shrink-0" />
                  )}
                </div>
              </CardHeader>
              <CardContent className="space-y-4">
                {isLocalMode ? (
                  <Alert className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription className="space-y-2">
                      <p className="font-medium text-sm">Local mode detected</p>
                      <p className="text-sm">
                        GitHub Apps require a publicly accessible URL to receive
                        webhook events. Since you&apos;re running in local mode
                        without external access, this option is not available.
                      </p>
                      {externalUrl && (
                        <p className="text-sm">
                          <strong>Tip:</strong> You can configure GitHub Apps at{' '}
                          <a
                            href={externalUrl}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="text-primary hover:underline inline-flex items-center gap-1"
                          >
                            {externalUrl}
                            <ExternalLink className="h-3 w-3" />
                          </a>
                        </p>
                      )}
                      <p className="text-sm">
                        Please use <strong>Personal Access Token</strong>{' '}
                        instead for local development.
                      </p>
                    </AlertDescription>
                  </Alert>
                ) : (
                  <div className="grid gap-3">
                    <div className="flex items-center gap-2 text-sm">
                      <Zap className="h-4 w-4 text-yellow-500" />
                      <span>Automatic deployments on push</span>
                    </div>
                    <div className="flex items-center gap-2 text-sm">
                      <Users className="h-4 w-4 text-blue-500" />
                      <span>Organization-wide access</span>
                    </div>
                    <div className="flex items-center gap-2 text-sm">
                      <Shield className="h-4 w-4 text-green-500" />
                      <span>Fine-grained permissions</span>
                    </div>
                    <div className="flex items-center gap-2 text-sm">
                      <Lock className="h-4 w-4 text-purple-500" />
                      <span>Enhanced security</span>
                    </div>
                  </div>
                )}

                {!isLocalMode && !isLocalhost && (
                  <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/20">
                    <Sparkles className="h-4 w-4" />
                    <AlertDescription className="text-sm">
                      Click below and we&apos;ll automatically configure everything
                      for you!
                    </AlertDescription>
                  </Alert>
                )}

                <div className="flex justify-end">
                  <Button
                    variant={
                      selectedMethod === 'app' && !isLocalMode
                        ? 'default'
                        : 'ghost'
                    }
                    disabled={isLocalMode || isCreatingApp}
                    onClick={(e) => {
                      e.stopPropagation()
                      if (!isLocalMode) {
                        handleMethodSelect('app')
                      }
                    }}
                  >
                    {isLocalMode ? (
                      <>
                        <Lock className="mr-2 h-4 w-4" />
                        Not Available
                      </>
                    ) : isCreatingApp ? (
                      <>
                        <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                        Creating...
                      </>
                    ) : (
                      <>
                        <Sparkles className="mr-2 h-4 w-4" />
                        Create GitHub App
                      </>
                    )}
                  </Button>
                </div>
              </CardContent>
            </Card>

            {/* Personal Access Token option */}
            <Card
              className={cn(
                'cursor-pointer transition-all',
                selectedMethod === 'pat' &&
                  'ring-2 ring-primary border-primary',
                selectedMethod !== 'pat' &&
                  'hover:border-muted-foreground/50 hover:shadow-md'
              )}
              onClick={() => setSelectedMethod('pat')}
            >
              <CardHeader>
                <div className="flex items-start justify-between">
                  <div className="flex items-start gap-3">
                    <div
                      className={cn(
                        'p-3 rounded-lg',
                        selectedMethod === 'pat' ? 'bg-primary/10' : 'bg-muted'
                      )}
                    >
                      <Key className="h-6 w-6" />
                    </div>
                    <div>
                      <CardTitle className="text-lg flex items-center gap-2">
                        Personal Access Token
                        {isLocalMode && !existingGitHubApp && (
                          <Badge variant="secondary" className="text-xs">
                            Recommended
                          </Badge>
                        )}
                      </CardTitle>
                      <CardDescription className="mt-1">
                        Quick setup with token-based access
                      </CardDescription>
                    </div>
                  </div>
                  {selectedMethod === 'pat' && (
                    <CheckCircle2 className="h-5 w-5 text-primary flex-shrink-0" />
                  )}
                </div>
              </CardHeader>
              <CardContent className="space-y-4">
                <div className="grid gap-3">
                  <div className="flex items-center gap-2 text-sm">
                    <Zap className="h-4 w-4 text-green-500" />
                    <span>Simple, immediate setup</span>
                  </div>
                  <div className="flex items-center gap-2 text-sm">
                    <Lock className="h-4 w-4 text-blue-500" />
                    <span>Works with private repositories</span>
                  </div>
                  <div className="flex items-center gap-2 text-sm">
                    <Shield className="h-4 w-4 text-purple-500" />
                    <span>No public endpoint required</span>
                  </div>
                  <div className="flex items-center gap-2 text-sm">
                    <AlertCircle className="h-4 w-4 text-orange-500" />
                    <span>Manual deployments only</span>
                  </div>
                </div>
                <div className="flex justify-between items-center">
                  <Button
                    variant="link"
                    className="p-0 h-auto font-normal text-sm"
                    onClick={() =>
                      window.open(
                        'https://github.com/settings/tokens/new?description=Temps%20Platform&scopes=repo,read:user,read:org',
                        '_blank'
                      )
                    }
                  >
                    <ExternalLink className="mr-1 h-3 w-3" />
                    Create GitHub token
                  </Button>
                  <Button
                    variant={selectedMethod === 'pat' ? 'default' : 'ghost'}
                    onClick={(e) => {
                      e.stopPropagation()
                      handleMethodSelect('pat')
                    }}
                  >
                    Use Personal Token
                    <ArrowRight className="ml-2 h-4 w-4" />
                  </Button>
                </div>
              </CardContent>
            </Card>
          </div>

          {isLocalhost &&
            selectedMethod === 'app' &&
            !useCustomUrl &&
            !isLocalMode && (
              <Card className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
                <CardContent className="pt-6 space-y-4">
                  <div className="flex items-center gap-2 text-orange-800 dark:text-orange-200">
                    <Settings className="h-5 w-5" />
                    <h3 className="font-medium">
                      Manual GitHub App Setup Instructions
                    </h3>
                  </div>

                  <div className="space-y-4 text-sm">
                    <div className="space-y-2">
                      <p className="font-medium">
                        1. Go to GitHub App settings:
                      </p>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() =>
                          window.open(
                            'https://github.com/settings/apps/new',
                            '_blank'
                          )
                        }
                      >
                        <ExternalLink className="mr-2 h-3 w-3" />
                        Open GitHub Settings
                      </Button>
                    </div>

                    <div className="space-y-2">
                      <p className="font-medium">2. Configure these URLs:</p>
                      <div className="space-y-2 ml-4">
                        <div>
                          <p className="text-muted-foreground">Webhook URL:</p>
                          <div className="flex items-center gap-2">
                            <code className="text-xs bg-muted px-2 py-1 rounded flex-1 overflow-x-auto">
                              {window.location.origin}
                              /api/webhook/git/github/events
                            </code>
                            <Button
                              size="sm"
                              variant="ghost"
                              className="shrink-0 h-7 w-7 p-0"
                              onClick={handleCopyWebhook}
                            >
                              {copiedWebhook ? (
                                <Check className="h-3 w-3" />
                              ) : (
                                <Copy className="h-3 w-3" />
                              )}
                            </Button>
                          </div>
                        </div>
                        <div>
                          <p className="text-muted-foreground">Callback URL:</p>
                          <div className="flex items-center gap-2">
                            <code className="text-xs bg-muted px-2 py-1 rounded flex-1 overflow-x-auto">
                              {window.location.origin}
                              /api/webhook/git/github/callback
                            </code>
                            <Button
                              size="sm"
                              variant="ghost"
                              className="shrink-0 h-7 w-7 p-0"
                              onClick={handleCopyCallback}
                            >
                              {copiedCallback ? (
                                <Check className="h-3 w-3" />
                              ) : (
                                <Copy className="h-3 w-3" />
                              )}
                            </Button>
                          </div>
                        </div>
                      </div>
                    </div>

                    <div className="space-y-1">
                      <p className="font-medium">3. Set permissions:</p>
                      <ul className="ml-4 text-muted-foreground list-disc list-inside space-y-1">
                        <li>Contents: Write</li>
                        <li>Metadata: Read</li>
                        <li>Pull requests: Write</li>
                        <li>Administration: Write</li>
                      </ul>
                    </div>

                    <div className="space-y-1">
                      <p className="font-medium">4. Subscribe to events:</p>
                      <ul className="ml-4 text-muted-foreground list-disc list-inside">
                        <li>Push, Pull request</li>
                      </ul>
                    </div>
                  </div>
                </CardContent>
              </Card>
            )}

          <div className="flex items-center justify-between">
            <Button variant="outline" onClick={handleBack}>
              <ArrowLeft className="mr-2 h-4 w-4" />
              Back
            </Button>
          </div>
        </>
      )}

      {/* Step 2b: GitLab Authentication Method */}
      {currentStep === 'configure-gitlab-method' && (
        <>
          <div className="text-center space-y-2 px-2">
            <h2 className="text-xl sm:text-2xl font-bold">
              Authentication Method
            </h2>
            <p className="text-sm sm:text-base text-muted-foreground">
              Choose how to authenticate with GitLab
            </p>
          </div>

          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            {/* GitLab App option */}
            <Card
              className={cn(
                'transition-all',
                isLocalMode
                  ? 'opacity-50 cursor-not-allowed'
                  : 'cursor-pointer',
                selectedMethod === 'gitlab-app' &&
                  !isLocalMode &&
                  'ring-2 ring-primary border-primary',
                selectedMethod !== 'gitlab-app' &&
                  !isLocalMode &&
                  'hover:border-muted-foreground/50 hover:shadow-md'
              )}
              onClick={() => !isLocalMode && setSelectedMethod('gitlab-app')}
            >
              <CardHeader>
                <div className="flex items-start justify-between">
                  <div className="flex items-start gap-3">
                    <div
                      className={cn(
                        'p-3 rounded-lg',
                        selectedMethod === 'gitlab-app'
                          ? 'bg-primary/10'
                          : 'bg-muted'
                      )}
                    >
                      <Shield className="h-6 w-6" />
                    </div>
                    <div className="space-y-2">
                      <div>
                        <CardTitle className="text-lg flex items-center gap-2">
                          GitLab Application
                          {!isLocalMode && (
                            <Badge variant="secondary" className="text-xs">
                              Recommended
                            </Badge>
                          )}
                          {isLocalMode && (
                            <Badge variant="outline" className="text-xs">
                              Unavailable
                            </Badge>
                          )}
                        </CardTitle>
                        <CardDescription className="mt-1">
                          {isLocalMode
                            ? 'Requires public URL for webhooks - not available in local mode'
                            : 'OAuth-based integration with automatic deployments'}
                        </CardDescription>
                      </div>
                    </div>
                  </div>
                  {selectedMethod === 'gitlab-app' && (
                    <CheckCircle2 className="h-5 w-5 text-primary flex-shrink-0" />
                  )}
                </div>
              </CardHeader>
              <CardContent className="space-y-4">
                {isLocalMode ? (
                  <Alert className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription className="space-y-2">
                      <p className="font-medium text-sm">Local mode detected</p>
                      <p className="text-sm">
                        GitLab Applications require a publicly accessible URL to
                        receive webhook events. Since you&apos;re running in local
                        mode without external access, this option is not
                        available.
                      </p>
                      {externalUrl && (
                        <p className="text-sm">
                          <strong>Tip:</strong> You can configure GitLab
                          Applications at{' '}
                          <a
                            href={externalUrl}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="text-primary hover:underline inline-flex items-center gap-1"
                          >
                            {externalUrl}
                            <ExternalLink className="h-3 w-3" />
                          </a>
                        </p>
                      )}
                      <p className="text-sm">
                        Please use <strong>Personal Access Token</strong>{' '}
                        instead for local development.
                      </p>
                    </AlertDescription>
                  </Alert>
                ) : (
                  <div className="grid gap-3">
                    <div className="flex items-center gap-2 text-sm">
                      <Zap className="h-4 w-4 text-yellow-500" />
                      <span>Automatic deployments on push</span>
                    </div>
                    <div className="flex items-center gap-2 text-sm">
                      <Users className="h-4 w-4 text-blue-500" />
                      <span>Organization-wide access</span>
                    </div>
                    <div className="flex items-center gap-2 text-sm">
                      <Shield className="h-4 w-4 text-green-500" />
                      <span>Fine-grained permissions</span>
                    </div>
                    <div className="flex items-center gap-2 text-sm">
                      <Lock className="h-4 w-4 text-purple-500" />
                      <span>Enhanced security</span>
                    </div>
                  </div>
                )}

                {!isLocalMode && selectedMethod === 'gitlab-app' && (
                  <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
                    <Info className="h-4 w-4 text-blue-600" />
                    <AlertDescription className="text-sm">
                      <p className="font-medium mb-2">Manual setup required</p>
                      <p>
                        GitLab requires manual application creation. We&apos;ll show
                        you the exact values to enter.
                      </p>
                    </AlertDescription>
                  </Alert>
                )}

                <div className="flex justify-end">
                  <Button
                    variant={
                      selectedMethod === 'gitlab-app' && !isLocalMode
                        ? 'default'
                        : 'ghost'
                    }
                    disabled={isLocalMode}
                    onClick={(e) => {
                      e.stopPropagation()
                      if (!isLocalMode) {
                        handleMethodSelect('gitlab-app')
                      }
                    }}
                  >
                    {isLocalMode ? (
                      <>
                        <Lock className="mr-2 h-4 w-4" />
                        Not Available
                      </>
                    ) : (
                      <>
                        <Settings className="mr-2 h-4 w-4" />
                        Create GitLab App
                      </>
                    )}
                  </Button>
                </div>
              </CardContent>
            </Card>

            {/* GitLab Personal Access Token option */}
            <Card
              className={cn(
                'cursor-pointer transition-all',
                selectedMethod === 'gitlab-pat' &&
                  'ring-2 ring-primary border-primary',
                selectedMethod !== 'gitlab-pat' &&
                  'hover:border-muted-foreground/50 hover:shadow-md'
              )}
              onClick={() => setSelectedMethod('gitlab-pat')}
            >
              <CardHeader>
                <div className="flex items-start justify-between">
                  <div className="flex items-start gap-3">
                    <div
                      className={cn(
                        'p-3 rounded-lg',
                        selectedMethod === 'gitlab-pat'
                          ? 'bg-primary/10'
                          : 'bg-muted'
                      )}
                    >
                      <Key className="h-6 w-6" />
                    </div>
                    <div>
                      <CardTitle className="text-lg flex items-center gap-2">
                        Personal Access Token
                        {isLocalMode && (
                          <Badge variant="secondary" className="text-xs">
                            Recommended
                          </Badge>
                        )}
                      </CardTitle>
                      <CardDescription className="mt-1">
                        Quick setup with token-based access
                      </CardDescription>
                    </div>
                  </div>
                  {selectedMethod === 'gitlab-pat' && (
                    <CheckCircle2 className="h-5 w-5 text-primary flex-shrink-0" />
                  )}
                </div>
              </CardHeader>
              <CardContent className="space-y-4">
                <div className="grid gap-3">
                  <div className="flex items-center gap-2 text-sm">
                    <Zap className="h-4 w-4 text-green-500" />
                    <span>Simple, immediate setup</span>
                  </div>
                  <div className="flex items-center gap-2 text-sm">
                    <Lock className="h-4 w-4 text-blue-500" />
                    <span>Works with private repositories</span>
                  </div>
                  <div className="flex items-center gap-2 text-sm">
                    <Shield className="h-4 w-4 text-purple-500" />
                    <span>No public endpoint required</span>
                  </div>
                  <div className="flex items-center gap-2 text-sm">
                    <AlertCircle className="h-4 w-4 text-orange-500" />
                    <span>Manual deployments only</span>
                  </div>
                </div>
                <div className="flex justify-between items-center">
                  <Button
                    variant="link"
                    className="p-0 h-auto font-normal text-sm"
                    onClick={() =>
                      window.open(
                        `${gitlabBaseUrl || 'https://gitlab.com'}/-/profile/personal_access_tokens`,
                        '_blank'
                      )
                    }
                  >
                    <ExternalLink className="mr-1 h-3 w-3" />
                    Create GitLab token
                  </Button>
                  <Button
                    variant={
                      selectedMethod === 'gitlab-pat' ? 'default' : 'ghost'
                    }
                    onClick={(e) => {
                      e.stopPropagation()
                      handleMethodSelect('gitlab-pat')
                    }}
                  >
                    Use Personal Token
                    <ArrowRight className="ml-2 h-4 w-4" />
                  </Button>
                </div>
              </CardContent>
            </Card>
          </div>

          <div className="flex items-center justify-between">
            <Button variant="outline" onClick={handleBack}>
              <ArrowLeft className="mr-2 h-4 w-4" />
              Back
            </Button>
          </div>
        </>
      )}

      {/* Step 3: Configure GitHub PAT */}
      {currentStep === 'configure-pat' && (
        <>
          <div className="text-center space-y-2 px-2">
            <h2 className="text-xl sm:text-2xl font-bold">
              Configure Personal Access Token
            </h2>
            <p className="text-sm sm:text-base text-muted-foreground">
              Enter your GitHub personal access token to continue
            </p>
          </div>

          <Card>
            <CardContent className="pt-6 space-y-4">
              <div>
                <Label htmlFor="provider-name">
                  Connection Name (Optional)
                </Label>
                <Input
                  id="provider-name"
                  value={providerName}
                  onChange={(e) => setProviderName(e.target.value)}
                  placeholder={`GitHub PAT - ${domain}`}
                  className="mt-1"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  A friendly name to identify this connection
                </p>
              </div>

              <div>
                <Label htmlFor="pat">Personal Access Token</Label>
                <Input
                  id="pat"
                  type="password"
                  value={patToken}
                  onChange={(e) => setPatToken(e.target.value)}
                  placeholder="ghp_..."
                  className="mt-1 font-mono"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  Token needs{' '}
                  <code className="px-1 py-0.5 bg-muted rounded text-xs">
                    repo
                  </code>{' '}
                  and{' '}
                  <code className="px-1 py-0.5 bg-muted rounded text-xs">
                    admin:repo_hook
                  </code>{' '}
                  scopes
                </p>
              </div>

              <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
                <Info className="h-4 w-4 text-blue-600" />
                <AlertDescription>
                  <div className="space-y-2">
                    <p>Need to create a token?</p>
                    <a
                      href={`https://${domain}/settings/tokens/new?scopes=repo,admin:repo_hook`}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="inline-flex items-center gap-1 text-primary hover:underline font-medium"
                    >
                      Create GitHub Token
                      <ExternalLink className="h-3 w-3" />
                    </a>
                  </div>
                </AlertDescription>
              </Alert>

              <Alert>
                <Shield className="h-4 w-4" />
                <AlertDescription>
                  Your token will be encrypted and stored securely. We&apos;ll never
                  display it again after this setup.
                </AlertDescription>
              </Alert>
            </CardContent>
          </Card>

          <div className="flex items-center justify-between">
            <Button variant="outline" onClick={handleBack}>
              <ArrowLeft className="mr-2 h-4 w-4" />
              Back
            </Button>
            <Button
              onClick={handleConfigureSubmit}
              disabled={!patToken || createGitHubPAT.isPending}
            >
              {createGitHubPAT.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Connecting...
                </>
              ) : (
                <>
                  Add Provider
                  <CheckCircle2 className="ml-2 h-4 w-4" />
                </>
              )}
            </Button>
          </div>
        </>
      )}

      {/* Step 3b: Configure GitLab PAT */}
      {currentStep === 'configure-gitlab-pat' && (
        <>
          <div className="text-center space-y-2 px-2">
            <h2 className="text-xl sm:text-2xl font-bold">
              Configure GitLab Access
            </h2>
            <p className="text-sm sm:text-base text-muted-foreground">
              Enter your GitLab instance URL and personal access token
            </p>
          </div>

          <Card>
            <CardContent className="pt-6 space-y-4">
              <div>
                <Label htmlFor="gitlab-url">GitLab URL</Label>
                <Input
                  id="gitlab-url"
                  type="url"
                  value={gitlabBaseUrl}
                  onChange={(e) => setGitlabBaseUrl(e.target.value)}
                  placeholder="https://gitlab.com"
                  className="mt-1"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  Use https://gitlab.com for GitLab.com or your self-hosted
                  GitLab URL
                </p>
              </div>

              <div>
                <Label htmlFor="provider-name">
                  Connection Name (Optional)
                </Label>
                <Input
                  id="provider-name"
                  value={providerName}
                  onChange={(e) => setProviderName(e.target.value)}
                  placeholder={`GitLab PAT`}
                  className="mt-1"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  A friendly name to identify this connection
                </p>
              </div>

              <div>
                <Label htmlFor="gitlab-pat">Personal Access Token</Label>
                <Input
                  id="gitlab-pat"
                  type="password"
                  value={patToken}
                  onChange={(e) => setPatToken(e.target.value)}
                  placeholder="glpat-..."
                  className="mt-1 font-mono"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  Token needs{' '}
                  <code className="px-1 py-0.5 bg-muted rounded text-xs">
                    api
                  </code>
                  ,{' '}
                  <code className="px-1 py-0.5 bg-muted rounded text-xs">
                    read_repository
                  </code>{' '}
                  and{' '}
                  <code className="px-1 py-0.5 bg-muted rounded text-xs">
                    write_repository
                  </code>{' '}
                  scopes
                </p>
              </div>

              <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
                <Info className="h-4 w-4 text-blue-600" />
                <AlertDescription>
                  <div className="space-y-2">
                    <p>Need to create a token?</p>
                    <a
                      href={`${gitlabBaseUrl || 'https://gitlab.com'}/-/profile/personal_access_tokens`}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="inline-flex items-center gap-1 text-primary hover:underline font-medium"
                    >
                      Create GitLab Token
                      <ExternalLink className="h-3 w-3" />
                    </a>
                  </div>
                </AlertDescription>
              </Alert>

              <Alert>
                <Shield className="h-4 w-4" />
                <AlertDescription>
                  Your token will be encrypted and stored securely. We&apos;ll never
                  display it again after this setup.
                </AlertDescription>
              </Alert>
            </CardContent>
          </Card>

          <div className="flex items-center justify-between">
            <Button variant="outline" onClick={handleBack}>
              <ArrowLeft className="mr-2 h-4 w-4" />
              Back
            </Button>
            <Button
              onClick={handleConfigureSubmit}
              disabled={
                !patToken || !gitlabBaseUrl || createGitLabPAT.isPending
              }
            >
              {createGitLabPAT.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Connecting...
                </>
              ) : (
                <>
                  Add GitLab Provider
                  <CheckCircle2 className="ml-2 h-4 w-4" />
                </>
              )}
            </Button>
          </div>
        </>
      )}

      {/* Step 3c: Configure GitLab App - Show setup instructions */}
      {currentStep === 'configure-gitlab-app' && (
        <>
          <div className="text-center space-y-2 px-2">
            <h2 className="text-xl sm:text-2xl font-bold">
              Create GitLab Application
            </h2>
            <p className="text-sm sm:text-base text-muted-foreground">
              Follow the instructions below to create your GitLab application
            </p>
          </div>

          <Card>
            <CardContent className="pt-6 space-y-6">
              <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
                <Info className="h-4 w-4 text-blue-600" />
                <AlertDescription>
                  <p className="font-medium mb-2">Manual Setup Required</p>
                  <p className="text-sm">
                    Click the button below to open GitLab&apos;s application
                    settings, then copy and paste the following values into the
                    form.
                  </p>
                </AlertDescription>
              </Alert>

              <div className="space-y-4">
                <div>
                  <div className="flex items-center justify-between mb-2">
                    <Label className="text-base font-semibold">Name</Label>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-7 gap-1"
                      onClick={() => {
                        const appName = `Temps GitLab App`
                        navigator.clipboard.writeText(appName)
                        toast.success('Copied to clipboard!')
                      }}
                    >
                      <Copy className="h-3 w-3" />
                      Copy
                    </Button>
                  </div>
                  <div className="bg-muted px-3 py-2 rounded-md font-mono text-sm">
                    Temps GitLab App
                  </div>
                  <p className="text-xs text-muted-foreground mt-1">
                    You can use any name you prefer
                  </p>
                </div>

                <div>
                  <div className="flex items-center justify-between mb-2">
                    <Label className="text-base font-semibold">
                      Redirect URI
                    </Label>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-7 gap-1"
                      onClick={() => {
                        const baseUrl =
                          useCustomUrl && customApiUrl
                            ? customApiUrl
                            : `${window.location.origin}`
                        const redirectUri = `${baseUrl}/api/webhook/git/gitlab/auth`
                        navigator.clipboard.writeText(redirectUri)
                        toast.success('Copied to clipboard!')
                      }}
                    >
                      <Copy className="h-3 w-3" />
                      Copy
                    </Button>
                  </div>
                  <div className="bg-muted px-3 py-2 rounded-md font-mono text-sm break-all">
                    {useCustomUrl && customApiUrl
                      ? customApiUrl
                      : window.location.origin}
                    /api/webhook/git/gitlab/auth
                  </div>
                  <p className="text-xs text-muted-foreground mt-1">
                    Use one line per URI
                  </p>
                </div>

                <div>
                  <div className="flex items-center justify-between mb-2">
                    <Label className="text-base font-semibold">Scopes</Label>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-7 gap-1"
                      onClick={() => {
                        navigator.clipboard.writeText(
                          'api read_repository write_repository'
                        )
                        toast.success('Copied to clipboard!')
                      }}
                    >
                      <Copy className="h-3 w-3" />
                      Copy
                    </Button>
                  </div>
                  <div className="space-y-2">
                    <div className="flex items-center gap-2">
                      <Badge variant="secondary" className="font-mono">
                        api
                      </Badge>
                      <span className="text-xs text-muted-foreground">
                        Full API access
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      <Badge variant="secondary" className="font-mono">
                        read_repository
                      </Badge>
                      <span className="text-xs text-muted-foreground">
                        Read repository data
                      </span>
                    </div>
                  </div>
                  <p className="text-xs text-muted-foreground mt-2">
                    Check all two scopes in the GitLab form
                  </p>
                </div>
              </div>

              <Alert>
                <Shield className="h-4 w-4" />
                <AlertDescription>
                  After creating the application, you&apos;ll receive a Client ID and
                  Client Secret. Save these credentials as you&apos;ll need them to
                  complete the integration.
                </AlertDescription>
              </Alert>
            </CardContent>
          </Card>

          <div className="flex items-center justify-between">
            <Button variant="outline" onClick={handleBack}>
              <ArrowLeft className="mr-2 h-4 w-4" />
              Back
            </Button>
            <div className="flex gap-2">
              <Button
                variant="outline"
                onClick={handleCreateGitLabAppManifest}
                className="gap-2"
              >
                <ExternalLink className="h-4 w-4" />
                Open GitLab Settings
              </Button>
              <Button
                onClick={() =>
                  setCurrentStep('configure-gitlab-app-credentials')
                }
                className="gap-2"
              >
                I&apos;ve Created It
                <ArrowRight className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </>
      )}

      {/* Step 3d: Enter GitLab App Credentials */}
      {currentStep === 'configure-gitlab-app-credentials' && (
        <>
          <div className="text-center space-y-2 px-2">
            <h2 className="text-xl sm:text-2xl font-bold">
              Enter Application Credentials
            </h2>
            <p className="text-sm sm:text-base text-muted-foreground">
              Enter the Client ID and Client Secret from your GitLab application
            </p>
          </div>

          <Card>
            <CardContent className="pt-6 space-y-4">
              <div>
                <Label htmlFor="gitlab-app-name">
                  Application Name (Optional)
                </Label>
                <Input
                  id="gitlab-app-name"
                  value={gitlabAppName}
                  onChange={(e) => setGitlabAppName(e.target.value)}
                  placeholder="GitLab OAuth App"
                  className="mt-1"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  A friendly name to identify this connection
                </p>
              </div>

              <div>
                <Label htmlFor="gitlab-base-url">GitLab URL</Label>
                <Input
                  id="gitlab-base-url"
                  type="url"
                  value={gitlabBaseUrl}
                  onChange={(e) => setGitlabBaseUrl(e.target.value)}
                  placeholder="https://gitlab.com"
                  className="mt-1"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  Use https://gitlab.com for GitLab.com or your self-hosted
                  GitLab URL
                </p>
              </div>

              <div>
                <Label htmlFor="gitlab-client-id">
                  Application ID (Client ID)
                </Label>
                <Input
                  id="gitlab-client-id"
                  value={gitlabClientId}
                  onChange={(e) => setGitlabClientId(e.target.value)}
                  placeholder="Enter your Application ID"
                  className="mt-1 font-mono"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  Found in your GitLab application details
                </p>
              </div>

              <div>
                <Label htmlFor="gitlab-client-secret">Secret</Label>
                <Input
                  id="gitlab-client-secret"
                  type="password"
                  value={gitlabClientSecret}
                  onChange={(e) => setGitlabClientSecret(e.target.value)}
                  placeholder="Enter your Secret"
                  className="mt-1 font-mono"
                />
                <p className="text-xs text-muted-foreground mt-1">
                  Found in your GitLab application details
                </p>
              </div>

              <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
                <Info className="h-4 w-4 text-blue-600" />
                <AlertDescription>
                  <div className="space-y-2">
                    <p className="text-sm">
                      After creating the application in GitLab, copy the
                      Application ID and Secret from the application details
                      page.
                    </p>
                  </div>
                </AlertDescription>
              </Alert>

              <Alert>
                <Shield className="h-4 w-4" />
                <AlertDescription>
                  Your credentials will be encrypted and stored securely. We&apos;ll
                  never display them again after this setup.
                </AlertDescription>
              </Alert>
            </CardContent>
          </Card>

          <div className="flex items-center justify-between">
            <Button variant="outline" onClick={handleBack}>
              <ArrowLeft className="mr-2 h-4 w-4" />
              Back
            </Button>
            <Button
              onClick={handleGitLabOAuthSubmit}
              disabled={
                !gitlabClientId ||
                !gitlabClientSecret ||
                createGitLabOAuth.isPending
              }
            >
              {createGitLabOAuth.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Connecting...
                </>
              ) : (
                <>
                  Add GitLab OAuth Provider
                  <CheckCircle2 className="ml-2 h-4 w-4" />
                </>
              )}
            </Button>
          </div>
        </>
      )}
    </div>
  )
}
