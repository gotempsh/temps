import { cn } from '@/lib/utils'
import { useState } from 'react'
import { projectCardMediaSources } from './project-card-media'

interface ProjectCardMediaProps {
  name: string
  deploymentUrl?: string | null
  screenshotLocation?: string | null
  className?: string
}

export function ProjectCardMedia({
  name,
  deploymentUrl,
  screenshotLocation,
  className,
}: ProjectCardMediaProps) {
  return (
    <ProjectCardMediaContent
      key={`${deploymentUrl ?? ''}:${screenshotLocation ?? ''}`}
      name={name}
      deploymentUrl={deploymentUrl}
      screenshotLocation={screenshotLocation}
      className={className}
    />
  )
}

function ProjectCardMediaContent({
  name,
  deploymentUrl,
  screenshotLocation,
  className,
}: ProjectCardMediaProps) {
  const sources = projectCardMediaSources(deploymentUrl, screenshotLocation)
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
            source.kind === 'favicon'
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
