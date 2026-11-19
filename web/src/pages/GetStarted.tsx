// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { Link, useLocation, useSearchParams } from 'react-router'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { OnboardingChoices } from '@/components/onboarding/OnboardingChoices'
import type { SampleDeployNavigationState } from '@/components/first-deploy/SampleDeployCard'
import { SampleDeployTracker } from '@/components/first-deploy/SampleDeployTracker'
import { WorkerNodeRequiredBanner } from '@/components/nodes/WorkerNodeRequiredBanner'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { FIRST_DEPLOY_PATH } from '@/lib/first-deploy'

/** Feature entry points, preserving existing sample-deployment tracking links. */
export function GetStarted() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams] = useSearchParams()
  const location = useLocation()
  const projectSlug = searchParams.get('project')?.trim() || null
  const startError = (location.state as SampleDeployNavigationState | null)
    ?.startError

  useEffect(() => {
    setBreadcrumbs(
      projectSlug
        ? [
            { label: 'Deploy your first app', href: FIRST_DEPLOY_PATH },
            { label: projectSlug },
          ]
        : [{ label: 'Get started' }]
    )
  }, [projectSlug, setBreadcrumbs])

  usePageTitle(projectSlug ? 'Deploy your first app' : 'Get started')

  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title={
          projectSlug
            ? 'Deploy your first app'
            : 'What would you like to try first?'
        }
        description={
          projectSlug
            ? 'Temps is pulling the image, starting the container and routing a URL to it.'
            : undefined
        }
      />

      {projectSlug ? (
        <>
          <WorkerNodeRequiredBanner />
          <SampleDeployTracker
            projectSlug={projectSlug}
            startError={startError}
          />
          <p className="text-sm text-muted-foreground">
            Want to deploy something else?{' '}
            <Link
              className="underline underline-offset-4"
              to={FIRST_DEPLOY_PATH}
            >
              Explore the other features
            </Link>
            .
          </p>
        </>
      ) : (
        <OnboardingChoices showHeading={false} />
      )}
    </PageContainer>
  )
}
