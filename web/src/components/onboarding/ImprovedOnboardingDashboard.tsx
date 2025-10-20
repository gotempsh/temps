import { useState, useEffect } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { CheckCircle2 } from 'lucide-react'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import {
  listGitProvidersOptions,
  listConnectionsOptions,
  getProjectsOptions,
  listDomainsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useNavigate } from 'react-router-dom'
import { DomainResponse } from '@/api/client/types.gen'
import { NetworkMode } from './NetworkModeSelector'

// Import all new onboarding step components
import { InstanceExposureStep } from './InstanceExposureStep'
import { BaseDomainStep } from './BaseDomainStep'
import { NetworkModeSelector } from './NetworkModeSelector'
import { NetworkSetupInstructions } from './NetworkSetupInstructions'
import { DomainChallengeStep } from './DomainChallengeStep'
import { ExternalUrlStep } from './ExternalUrlStep'
import { ScreenshotSetupStep } from './ScreenshotSetupStep'

// Import existing components for git/project setup
import { GitProviderFlow } from '@/components/git-providers/GitProviderFlow'
import { Button } from '@/components/ui/button'
import { useSettings } from '@/hooks/useSettings'
import { ProjectOnboardingStep } from './ProjectOnboardingStep'

type OnboardingStep =
  | 'exposure'
  | 'base-domain'
  | 'network-mode'
  | 'network-setup'
  | 'domain-challenge'
  | 'external-url'
  | 'screenshot-setup'
  | 'git-provider'
  | 'project'
  | 'complete'

interface OnboardingState {
  currentStep: OnboardingStep
  wantsExpose: boolean | null
  baseDomain: string
  networkMode: NetworkMode | null
  createdDomain: DomainResponse | null
  completedSteps: OnboardingStep[]
}

const STORAGE_KEY = 'temps_onboarding_state'

// Step labels for progress indicator
const STEP_LABELS = {
  exposure: 'Instance Exposure',
  'base-domain': 'Base Domain',
  'network-mode': 'Network Mode',
  'network-setup': 'Network Setup',
  'domain-challenge': 'Domain Challenge',
  'external-url': 'External URL',
  'screenshot-setup': 'Screenshots',
  'git-provider': 'Git Provider',
  project: 'Project',
  complete: 'Complete',
}

export function ImprovedOnboardingDashboard() {
  const navigate = useNavigate()
  const { data: settings } = useSettings()

  // Load saved state from localStorage
  const loadSavedState = (): Partial<OnboardingState> => {
    try {
      const saved = localStorage.getItem(STORAGE_KEY)
      return saved ? JSON.parse(saved) : {}
    } catch {
      return {}
    }
  }

  const savedState = loadSavedState()

  // State management
  const [currentStep, setCurrentStep] = useState<OnboardingStep>(
    savedState.currentStep || 'exposure'
  )
  const [wantsExpose, setWantsExpose] = useState<boolean | null>(
    savedState.wantsExpose ?? null
  )
  const [baseDomain, setBaseDomain] = useState<string>(
    savedState.baseDomain || ''
  )
  const [networkMode, setNetworkMode] = useState<NetworkMode | null>(
    savedState.networkMode || null
  )
  const [createdDomain, setCreatedDomain] = useState<DomainResponse | null>(
    savedState.createdDomain || null
  )
  const [completedSteps, setCompletedSteps] = useState<OnboardingStep[]>(
    savedState.completedSteps || []
  )
  const [hasAutoSkipped, setHasAutoSkipped] = useState(false)

  // Check completion status
  const { data: gitProviders } = useQuery(listGitProvidersOptions({}))
  const { data: connections } = useQuery(listConnectionsOptions({}))
  const { data: projectsData } = useQuery(getProjectsOptions({}))
  const { data: domains } = useQuery(listDomainsOptions({}))

  const hasConnections = (connections?.connections?.length || 0) > 0
  const hasProjects = (projectsData?.projects?.length || 0) > 0
  const hasExternalUrl = !!settings?.external_url
  const hasPreviewDomain = !!settings?.preview_domain
  const hasDomain = (domains?.domains?.length || 0) > 0
  // Check if there's an active/provisioned domain (not just pending)
  const hasActiveDomain =
    domains?.domains?.some((domain) => domain.status === 'active') || false
  const hasPendingDomain =
    domains?.domains?.some(
      (domain) => domain.status !== 'active' && domain.status !== 'failed'
    ) || false

  // Auto-reset onboarding if user refreshes with no resources
  useEffect(() => {
    // Only check after initial data load
    if (hasAutoSkipped || !domains || !gitProviders || !projectsData) return

    // If user has saved onboarding state but no resources exist, reset
    const hasSavedState = savedState.currentStep || completedSteps.length > 0
    const hasAnyResources = hasDomain || hasConnections || hasProjects

    if (hasSavedState && !hasAnyResources) {
      // User refreshed with no resources - reset onboarding
      queueMicrotask(() => {
        localStorage.removeItem(STORAGE_KEY)
        setCompletedSteps([])
        setCurrentStep('exposure')
        setWantsExpose(null)
        setBaseDomain('')
        setNetworkMode(null)
        setCreatedDomain(null)
        setHasAutoSkipped(true)
      })
    }
  }, [
    hasAutoSkipped,
    domains,
    gitProviders,
    projectsData,
    savedState.currentStep,
    completedSteps.length,
    hasDomain,
    hasConnections,
    hasProjects,
  ])

  // Smart initialization: infer progress from system state on first load
  useEffect(() => {
    // Only run if we haven't set up state yet and data is loaded
    if (hasAutoSkipped || !domains) return

    const noSavedState = !savedState.currentStep && completedSteps.length === 0

    if (noSavedState) {
      queueMicrotask(() => {
        const stepsToMark: OnboardingStep[] = []
        let nextStep: OnboardingStep = 'exposure'

        // If domain exists (any status), user completed steps 1-5
        if (hasDomain) {
          stepsToMark.push(
            'exposure',
            'base-domain',
            'network-mode',
            'network-setup'
          )

          // Get the first domain to populate state
          const firstDomain = domains.domains?.[0]
          if (firstDomain) {
            setCreatedDomain(firstDomain)
            // Extract base domain from wildcard domain (*.example.com -> example.com)
            const domainName = firstDomain.domain.replace(/^\*\./, '')
            setBaseDomain(domainName)
          }

          // If domain is pending, user is on domain-challenge step
          if (hasPendingDomain) {
            nextStep = 'domain-challenge'
          }
          // If domain is active, user completed domain-challenge
          else if (hasActiveDomain) {
            stepsToMark.push('domain-challenge')

            // Check where to go next based on what's configured
            if (hasExternalUrl) {
              stepsToMark.push('external-url')
            }
            // Note: screenshot-setup is optional, so we skip marking it

            // If git connections exist, go to project step
            if (hasConnections) {
              stepsToMark.push('git-provider')
              nextStep = 'project'
            } else {
              nextStep = 'git-provider'
            }
          }
        }

        setCompletedSteps(stepsToMark)
        setCurrentStep(nextStep)
        setWantsExpose(hasDomain ? true : null)
        setHasAutoSkipped(true)
      })
    }
  }, [
    domains,
    hasAutoSkipped,
    hasDomain,
    hasPendingDomain,
    hasActiveDomain,
    hasConnections,
    hasExternalUrl,
    savedState.currentStep,
    completedSteps.length,
  ])

  // Check for pending domain and redirect back to domain-challenge step
  useEffect(() => {
    const pendingDomain = domains?.domains?.find(
      (domain) => domain.status !== 'active' && domain.status !== 'failed'
    )

    if (pendingDomain && currentStep === 'git-provider' && !hasActiveDomain) {
      // User has a pending domain but skipped to git-provider, send them back
      queueMicrotask(() => {
        setCurrentStep('domain-challenge')
        setCreatedDomain(pendingDomain)
      })
    }
  }, [domains, currentStep, hasActiveDomain])

  // Auto-advance from git-provider to project if connections already exist
  // Only auto-advance if we've already marked earlier steps as complete
  useEffect(() => {
    if (
      currentStep === 'git-provider' &&
      hasConnections &&
      completedSteps.includes('domain-challenge')
    ) {
      queueMicrotask(() => {
        setCompletedSteps((prev) => [
          ...Array.from(new Set<OnboardingStep>([...prev, 'git-provider'])),
        ])
        setCurrentStep('project')
      })
    }
  }, [currentStep, hasConnections, completedSteps])

  // Save state to localStorage whenever it changes
  useEffect(() => {
    const state: OnboardingState = {
      currentStep,
      wantsExpose,
      baseDomain,
      networkMode,
      createdDomain,
      completedSteps,
    }
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state))
  }, [
    currentStep,
    wantsExpose,
    baseDomain,
    networkMode,
    createdDomain,
    completedSteps,
  ])

  // Helper to mark step as completed and move to next
  const completeStep = (step: OnboardingStep, nextStep: OnboardingStep) => {
    setCompletedSteps((prev) => [...new Set([...prev, step])])
    setCurrentStep(nextStep)
  }

  // Reset onboarding
  const resetOnboarding = () => {
    localStorage.removeItem(STORAGE_KEY)
    window.location.reload()
  }

  // Check if all steps are complete
  const allStepsComplete =
    hasActiveDomain &&
    hasConnections &&
    hasProjects &&
    hasExternalUrl &&
    hasPreviewDomain

  if (allStepsComplete) {
    return (
      <div className="max-w-5xl mx-auto space-y-6 p-6">
        <Card className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
          <CardContent className="pt-6">
            <div className="text-center space-y-4">
              <CheckCircle2 className="h-16 w-16 text-green-600 mx-auto" />
              <h2 className="text-2xl font-bold">Setup Complete!</h2>
              <p className="text-muted-foreground">
                You&apos;re all set to start deploying projects with Temps
              </p>
              <div className="flex gap-3 justify-center pt-4">
                <Button onClick={() => navigate('/projects')}>
                  Go to Projects
                </Button>
                <Button variant="outline" onClick={resetOnboarding}>
                  Reset Onboarding
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  // Calculate progress
  const allSteps = Object.keys(STEP_LABELS) as OnboardingStep[]
  // Exclude 'complete' step from progress calculation to match step counter
  const actualSteps = allSteps.filter((step) => step !== 'complete')
  const currentStepIndex = actualSteps.indexOf(
    currentStep as Exclude<OnboardingStep, 'complete'>
  )
  // Progress is based on current step position (1-indexed) out of total actual steps
  // If currentStep is 'complete', show 100% progress
  const progress =
    currentStep === 'complete'
      ? 100
      : currentStepIndex >= 0
        ? ((currentStepIndex + 1) / actualSteps.length) * 100
        : 0

  return (
    <div className="max-w-6xl mx-auto space-y-6 p-6">
      {/* Progress Bar */}
      <div className="space-y-2">
        <div className="flex items-center justify-between text-sm">
          <span className="font-medium">
            Step {currentStepIndex + 1} of {allSteps.length - 1}
          </span>
          <span className="text-muted-foreground">
            {Math.round(progress)}% Complete
          </span>
        </div>
        <div className="h-2 bg-muted rounded-full overflow-hidden">
          <div
            className="h-full bg-primary transition-all duration-500"
            style={{ width: `${progress}%` }}
          />
        </div>
      </div>

      {/* Step Indicators */}
      <div className="flex items-center justify-between relative">
        {allSteps.slice(0, -1).map((step, index) => {
          const isCompleted = completedSteps.includes(step)
          const isCurrent = step === currentStep
          return (
            <div
              key={step}
              className="flex flex-col items-center relative z-10"
            >
              <div
                className={cn(
                  'flex h-8 w-8 items-center justify-center rounded-full border-2 transition-all text-xs font-medium',
                  isCompleted
                    ? 'bg-primary border-primary text-primary-foreground'
                    : isCurrent
                      ? 'border-primary bg-background text-primary'
                      : 'border-muted bg-background text-muted-foreground'
                )}
              >
                {isCompleted ? <CheckCircle2 className="h-4 w-4" /> : index + 1}
              </div>
              <span
                className={cn(
                  'text-xs mt-1 text-center max-w-[80px]',
                  isCompleted && 'line-through text-muted-foreground',
                  isCurrent && 'font-medium'
                )}
              >
                {STEP_LABELS[step]?.split(' ')[0]}
              </span>
            </div>
          )
        })}
        <div className="absolute top-4 left-0 right-0 h-0.5 bg-muted -z-10" />
      </div>

      {/* Current Step Content */}
      <Card>
        <CardContent className="pt-6">
          {currentStep === 'exposure' && (
            <InstanceExposureStep
              selectedValue={wantsExpose}
              onSelect={(value) => {
                setWantsExpose(value)
                if (value) {
                  completeStep('exposure', 'base-domain')
                } else {
                  // Skip to git provider if keeping local
                  completeStep('exposure', 'git-provider')
                }
              }}
            />
          )}

          {currentStep === 'base-domain' && (
            <BaseDomainStep
              value={baseDomain}
              onChange={setBaseDomain}
              onNext={() => completeStep('base-domain', 'network-mode')}
              onBack={() => setCurrentStep('exposure')}
            />
          )}

          {currentStep === 'network-mode' && (
            <NetworkModeSelector
              selectedMode={networkMode}
              onSelect={setNetworkMode}
              onNext={() => completeStep('network-mode', 'network-setup')}
              onBack={() => setCurrentStep('base-domain')}
            />
          )}

          {currentStep === 'network-setup' && networkMode && (
            <NetworkSetupInstructions
              networkMode={networkMode}
              baseDomain={baseDomain}
              onNext={() => completeStep('network-setup', 'domain-challenge')}
              onBack={() => setCurrentStep('network-mode')}
            />
          )}

          {currentStep === 'domain-challenge' && (
            <DomainChallengeStep
              baseDomain={baseDomain}
              onSuccess={(domain) => {
                setCreatedDomain(domain)
                completeStep('domain-challenge', 'external-url')
              }}
              onBack={() => setCurrentStep('network-setup')}
            />
          )}

          {currentStep === 'external-url' && (
            <ExternalUrlStep
              baseDomain={baseDomain}
              domain={createdDomain}
              onSuccess={() => completeStep('external-url', 'screenshot-setup')}
              onBack={() => setCurrentStep('domain-challenge')}
            />
          )}

          {currentStep === 'screenshot-setup' && (
            <ScreenshotSetupStep
              onNext={() => completeStep('screenshot-setup', 'git-provider')}
              onSkip={() => completeStep('screenshot-setup', 'git-provider')}
            />
          )}

          {currentStep === 'git-provider' && (
            <div className="space-y-6">
              {!hasConnections ? (
                <GitProviderFlow
                  onSuccess={() => {
                    completeStep('git-provider', 'project')
                  }}
                  onCancel={() => {}}
                  mode="onboarding"
                />
              ) : (
                <div className="text-center space-y-4">
                  <p className="text-muted-foreground">
                    Git provider configured!
                  </p>
                </div>
              )}
            </div>
          )}

          {currentStep === 'project' && (
            <ProjectOnboardingStep
              onSuccess={() => {
                completeStep('project', 'complete')
              }}
            />
          )}
        </CardContent>
      </Card>

      {/* Debug/Reset Button (only in development) */}
      {process.env.NODE_ENV === 'development' && (
        <div className="text-center">
          <Button variant="ghost" size="sm" onClick={resetOnboarding}>
            Reset Onboarding (Dev Only)
          </Button>
        </div>
      )}
    </div>
  )
}
