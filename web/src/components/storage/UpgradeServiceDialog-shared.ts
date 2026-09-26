// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

export const upgradeFormSchema = z.object({
  docker_image: z
    .string()
    .min(1, 'Docker image is required')
    .regex(
      /^[\w.\-/:]+$/,
      'Invalid Docker image format. Example: postgres:18-bookworm'
    ),
})

export type UpgradeFormValues = z.infer<typeof upgradeFormSchema>

export interface SupportedImage {
  image: string
  label: string
}

/**
 * Returns the list of supported WAL-G images for a given service type.
 */
// Pure catalog helper shared with the service edit form.
export function getSupportedImages(serviceType: string): SupportedImage[] {
  if (serviceType === 'postgres') {
    return [
      {
        image: 'gotempsh/postgres-walg:18-bookworm',
        label: 'PostgreSQL 18 + WAL-G',
      },
      {
        image: 'gotempsh/postgres-walg:17-bookworm',
        label: 'PostgreSQL 17 + WAL-G',
      },
      { image: 'gotempsh/pgvector-walg:pg18', label: 'pgvector 18 + WAL-G' },
      { image: 'gotempsh/pgvector-walg:pg17', label: 'pgvector 17 + WAL-G' },
      {
        image: 'gotempsh/timescaledb-walg:pg18',
        label: 'TimescaleDB (PG 18) + WAL-G',
      },
    ]
  }
  if (serviceType === 'redis') {
    return [
      { image: 'gotempsh/redis-walg:8-bookworm', label: 'Redis 8 + WAL-G' },
    ]
  }
  if (serviceType === 'mongodb') {
    return [
      { image: 'gotempsh/mongodb-walg:8.0', label: 'MongoDB 8.0 + WAL-G' },
    ]
  }
  return []
}

export interface UpgradeServiceDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceId: number
  serviceName: string
  currentImage?: string
  serviceType: string
  /** Called after a successful upgrade so the parent can refetch service data. */
  onSuccess?: () => void
}
