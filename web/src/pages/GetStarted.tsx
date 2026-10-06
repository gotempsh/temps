// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { Link, useLocation, useSearchParams } from 'react-router'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { DeployPathChoices } from '@/components/first-deploy/DeployPathChoices'
import { SampleDeployCard } from '@/components/first-deploy/SampleDeployCard'
import type { SampleDeployNavigationState } from '@/components/first-deploy/SampleDeployCard'
import { SampleDeployTracker } from '@/components/first-deploy/SampleDeployTracker'
import { WorkerNodeRequiredBanner } from '@/components/nodes/WorkerNodeRequiredBanner'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { FIRST_DEPLOY_PATH } from '@/lib/first-deploy'

/**
 * "Deploy your first app": the do-this-first path for a new installation.
 *
 * Without `?project=` it offers the one-click sample and the three ways to
 * deploy your own app. With it, it follows that project's deployment to a
 * live URL. Linked from the Platform setup checklist so it stays reachable
 * after the projects page stops showing its empty state.
 */
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
        : [{ label: 'Deploy your first app' }]
    )
  }, [projectSlug, setBreadcrumbs])

  usePageTitle('Deploy your first app')

  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Deploy your first app"
        description={
          projectSlug
            ? 'Temps is pulling the image, starting the container and routing a URL to it.'
            : 'Get something live on this server first. It proves Docker, image downloads and routing all work, and takes about a minute.'
        }
      />

      <WorkerNodeRequiredBanner />

      {projectSlug ? (
        <>
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
              See all the ways to deploy
            </Link>
            .
          </p>
        </>
      ) : (
        <>
          <SampleDeployCard />
          <section aria-labelledby="own-app-title" className="space-y-3">
            <h2 id="own-app-title" className="text-lg font-semibold">
              Or deploy your own app
            </h2>
            <DeployPathChoices />
          </section>
        </>
      )}
    </PageContainer>
  )
}
