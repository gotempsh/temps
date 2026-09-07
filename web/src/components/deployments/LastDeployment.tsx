// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { DeploymentResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { ReloadableImage } from '@/components/utils/ReloadableImage'
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  ArrowRight,
  Camera,
  Container,
  ExternalLink,
  FileArchive,
  GitBranch,
} from 'lucide-react'
import { Link } from 'react-router'
import { normalizeUrl } from '@/lib/deployment-url'
import { deploymentSourceLabel } from '@/lib/deployment-source-label'
import { DeploymentStatusBadge } from '../deployment/DeploymentStatusBadge'

interface LastDeploymentProps {
  deployment: DeploymentResponse
  projectName: string
  screenshotsEnabled?: boolean
}

export function LastDeployment({
  deployment,
  projectName,
  screenshotsEnabled,
}: LastDeploymentProps) {
  const primaryUrl = deployment.environment.domains[0] ?? deployment.url
  const primaryHref = normalizeUrl(primaryUrl)
  const screenshotLocation = deployment.screenshot_location
  const screenshotUrl = screenshotLocation
    ? `/api/files${screenshotLocation.startsWith('/') ? screenshotLocation : `/${screenshotLocation}`}`
    : null
  const sourceType = deployment.metadata?.deploymentSourceType
  const SourceIcon =
    sourceType === 'docker_image'
      ? Container
      : sourceType === 'uploaded_source' || sourceType === 'static_files'
        ? FileArchive
        : GitBranch

  return (
    <Card className="overflow-hidden">
      <CardContent className="p-0">
        <div className="grid lg:grid-cols-[minmax(18rem,0.85fr)_minmax(0,1.15fr)]">
          <DeploymentPreview
            screenshotUrl={screenshotUrl}
            primaryHref={primaryHref}
            alt={`${projectName} latest deployment preview`}
            status={deployment.status}
            screenshotsEnabled={screenshotsEnabled}
          />

          <div className="flex min-w-0 flex-col justify-between gap-5 p-4 sm:p-5 lg:p-6">
            <div className="min-w-0 space-y-4">
              <div className="flex flex-wrap items-center gap-2">
                <h3 className="text-sm font-semibold">Latest deployment</h3>
                <DeploymentStatusBadge deployment={deployment} />
                <span className="text-xs text-muted-foreground">
                  <TimeAgo date={deployment.created_at} />
                  {deployment.commit_author
                    ? ` by ${deployment.commit_author}`
                    : ''}
                </span>
              </div>
              <div className="min-w-0">
                <p className="truncate text-sm font-medium">
                  {deployment.commit_message || 'Manual deployment'}
                </p>
                <p className="mt-1 flex items-center gap-1.5 text-xs text-muted-foreground">
                  <SourceIcon className="size-3.5 shrink-0" />
                  <span className="truncate">
                    {deploymentSourceLabel(deployment)}
                    {deployment.commit_hash
                      ? ` · ${deployment.commit_hash.slice(0, 7)}`
                      : ''}
                  </span>
                </p>
              </div>
              {primaryUrl && <DeploymentUrlRow value={primaryUrl} />}
            </div>

            <div className="flex flex-wrap items-center gap-2">
              {primaryHref && (
                <Button variant="outline" size="sm" asChild>
                  <a
                    href={primaryHref}
                    target="_blank"
                    rel="noopener noreferrer"
                  >
                    Open site
                    <ExternalLink className="ml-1.5 size-3.5" />
                  </a>
                </Button>
              )}
              <Button variant="ghost" size="sm" asChild>
                <Link
                  to={`/projects/${projectName}/deployments/${deployment.id}`}
                >
                  Details
                  <ArrowRight className="ml-1.5 size-3.5" />
                </Link>
              </Button>
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}

function DeploymentPreview({
  screenshotUrl,
  primaryHref,
  alt,
  status,
  screenshotsEnabled,
}: {
  screenshotUrl: string | null
  primaryHref: string | null
  alt: string
  status: DeploymentResponse['status']
  screenshotsEnabled: boolean | undefined
}) {
  const preview = screenshotUrl ? (
    <ReloadableImage
      src={screenshotUrl}
      alt={alt}
      className="size-full object-cover object-top transition-transform duration-300 group-hover/preview:scale-[1.015]"
    />
  ) : screenshotsEnabled === false ? (
    <div className="flex size-full flex-col items-center justify-center gap-2 bg-muted/35 px-6 text-center text-muted-foreground">
      <Camera className="size-5" />
      <span className="text-xs">Screenshot previews are off</span>
      <Button variant="outline" size="sm" asChild>
        <Link to="/settings">Enable in settings</Link>
      </Button>
    </div>
  ) : (
    <div className="flex size-full flex-col items-center justify-center gap-2 bg-muted/35 px-6 text-center text-muted-foreground">
      <Camera className="size-5" />
      <span className="text-xs">
        {status === 'completed'
          ? 'Generating preview…'
          : 'Preview available after deployment'}
      </span>
    </div>
  )

  const className =
    'group/preview aspect-[16/9] min-h-44 overflow-hidden border-b bg-muted/25 lg:aspect-auto lg:min-h-56 lg:border-b-0 lg:border-r'

  return screenshotUrl && primaryHref ? (
    <a
      href={primaryHref}
      target="_blank"
      rel="noopener noreferrer"
      className={`block ${className}`}
      aria-label="Open latest deployment"
    >
      {preview}
    </a>
  ) : (
    <div className={className}>{preview}</div>
  )
}

function DeploymentUrlRow({ value }: { value: string }) {
  const href = normalizeUrl(value)
  return (
    <div className="flex min-w-0 items-center gap-1">
      {href ? (
        <>
          <a
            href={href}
            target="_blank"
            rel="noopener noreferrer"
            className="flex min-w-0 items-center gap-1 hover:opacity-80 transition-opacity"
          >
            <span className="truncate text-sm text-muted-foreground">
              {value}
            </span>
            <ExternalLink className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          </a>
          <CopyButton
            value={href}
            minimal
            label="Copy URL"
            className="h-6 w-6 shrink-0 p-0 text-muted-foreground"
          />
        </>
      ) : (
        <span className="truncate text-sm text-muted-foreground">{value}</span>
      )}
    </div>
  )
}
