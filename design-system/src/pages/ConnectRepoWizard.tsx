// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { CheckCircle2, GitBranch } from 'lucide-react'
import { Button, Wizard } from '@temps-sdk/ds'
import { Input } from '@temps-sdk/ui'

type StepId = 'provider' | 'repository' | 'done'

const STEPS = [
  { id: 'provider' satisfies StepId, label: 'Provider' },
  { id: 'repository' satisfies StepId, label: 'Repository' },
  { id: 'done' satisfies StepId, label: 'Connected' },
]

/**
 * Reference screen for the `Wizard` template — a simple 3-step "Connect a
 * repository" flow. Mirrors the shape of the 4 real `SetupWizardShell`
 * consumers (`ErrorTrackingSetup.tsx`, `ProjectAnalytics.tsx`,
 * `AiFirstWorkspace.tsx`, `TracesList.tsx`): pick something, configure it,
 * confirm — with the confetti celebration on the final step.
 */
export default function ConnectRepoWizard() {
  const [step, setStep] = useState<StepId>('provider')
  const [provider, setProvider] = useState<string>()
  const [repo, setRepo] = useState('')

  return (
    <div className="mx-auto max-w-lg p-8">
      <Wizard
        title="Connect a repository"
        description="Link a Git repository to deploy from a push."
        currentStep={step}
        steps={STEPS}
        celebrate={step === 'done'}
      >
        {step === 'provider' && (
          <div className="space-y-4">
            <div className="grid grid-cols-3 gap-2">
              {['GitHub', 'GitLab', 'Bitbucket'].map((name) => (
                <button
                  key={name}
                  type="button"
                  onClick={() => setProvider(name)}
                  className={`rounded-md border p-3 text-sm font-medium transition-colors ${
                    provider === name ? 'border-primary bg-primary/5' : 'hover:bg-accent'
                  }`}
                >
                  {name}
                </button>
              ))}
            </div>
            <Button disabled={!provider} onClick={() => setStep('repository')} className="w-full">
              Continue
            </Button>
          </div>
        )}

        {step === 'repository' && (
          <div className="space-y-4">
            <Input
              placeholder="org/repository-name"
              value={repo}
              onChange={(e) => setRepo(e.target.value)}
            />
            <Button disabled={!repo} onClick={() => setStep('done')} className="w-full">
              <GitBranch /> Connect repository
            </Button>
          </div>
        )}

        {step === 'done' && (
          <div className="flex flex-col items-center gap-3 py-6 text-center">
            <CheckCircle2 className="size-10 text-success" />
            <p className="font-medium">{repo || 'Repository'} is connected</p>
            <p className="text-sm text-muted-foreground">
              Pushes to the default branch will trigger a deployment.
            </p>
          </div>
        )}
      </Wizard>
    </div>
  )
}
