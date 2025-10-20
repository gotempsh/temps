import { useState, useEffect } from 'react'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Progress } from '@/components/ui/progress'
import { Separator } from '@/components/ui/separator'
import { cn } from '@/lib/utils'
import {
  GitBranch,
  ArrowRight,
  ArrowLeft,
  CheckCircle2,
  ExternalLink,
  Loader2,
  RefreshCw,
  AlertCircle,
  Github,
  Rocket,
  Shield,
  Webhook,
  Key,
} from 'lucide-react'
import { useQuery, useMutation } from '@tanstack/react-query'
// import { getAllGithubAppsOptions, getAllGithubInstallationsOptions, syncGithubInstallationMutation } from '@/api/client/@tanstack/react-query.gen'
import { toast } from 'sonner'

interface GitHubSetupWizardProps {
  isOpen: boolean
  onClose: () => void
  onComplete: () => void
}

type WizardStep = 'intro' | 'install' | 'authorize' | 'verify' | 'complete'

export function GitHubSetupWizard({
  isOpen,
  onClose,
  onComplete,
}: GitHubSetupWizardProps) {
  const [currentStep, setCurrentStep] = useState<WizardStep>('intro')
  const [isCheckingInstallation, setIsCheckingInstallation] = useState(false)

  // Query GitHub apps configuration
  const { data: githubApps, isLoading: _appsLoading } = useQuery({
    // ...getAllGithubAppsOptions({}),
    queryKey: ['github-apps'],
    queryFn: () => [],
    enabled: isOpen,
    retry: false,
  })

  // Query GitHub installations
  const {
    data: installations,
    refetch: refetchInstallations,
    isLoading: installationsLoading,
  } = useQuery({
    // ...getAllGithubInstallationsOptions({}),
    queryKey: ['github-installations'],
    queryFn: () => [],
    enabled: isOpen && currentStep !== 'intro',
    retry: false,
    refetchInterval: currentStep === 'verify' ? 3000 : false, // Poll when verifying
  })

  // Sync installation mutation
  const _syncInstallation = useMutation({
    // ...syncGithubInstallationMutation(),
    mutationFn: () => Promise.resolve(),
    meta: {
      errorTitle: 'Failed to sync GitHub installation',
    },
    onSuccess: () => {
      toast.success('GitHub installation synced successfully')
      refetchInstallations()
    },
  })

  const githubApp = githubApps?.[0] // Assuming single app for now
  const hasInstallation = installations && installations.length > 0

  // Auto-advance when installation is detected
  useEffect(() => {
    if (hasInstallation && currentStep === 'verify') {
      setCurrentStep('complete')
    }
  }, [hasInstallation, currentStep])

  const handleInstallClick = () => {
    if (!githubApp?.app_name) {
      toast.error('GitHub app configuration not found')
      return
    }

    // Open GitHub app installation page in new tab
    const installUrl = `https://github.com/apps/${githubApp.app_name}/installations/new`
    window.open(installUrl, '_blank')

    // Move to authorize step
    setCurrentStep('authorize')
  }

  const handleAuthorizeClick = () => {
    // Trigger OAuth flow
    window.location.href = '/api/github/login'
  }

  const handleVerifyInstallation = async () => {
    setIsCheckingInstallation(true)
    try {
      await refetchInstallations()
      if (hasInstallation) {
        setCurrentStep('complete')
      } else {
        toast.info(
          'Installation not detected yet. Please complete the GitHub app installation.'
        )
      }
    } finally {
      setIsCheckingInstallation(false)
    }
  }

  const handleComplete = () => {
    onComplete()
    onClose()
    // Reload to refresh the app state
    window.location.reload()
  }

  const getStepNumber = (step: WizardStep): number => {
    const steps: WizardStep[] = [
      'intro',
      'install',
      'authorize',
      'verify',
      'complete',
    ]
    return steps.indexOf(step) + 1
  }

  const currentStepNumber = getStepNumber(currentStep)
  const progress = (currentStepNumber / 5) * 100

  return (
    <Dialog open={isOpen} onOpenChange={onClose}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Github className="h-5 w-5" />
            GitHub Integration Setup
          </DialogTitle>
          <DialogDescription>
            Connect your GitHub account to enable automatic deployments
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-6">
          {/* Progress Bar */}
          <div className="space-y-2">
            <div className="flex justify-between text-sm text-muted-foreground">
              <span>Step {currentStepNumber} of 5</span>
              <span>{Math.round(progress)}% Complete</span>
            </div>
            <Progress value={progress} className="h-2" />
          </div>

          <Separator />

          {/* Step Content */}
          <div className="min-h-[300px]">
            {currentStep === 'intro' && (
              <div className="space-y-6">
                <div className="text-center py-4">
                  <div className="inline-flex h-16 w-16 items-center justify-center rounded-full bg-primary/10 mb-4">
                    <GitBranch className="h-8 w-8 text-primary" />
                  </div>
                  <h3 className="text-lg font-semibold mb-2">
                    Welcome to GitHub Integration
                  </h3>
                  <p className="text-muted-foreground max-w-md mx-auto">
                    This wizard will guide you through connecting your GitHub
                    account and installing the Temps GitHub App.
                  </p>
                </div>

                <div className="grid gap-4">
                  <Card>
                    <CardHeader className="pb-3">
                      <CardTitle className="text-base flex items-center gap-2">
                        <Rocket className="h-4 w-4 text-primary" />
                        What You&apos;ll Get
                      </CardTitle>
                    </CardHeader>
                    <CardContent>
                      <ul className="space-y-2 text-sm text-muted-foreground">
                        <li className="flex items-start gap-2">
                          <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5" />
                          <span>Automatic deployments on every push</span>
                        </li>
                        <li className="flex items-start gap-2">
                          <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5" />
                          <span>Access to private repositories</span>
                        </li>
                        <li className="flex items-start gap-2">
                          <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5" />
                          <span>Branch-based preview deployments</span>
                        </li>
                        <li className="flex items-start gap-2">
                          <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5" />
                          <span>Real-time deployment status updates</span>
                        </li>
                      </ul>
                    </CardContent>
                  </Card>
                </div>
              </div>
            )}

            {currentStep === 'install' && (
              <div className="space-y-6">
                <div className="text-center py-4">
                  <div className="inline-flex h-16 w-16 items-center justify-center rounded-full bg-primary/10 mb-4">
                    <Shield className="h-8 w-8 text-primary" />
                  </div>
                  <h3 className="text-lg font-semibold mb-2">
                    Install GitHub App
                  </h3>
                  <p className="text-muted-foreground max-w-md mx-auto">
                    First, install the Temps GitHub App to your account or
                    organization.
                  </p>
                </div>

                <Alert>
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    You&apos;ll be redirected to GitHub to install the app. You
                    can choose which repositories to grant access to.
                  </AlertDescription>
                </Alert>

                <Card>
                  <CardHeader>
                    <CardTitle className="text-base">
                      Installation Steps:
                    </CardTitle>
                  </CardHeader>
                  <CardContent>
                    <ol className="space-y-3 text-sm">
                      <li className="flex gap-3">
                        <span className="font-semibold text-primary">1.</span>
                        <span>Click &quot;Install GitHub App&quot; below</span>
                      </li>
                      <li className="flex gap-3">
                        <span className="font-semibold text-primary">2.</span>
                        <span>Choose your account or organization</span>
                      </li>
                      <li className="flex gap-3">
                        <span className="font-semibold text-primary">3.</span>
                        <span>Select repositories (all or specific ones)</span>
                      </li>
                      <li className="flex gap-3">
                        <span className="font-semibold text-primary">4.</span>
                        <span>Click &quot;Install&quot; on GitHub</span>
                      </li>
                      <li className="flex gap-3">
                        <span className="font-semibold text-primary">5.</span>
                        <span>Return to this wizard and continue</span>
                      </li>
                    </ol>
                  </CardContent>
                </Card>
              </div>
            )}

            {currentStep === 'authorize' && (
              <div className="space-y-6">
                <div className="text-center py-4">
                  <div className="inline-flex h-16 w-16 items-center justify-center rounded-full bg-primary/10 mb-4">
                    <Key className="h-8 w-8 text-primary" />
                  </div>
                  <h3 className="text-lg font-semibold mb-2">
                    Authorize Your Account
                  </h3>
                  <p className="text-muted-foreground max-w-md mx-auto">
                    Now, authorize Temps to access your GitHub account for
                    authentication.
                  </p>
                </div>

                <Alert>
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    This step allows you to log in with GitHub and grants Temps
                    permission to read your profile and repositories.
                  </AlertDescription>
                </Alert>

                <Card>
                  <CardHeader>
                    <CardTitle className="text-base">
                      Why is this needed?
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-3 text-sm text-muted-foreground">
                    <p>OAuth authorization allows Temps to:</p>
                    <ul className="space-y-2 ml-4">
                      <li className="flex items-start gap-2">
                        <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5" />
                        <span>Authenticate you securely with GitHub</span>
                      </li>
                      <li className="flex items-start gap-2">
                        <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5" />
                        <span>Access your repository list</span>
                      </li>
                      <li className="flex items-start gap-2">
                        <CheckCircle2 className="h-4 w-4 text-green-500 mt-0.5" />
                        <span>Create webhooks for automatic deployments</span>
                      </li>
                    </ul>
                  </CardContent>
                </Card>
              </div>
            )}

            {currentStep === 'verify' && (
              <div className="space-y-6">
                <div className="text-center py-4">
                  <div className="inline-flex h-16 w-16 items-center justify-center rounded-full bg-primary/10 mb-4">
                    <RefreshCw
                      className={cn(
                        'h-8 w-8 text-primary',
                        isCheckingInstallation && 'animate-spin'
                      )}
                    />
                  </div>
                  <h3 className="text-lg font-semibold mb-2">
                    Verifying Installation
                  </h3>
                  <p className="text-muted-foreground max-w-md mx-auto">
                    Checking your GitHub app installation and permissions...
                  </p>
                </div>

                {installationsLoading ? (
                  <div className="text-center py-8">
                    <Loader2 className="h-8 w-8 animate-spin mx-auto mb-4 text-primary" />
                    <p className="text-sm text-muted-foreground">
                      Checking GitHub installation...
                    </p>
                  </div>
                ) : hasInstallation ? (
                  <Alert className="border-green-200 bg-green-50 dark:bg-green-950/20">
                    <CheckCircle2 className="h-4 w-4 text-green-600" />
                    <AlertDescription className="text-green-900 dark:text-green-100">
                      GitHub app successfully installed and connected!
                    </AlertDescription>
                  </Alert>
                ) : (
                  <Alert>
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription>
                      Installation not detected yet. Please complete the GitHub
                      app installation and authorization steps.
                    </AlertDescription>
                  </Alert>
                )}

                <Card>
                  <CardHeader>
                    <CardTitle className="text-base">
                      Installation Status
                    </CardTitle>
                  </CardHeader>
                  <CardContent>
                    <div className="space-y-3">
                      <div className="flex items-center justify-between">
                        <span className="text-sm">GitHub App Installed</span>
                        {hasInstallation ? (
                          <CheckCircle2 className="h-5 w-5 text-green-500" />
                        ) : (
                          <div className="h-5 w-5 rounded-full border-2 border-muted" />
                        )}
                      </div>
                      <div className="flex items-center justify-between">
                        <span className="text-sm">Account Authorized</span>
                        {hasInstallation ? (
                          <CheckCircle2 className="h-5 w-5 text-green-500" />
                        ) : (
                          <div className="h-5 w-5 rounded-full border-2 border-muted" />
                        )}
                      </div>
                      <div className="flex items-center justify-between">
                        <span className="text-sm">Repositories Accessible</span>
                        {hasInstallation ? (
                          <CheckCircle2 className="h-5 w-5 text-green-500" />
                        ) : (
                          <div className="h-5 w-5 rounded-full border-2 border-muted" />
                        )}
                      </div>
                    </div>
                  </CardContent>
                </Card>
              </div>
            )}

            {currentStep === 'complete' && (
              <div className="space-y-6">
                <div className="text-center py-4">
                  <div className="inline-flex h-16 w-16 items-center justify-center rounded-full bg-green-100 dark:bg-green-950/30 mb-4">
                    <CheckCircle2 className="h-8 w-8 text-green-600" />
                  </div>
                  <h3 className="text-lg font-semibold mb-2">
                    Setup Complete!
                  </h3>
                  <p className="text-muted-foreground max-w-md mx-auto">
                    Your GitHub integration is ready. You can now deploy
                    repositories with automatic updates.
                  </p>
                </div>

                <Card className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
                  <CardHeader>
                    <CardTitle className="text-base text-green-900 dark:text-green-100">
                      🎉 You&apos;re All Set!
                    </CardTitle>
                  </CardHeader>
                  <CardContent>
                    <p className="text-sm text-muted-foreground mb-4">
                      Your GitHub account is now connected. Here&apos;s what you
                      can do next:
                    </p>
                    <ul className="space-y-2 text-sm">
                      <li className="flex items-start gap-2">
                        <Rocket className="h-4 w-4 text-primary mt-0.5" />
                        <span>Deploy your first project from GitHub</span>
                      </li>
                      <li className="flex items-start gap-2">
                        <Webhook className="h-4 w-4 text-primary mt-0.5" />
                        <span>Set up automatic deployments with webhooks</span>
                      </li>
                      <li className="flex items-start gap-2">
                        <GitBranch className="h-4 w-4 text-primary mt-0.5" />
                        <span>Create preview deployments for branches</span>
                      </li>
                    </ul>
                  </CardContent>
                </Card>
              </div>
            )}
          </div>

          <Separator />

          {/* Action Buttons */}
          <div className="flex justify-between">
            <Button
              variant="outline"
              onClick={() => {
                if (currentStep === 'intro') {
                  onClose()
                } else if (currentStep === 'install') {
                  setCurrentStep('intro')
                } else if (currentStep === 'authorize') {
                  setCurrentStep('install')
                } else if (currentStep === 'verify') {
                  setCurrentStep('authorize')
                }
              }}
              disabled={currentStep === 'complete'}
            >
              {currentStep === 'intro' ? (
                'Cancel'
              ) : (
                <>
                  <ArrowLeft className="mr-2 h-4 w-4" />
                  Back
                </>
              )}
            </Button>

            <div className="flex gap-2">
              {currentStep === 'intro' && (
                <Button onClick={() => setCurrentStep('install')}>
                  Get Started
                  <ArrowRight className="ml-2 h-4 w-4" />
                </Button>
              )}

              {currentStep === 'install' && (
                <Button onClick={handleInstallClick}>
                  <ExternalLink className="mr-2 h-4 w-4" />
                  Install GitHub App
                </Button>
              )}

              {currentStep === 'authorize' && (
                <>
                  <Button
                    variant="outline"
                    onClick={() => setCurrentStep('verify')}
                  >
                    I&apos;ve installed the app
                  </Button>
                  <Button onClick={handleAuthorizeClick}>
                    <Github className="mr-2 h-4 w-4" />
                    Authorize with GitHub
                  </Button>
                </>
              )}

              {currentStep === 'verify' && (
                <>
                  {!hasInstallation && (
                    <Button
                      variant="outline"
                      onClick={() => setCurrentStep('install')}
                    >
                      Back to Installation
                    </Button>
                  )}
                  <Button
                    onClick={handleVerifyInstallation}
                    disabled={isCheckingInstallation}
                  >
                    {isCheckingInstallation ? (
                      <>
                        <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                        Checking...
                      </>
                    ) : (
                      <>
                        <RefreshCw className="mr-2 h-4 w-4" />
                        Verify Installation
                      </>
                    )}
                  </Button>
                </>
              )}

              {currentStep === 'complete' && (
                <Button onClick={handleComplete}>
                  <CheckCircle2 className="mr-2 h-4 w-4" />
                  Complete Setup
                </Button>
              )}
            </div>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
