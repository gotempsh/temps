import { useState, useEffect } from 'react'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge } from '@/components/ui/badge'
import {
  CheckCircle2,
  Github,
  Settings,
  RefreshCw,
  Sparkles,
  Zap,
  AlertCircle,
  Copy,
  Check,
  ExternalLink,
} from 'lucide-react'
import { toast } from 'sonner'

interface GitHubAppCreationFormProps {
  onAppCreated: () => void | Promise<void> | Promise<boolean>
  isActive?: boolean // To know if this step is active
}

export function GitHubAppCreationForm({
  onAppCreated,
  isActive = true,
}: GitHubAppCreationFormProps) {
  const [isCreating, setIsCreating] = useState(false)
  const [isCheckingApp, setIsCheckingApp] = useState(false)
  const [hasOpenedGitHub, setHasOpenedGitHub] = useState(false)
  const [useCustomUrl, setUseCustomUrl] = useState(false)
  const [customApiUrl, setCustomApiUrl] = useState('')
  const [copiedWebhook, setCopiedWebhook] = useState(false)
  const [copiedCallback, setCopiedCallback] = useState(false)

  // Auto-check when tab becomes visible after GitHub app creation
  useEffect(() => {
    if (!isActive || !hasOpenedGitHub) return

    const handleVisibilityChange = async () => {
      if (
        document.visibilityState === 'visible' &&
        hasOpenedGitHub &&
        !isCheckingApp
      ) {
        setIsCheckingApp(true)
        try {
          const result = await onAppCreated()
          // Check if result is explicitly false (not found) vs true/undefined (found)
          if (result === true || result === undefined) {
            toast.success('GitHub App detected!', {
              description: 'Proceeding to the next step...',
            })
            setHasOpenedGitHub(false)
          }
        } finally {
          setIsCheckingApp(false)
        }
      }
    }

    document.addEventListener('visibilitychange', handleVisibilityChange)
    return () =>
      document.removeEventListener('visibilitychange', handleVisibilityChange)
  }, [hasOpenedGitHub, isActive, onAppCreated, isCheckingApp])

  // Check if we're on localhost
  const isLocalhost =
    window.location.hostname === 'localhost' ||
    window.location.hostname === '127.0.0.1' ||
    window.location.hostname.startsWith('192.168.')

  const handleCopyWebhook = () => {
    const url =
      useCustomUrl && customApiUrl
        ? `${customApiUrl}/github/webhook`
        : `${window.location.origin}/api/github/webhook`
    navigator.clipboard.writeText(url)
    setCopiedWebhook(true)
    setTimeout(() => setCopiedWebhook(false), 2000)
  }

  const handleCopyCallback = () => {
    const url =
      useCustomUrl && customApiUrl
        ? `${customApiUrl}/github/callback`
        : `${window.location.origin}/api/github/callback`
    navigator.clipboard.writeText(url)
    setCopiedCallback(true)
    setTimeout(() => setCopiedCallback(false), 2000)
  }

  const handleCreateGitHubAppManifest = () => {
    try {
      if (isLocalhost && !useCustomUrl) {
        toast.error(
          'Please use Manual Setup for localhost or provide a public URL'
        )
        return
      }

      setIsCreating(true)

      // Generate a unique name for the app
      const appName = `temps-${Math.random().toString(36).substring(2, 8)}`
      const source = crypto.randomUUID()

      const baseUrl =
        useCustomUrl && customApiUrl
          ? customApiUrl
          : `${window.location.origin}`
      const appUrl = `${baseUrl}`
      const apiUrl = `${baseUrl}/api`
      // Create the manifest data matching the Rust implementation
      // Note: The Rust code shows webhook_base_url = format!("{}/webhook", api_base_url)
      // where api_base_url = format!("{}/api", app_state.config.external_url)

      // Start with minimal manifest - only required fields per GitHub docs
      const manifestData = {
        name: appName,
        url: appUrl,
        hook_attributes: {
          url: `${apiUrl}/webhook/git/github/events`,
          active: true,
        },
        // redirect_url is where GitHub sends the user after app creation/authorization
        // This should be the OAuth authorization callback
        redirect_url: `${apiUrl}/webhook/git/github/auth`,

        // callback_urls are the OAuth callback URLs that GitHub will accept
        // Include both auth and callback endpoints for flexibility
        callback_urls: [
          `${apiUrl}/webhook/git/github/auth`, // OAuth authorization callback
          `${apiUrl}/webhook/git/github/callback`, // Installation callback
        ],

        description: 'Temps deployment platform',
        public: true,

        // This ensures GitHub requests OAuth authorization when installing
        request_oauth_on_install: true,

        // setup_url is where users go after installation to complete setup
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

      // Log the manifest for debugging

      // Create a form and submit it to GitHub
      const form = document.createElement('form')
      form.method = 'POST'
      form.action = `https://github.com/settings/apps/new?state=${source}`
      form.target = '_blank'

      // // Add state as a separate input
      // const stateInput = document.createElement('input')
      // stateInput.type = 'hidden'
      // stateInput.name = 'state'
      // stateInput.value = source

      const input = document.createElement('input')
      input.type = 'hidden'
      input.name = 'manifest'
      const manifestJson = JSON.stringify(manifestData)
      input.value = manifestJson

      // Log what we're submitting

      form.appendChild(input)
      document.body.appendChild(form)

      // Submit the form
      form.submit()
      toast.success('Opening GitHub App creation page...', {
        description:
          'Complete the setup in the new tab, then click "I\'ve created it"',
        duration: 5000,
      })

      // Mark that we've opened GitHub
      setHasOpenedGitHub(true)

      // Give the form time to submit before removing
      setTimeout(() => {
        if (document.body.contains(form)) {
          document.body.removeChild(form)
        }
        setIsCreating(false)
      }, 100)
    } catch (error) {
      console.error('Error creating GitHub App:', error)
      setIsCreating(false)

      // Provide specific error messages based on the error type
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

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-3">
          <div className="flex h-12 w-12 items-center justify-center rounded-full bg-primary/10">
            <Settings className="h-6 w-6 text-primary" />
          </div>
          <div>
            <CardTitle>Step 1: Create GitHub App</CardTitle>
            <CardDescription>
              Set up a GitHub App to enable automatic deployments
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-6">
        {isLocalhost && (
          <Alert className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription className="space-y-2">
              <p className="font-medium">Running on localhost</p>
              <p className="text-sm">
                GitHub requires webhook URLs to be publicly accessible. You have
                these options:
              </p>
              <ul className="text-sm mt-2 space-y-1 list-disc list-inside">
                <li>Use Manual Setup and copy/paste the URLs</li>
                <li>Deploy this app first, then run the setup</li>
                <li>
                  Use a tunnel service (e.g., ngrok) and provide the public URL
                  below
                </li>
              </ul>
            </AlertDescription>
          </Alert>
        )}

        {isLocalhost && (
          <div className="space-y-3">
            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="use-custom-url"
                checked={useCustomUrl}
                onChange={(e) => setUseCustomUrl(e.target.checked)}
                className="rounded border-gray-300"
              />
              <Label
                htmlFor="use-custom-url"
                className="font-medium cursor-pointer"
              >
                I have a public URL (e.g., from ngrok or deployed instance)
              </Label>
            </div>

            {useCustomUrl && (
              <div className="ml-6 space-y-2">
                <Label htmlFor="custom-url">Public API URL</Label>
                <Input
                  id="custom-url"
                  type="url"
                  placeholder="https://your-domain.com/api or https://xyz.ngrok.io/api"
                  value={customApiUrl}
                  onChange={(e) => setCustomApiUrl(e.target.value)}
                  className="font-mono text-sm"
                />
                <p className="text-xs text-muted-foreground">
                  Enter your public API base URL (should end with /api)
                </p>
              </div>
            )}
          </div>
        )}

        {(!isLocalhost || useCustomUrl) && (
          <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/20">
            <Zap className="h-4 w-4" />
            <AlertDescription className="text-sm">
              Click the button below and we&apos;ll automatically configure
              everything for you!
            </AlertDescription>
          </Alert>
        )}

        <div className="space-y-4">
          {isLocalhost && !useCustomUrl && (
            <div className="space-y-4">
              <h3 className="font-medium mb-3">Manual Setup Instructions:</h3>
              <div className="space-y-3">
                <div className="flex items-start gap-3">
                  <Badge variant="outline" className="mt-0.5 shrink-0">
                    1
                  </Badge>
                  <div className="space-y-1 flex-1">
                    <p className="font-medium text-sm">GitHub App name</p>
                    <p className="text-xs text-muted-foreground">
                      Use any unique name (e.g., &quot;temps-abc123&quot;)
                    </p>
                  </div>
                </div>

                <div className="flex items-start gap-3">
                  <Badge variant="outline" className="mt-0.5 shrink-0">
                    2
                  </Badge>
                  <div className="space-y-1 flex-1">
                    <p className="font-medium text-sm">Webhook URL</p>
                    <div className="flex items-center gap-2">
                      <code className="text-xs bg-muted px-2 py-1 rounded flex-1 overflow-x-auto">
                        {window.location.origin}/api/github/webhook
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
                </div>

                <div className="flex items-start gap-3">
                  <Badge variant="outline" className="mt-0.5 shrink-0">
                    3
                  </Badge>
                  <div className="space-y-1 flex-1">
                    <p className="font-medium text-sm">Callback URL</p>
                    <div className="flex items-center gap-2">
                      <code className="text-xs bg-muted px-2 py-1 rounded flex-1 overflow-x-auto">
                        {window.location.origin}/api/github/callback
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

                <div className="flex items-start gap-3">
                  <Badge variant="outline" className="mt-0.5 shrink-0">
                    4
                  </Badge>
                  <div className="space-y-1 flex-1">
                    <p className="font-medium text-sm">Permissions</p>
                    <p className="text-xs text-muted-foreground">
                      Contents: Write, Metadata: Read, Pull requests: Write
                    </p>
                  </div>
                </div>
              </div>
            </div>
          )}

          {(!isLocalhost || useCustomUrl) && (
            <div className="rounded-lg border bg-muted/30 p-4">
              <h3 className="font-medium mb-3">What will be configured:</h3>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-3 text-sm">
                <div className="flex items-start gap-2">
                  <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5 shrink-0" />
                  <div>
                    <p className="font-medium">Automatic Setup</p>
                    <p className="text-xs text-muted-foreground">
                      All settings pre-configured
                    </p>
                  </div>
                </div>
                <div className="flex items-start gap-2">
                  <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5 shrink-0" />
                  <div>
                    <p className="font-medium">Webhook URLs</p>
                    <p className="text-xs text-muted-foreground">
                      Automatically set to your domain
                    </p>
                  </div>
                </div>
                <div className="flex items-start gap-2">
                  <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5 shrink-0" />
                  <div>
                    <p className="font-medium">Repository Permissions</p>
                    <p className="text-xs text-muted-foreground">
                      Read & write access for deployments
                    </p>
                  </div>
                </div>
                <div className="flex items-start gap-2">
                  <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5 shrink-0" />
                  <div>
                    <p className="font-medium">Event Subscriptions</p>
                    <p className="text-xs text-muted-foreground">
                      Push and pull request events
                    </p>
                  </div>
                </div>
              </div>
            </div>
          )}

          <div className="space-y-2">
            <h4 className="text-sm font-medium text-muted-foreground">
              How it works:
            </h4>
            <ol className="space-y-1 text-sm text-muted-foreground list-decimal list-inside">
              <li>Click &quot;Create GitHub App Automatically&quot;</li>
              <li>
                You&apos;ll be redirected to GitHub with all settings pre-filled
              </li>
              <li>Review and confirm the app creation on GitHub</li>
              <li>
                Return here and click &quot;I&apos;ve created it&quot; to
                continue
              </li>
            </ol>
          </div>
        </div>
      </CardContent>
      <CardFooter className="flex flex-col gap-3">
        {(!isLocalhost || (useCustomUrl && customApiUrl)) && (
          <Button
            onClick={handleCreateGitHubAppManifest}
            className="w-full"
            size="lg"
            disabled={isCreating || (useCustomUrl && !customApiUrl)}
          >
            <Github className="mr-2 h-5 w-5" />
            Create GitHub App Automatically
            <Sparkles className="ml-2 h-4 w-4" />
          </Button>
        )}
        <div className="flex gap-2 w-full">
          <Button
            onClick={() =>
              window.open('https://github.com/settings/apps/new', '_blank')
            }
            variant="outline"
            className="flex-1"
            size="sm"
          >
            {isLocalhost && !useCustomUrl ? (
              <>
                <ExternalLink className="mr-2 h-3 w-3" />
                Open GitHub Settings
              </>
            ) : (
              <>
                <Settings className="mr-2 h-3 w-3" />
                Manual Setup
              </>
            )}
          </Button>
          <Button
            onClick={async () => {
              setIsCheckingApp(true)
              try {
                await onAppCreated()
                toast.success('GitHub App found!', {
                  description: 'Proceeding to the next step...',
                })
              } catch (error) {
                console.error('Error checking GitHub App:', error)
                toast.error('GitHub App not found', {
                  description:
                    "Please make sure you've created the app and try again.",
                })
              } finally {
                setIsCheckingApp(false)
              }
            }}
            variant="outline"
            className="flex-1"
            size="sm"
            disabled={isCheckingApp}
          >
            {isCheckingApp ? (
              <>
                <RefreshCw className="mr-2 h-3 w-3 animate-spin" />
                Checking...
              </>
            ) : (
              <>
                <RefreshCw className="mr-2 h-3 w-3" />
                I&apos;ve created it
              </>
            )}
          </Button>
        </div>
      </CardFooter>
    </Card>
  )
}
