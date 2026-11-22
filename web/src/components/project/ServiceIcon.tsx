// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { skipToken, useQuery } from '@tanstack/react-query'
import { ProjectCardMedia } from '@/components/dashboard/ProjectCardMedia'
import { ROUTE_FAILURE_IGNORE } from '@/lib/route-query-failures'
import { loadServiceMedia } from '@/lib/service-media-batcher'
import { cn } from '@/lib/utils'

const SIZES = {
  xxs: { box: 'size-4 rounded-sm', logo: 'p-0', initial: 'text-[10px]' },
  xs: { box: 'size-5 rounded-sm', logo: 'p-0.5', initial: 'text-[10px]' },
  sm: { box: 'size-6 rounded-md', logo: 'p-0.5', initial: 'text-xs' },
  md: { box: 'size-8 rounded-md', logo: 'p-1', initial: 'text-sm' },
  lg: { box: 'size-10 rounded-md', logo: 'p-2', initial: 'text-sm' },
} as const

export interface ServiceIconProps {
  /**
   * Leave `id` out for a project the user may not be able to read: one such id
   * fails the batched request, and every icon sharing it loses its image.
   */
  service: {
    id?: number
    name: string
    service_template_image_url?: string | null
    source_type?: string | null
  }
  size?: keyof typeof SIZES
  className?: string
}

/**
 * The one icon for a project or service, wherever one is listed beside its
 * name: its template image, then the favicon of its latest deployment, then
 * that deployment's screenshot, then its initial. Fetches its own media; icons
 * rendered together share one request (see service-media-batcher). Logos are
 * background context, so a failed read never reports the page as failing.
 */
export function ServiceIcon({
  service,
  size = 'md',
  className,
}: ServiceIconProps) {
  // An external project has no deployments to ask about.
  const mediaId = service.source_type === 'external' ? undefined : service.id
  const media = useQuery({
    queryKey: ['service-media', mediaId],
    queryFn:
      mediaId === undefined ? skipToken : () => loadServiceMedia(mediaId),
    staleTime: 60_000,
    retry: false,
    meta: { routeFailure: ROUTE_FAILURE_IGNORE },
  })
  const s = SIZES[size]
  return (
    <ProjectCardMedia
      decorative
      name={service.name}
      deploymentUrl={media.data?.url}
      screenshotLocation={media.data?.screenshot_location}
      templateImageUrl={service.service_template_image_url}
      className={cn(s.box, className)}
      logoClassName={s.logo}
      fallbackClassName={s.initial}
    />
  )
}
