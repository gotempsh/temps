// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { ProjectCardMedia } from '@/components/dashboard/ProjectCardMedia'
import { loadServiceMedia } from '@/lib/service-media-batcher'
import { cn } from '@/lib/utils'

const SIZES = {
  xs: { box: 'size-5 rounded-sm', logo: 'p-0.5', initial: 'text-[10px]' },
  sm: { box: 'size-6 rounded-md', logo: 'p-0.5', initial: 'text-xs' },
  md: { box: 'size-8 rounded-md', logo: 'p-1', initial: 'text-sm' },
  lg: { box: 'size-10 rounded-md', logo: 'p-2', initial: 'text-sm' },
} as const

export interface ServiceIconProps {
  service: { id: number; name: string; service_template_image_url?: string | null }
  size?: keyof typeof SIZES
  className?: string
}

/**
 * A service's icon anywhere services are listed: its template image, then the
 * favicon of its latest deployment, then that deployment's screenshot, then
 * its initial. Fetches its own media; icons rendered together share one
 * request (see service-media-batcher).
 */
export function ServiceIcon({ service, size = 'md', className }: ServiceIconProps) {
  const media = useQuery({
    queryKey: ['service-media', service.id],
    queryFn: () => loadServiceMedia(service.id),
    staleTime: 60_000,
    retry: false,
  })
  const s = SIZES[size]
  return (
    <ProjectCardMedia
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
