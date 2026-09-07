// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import { useState } from 'react'
import { projectCardMediaSources } from './project-card-media'

interface ProjectCardMediaProps {
  name: string
  deploymentUrl?: string | null
  screenshotLocation?: string | null
  templateImageUrl?: string | null
  className?: string
}

export function ProjectCardMedia({
  name,
  deploymentUrl,
  screenshotLocation,
  templateImageUrl,
  className,
}: ProjectCardMediaProps) {
  return (
    <ProjectCardMediaContent
      key={`${templateImageUrl ?? ''}:${deploymentUrl ?? ''}:${screenshotLocation ?? ''}`}
      name={name}
      deploymentUrl={deploymentUrl}
      screenshotLocation={screenshotLocation}
      templateImageUrl={templateImageUrl}
      className={className}
    />
  )
}

function ProjectCardMediaContent({
  name,
  deploymentUrl,
  screenshotLocation,
  templateImageUrl,
  className,
}: ProjectCardMediaProps) {
  const sources = projectCardMediaSources(
    deploymentUrl,
    screenshotLocation,
    templateImageUrl
  )
  const [sourceIndex, setSourceIndex] = useState(0)
  const source = sources[sourceIndex]

  return (
    <span
      className={cn(
        'relative flex size-10 shrink-0 items-center justify-center overflow-hidden rounded-md border bg-muted/55',
        className
      )}
      aria-label={`${name} project image`}
    >
      {source ? (
        <img
          src={source.src}
          alt=""
          loading="lazy"
          decoding="async"
          className={cn(
            'size-full',
            source.kind === 'favicon' || source.kind === 'template'
              ? 'object-contain p-2'
              : 'object-cover object-top'
          )}
          onError={() => setSourceIndex((current) => current + 1)}
        />
      ) : (
        <span className="text-sm font-medium text-muted-foreground">
          {name.slice(0, 1).toUpperCase()}
        </span>
      )}
    </span>
  )
}
