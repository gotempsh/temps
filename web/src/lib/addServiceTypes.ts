// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { CreatableServiceTypeRoute } from '@/api/client'

export interface AddServiceTypeOption {
  id: CreatableServiceTypeRoute
  name: string
  description: string
}

/**
 * The "Add Service" picker's catalogue, shared by every project configurator
 * (new-project wizard, manual setup, templates) so they can't drift out of
 * sync with each other or with the backend's `ServiceTypeRoute` values.
 */
export const ADD_SERVICE_TYPES: AddServiceTypeOption[] = [
  {
    id: 'postgres',
    name: 'PostgreSQL',
    description: 'Reliable Relational Database',
  },
  {
    id: 'mariadb',
    name: 'MariaDB',
    description: 'MySQL-compatible Database',
  },
  {
    id: 'mongodb',
    name: 'MongoDB',
    description: 'NoSQL Document Database',
  },
  {
    id: 'redis',
    name: 'Redis',
    description: 'In-Memory Data Store',
  },
  {
    id: 's3',
    name: 'S3 / RustFS',
    description: 'S3-compatible Object Storage',
  },
]
