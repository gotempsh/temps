// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  ServiceIcon,
  type ServiceIconProps,
} from '@/components/project/ServiceIcon'
import { useQuery } from '@tanstack/react-query'
import { chatProjectQueryOptions } from './chat-project-media'

/**
 * `ServiceIcon` for a project a chat only knows by id and name. A chat can name
 * a project the user has since lost access to, so the media is only asked for
 * once the project itself has loaded; until then, or without access, it is the
 * initial.
 */
export function ChatProjectIcon({
  projectId,
  name,
  size,
  className,
}: {
  projectId: number
  name: string
  size?: ServiceIconProps['size']
  className?: string
}) {
  const project = useQuery(chatProjectQueryOptions(projectId))
  return (
    <ServiceIcon
      service={project.data ?? { name }}
      size={size}
      className={className}
    />
  )
}
