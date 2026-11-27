// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getHostFactsOptions,
  getPrivateIngressOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { PageContainer } from '@/components/layout/PageContainer'
import { factsFromHost } from '@/components/welcome/access'
import { AgentsStep } from '@/components/welcome/AgentsStep'
import { HostStep } from '@/components/welcome/HostStep'
import { STEPS, nextStep, type StepKey } from '@/components/welcome/onboarding'
import { ReachStep } from '@/components/welcome/ReachStep'
import { ShipStep } from '@/components/welcome/ShipStep'
import {
  useOnboarding,
  useUpdateOnboarding,
} from '@/components/welcome/useOnboarding'
import { AdminOnlyNote } from '@/components/ui/admin-only-note'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useCurrentUserRole } from '@/hooks/useCurrentUserRole'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import { Button, Callout, Wizard } from '@temps-sdk/ds'
import { useMutation, useQuery } from '@tanstack/react-query'
import { useEffect } from 'react'
import { useNavigate, useSearchParams } from 'react-router'

const isStep = (value: string | null): value is StepKey =>
  STEPS.some((s) => s.key === value)

/**
 * First run: check this machine, choose who reaches the apps, connect an
 * agent, deploy an app. One question per screen; every step can be left for later
 * with its reason kept on the server, where the Setup page reads it.
 */
export function Welcome() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams, setSearchParams] = useSearchParams()
  // Setup is the administrator's: for anyone else the onboarding query is
  // disabled, and would stay pending forever.
  const isAdmin = useIsInstanceAdmin()
  const role = useCurrentUserRole()
  const onboarding = useOnboarding()
  const update = useUpdateOnboarding()
  const host = useQuery({
    ...getHostFactsOptions(),
    staleTime: 60_000,
    enabled: isAdmin,
  })
  // This machine's private networks (ADR-115 D8); absent where the proxy
  // runs in another process, and the facts then fall back to Tailscale.
  const privateIngress = useQuery({
    ...getPrivateIngressOptions(),
    staleTime: 60_000,
    enabled: isAdmin,
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Welcome' }])
  }, [setBreadcrumbs])
  usePageTitle('Welcome to Temps')

  const requested = searchParams.get('step')
  const step: StepKey = isStep(requested)
    ? requested
    : onboarding.data
      ? (nextStep(onboarding.data) ?? 'app')
      : 'host'
  const goTo = (key: StepKey) => setSearchParams({ step: key })
  const following = (key: StepKey): StepKey =>
    STEPS[STEPS.findIndex((s) => s.key === key) + 1]?.key ?? 'app'

  const hostDone = useMutation({
    mutationFn: () => update.mutateAsync({ done: ['host'] }),
    onSuccess: () => goTo('reach'),
  })
  const skip = useMutation({
    mutationFn: () => update.mutateAsync({ completed: true }),
    onSuccess: () => navigate('/projects'),
  })

  return (
    // The shell owns width and padding; the wizard adds neither.
    <PageContainer width="form">
      <Wizard
        fullWidth
        title="Welcome to Temps"
        currentStep={step}
        steps={STEPS.map((s) => ({ id: s.key, label: s.title }))}
        headerActions={
          isAdmin ? (
            <Button
              variant="outline"
              busy={skip.isPending}
              busyLabel="Saving…"
              onClick={() => skip.mutate()}
            >
              Skip setup
            </Button>
          ) : undefined
        }
        footer={null}
      >
        {/* No card around the step: its own lists and choices are the
            surfaces, so nothing nests. */}
        <section aria-label="Current step" className="min-w-0">
          {!isAdmin ? (
            <div className="space-y-4">
              <AdminOnlyNote action="set up this install" role={role} />
              <Button variant="outline" onClick={() => navigate('/projects')}>
                All projects
              </Button>
            </div>
          ) : onboarding.isPending ? (
            <div className="space-y-3" role="status" aria-label="Loading setup">
              <Skeleton className="h-8 w-64" />
              <Skeleton className="h-32 w-full" />
            </div>
          ) : onboarding.isError ? (
            <Callout tone="error" title="Couldn’t load setup progress">
              {problemDetail(onboarding.error, 'Try again.')}
            </Callout>
          ) : step === 'host' ? (
            <HostStep
              facts={host.data}
              isPending={host.isPending}
              error={host.error}
              rechecking={host.isFetching && !host.isPending}
              onRecheck={() => void host.refetch()}
              saving={hostDone.isPending}
              onContinue={() => hostDone.mutate()}
            />
          ) : step === 'reach' ? (
            host.data ? (
              <ReachStep
                facts={factsFromHost(host.data, privateIngress.data)}
                onContinue={() => goTo(following('reach'))}
              />
            ) : host.isError ? (
              <Callout tone="error" title="Couldn’t read this machine">
                {problemDetail(
                  host.error,
                  'The ways in depend on it. Try again.'
                )}
              </Callout>
            ) : (
              <div
                className="space-y-3"
                role="status"
                aria-label="Reading this machine"
              >
                <Skeleton className="h-16 w-full" />
                <Skeleton className="h-16 w-full" />
              </div>
            )
          ) : step === 'agents' ? (
            <AgentsStep onContinue={() => goTo(following('agents'))} />
          ) : (
            <ShipStep />
          )}
          {(hostDone.isError || skip.isError) && (
            <Callout tone="error" title="Not saved" className="mt-4">
              {problemDetail(hostDone.error ?? skip.error, 'Try again.')}
            </Callout>
          )}
        </section>
      </Wizard>
    </PageContainer>
  )
}

export default Welcome
