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
  /** Overrides the logo padding (favicons and template images). */
  logoClassName?: string
  /** Overrides the initial shown when there is no image. */
  fallbackClassName?: string
  /** The name is already written beside the image, so do not announce it twice. */
  decorative?: boolean
}

export function ProjectCardMedia({
  name,
  deploymentUrl,
  screenshotLocation,
  templateImageUrl,
  className,
  logoClassName,
  fallbackClassName,
  decorative,
}: ProjectCardMediaProps) {
  return (
    <ProjectCardMediaContent
      key={`${templateImageUrl ?? ''}:${deploymentUrl ?? ''}:${screenshotLocation ?? ''}`}
      name={name}
      deploymentUrl={deploymentUrl}
      screenshotLocation={screenshotLocation}
      templateImageUrl={templateImageUrl}
      className={className}
      logoClassName={logoClassName}
      fallbackClassName={fallbackClassName}
      decorative={decorative}
    />
  )
}

function ProjectCardMediaContent({
  name,
  deploymentUrl,
  screenshotLocation,
  templateImageUrl,
  className,
  logoClassName,
  fallbackClassName,
  decorative = false,
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
      {...(decorative
        ? { 'aria-hidden': true }
        : { role: 'img', 'aria-label': `${name} service image` })}
      className={cn(
        'relative flex size-10 shrink-0 items-center justify-center overflow-hidden rounded-md border bg-muted/55',
        className
      )}
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
              ? cn('object-contain p-2', logoClassName)
              : 'object-cover object-top'
          )}
          onError={() => setSourceIndex((current) => current + 1)}
        />
      ) : (
        <span
          className={cn(
            'text-sm font-medium text-muted-foreground',
            fallbackClassName
          )}
        >
          {name.slice(0, 1).toUpperCase()}
        </span>
      )}
    </span>
  )
}
