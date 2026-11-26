// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  MissingLinkedDatabase,
  RestoredDatabasesReport,
} from '@/api/client/types.gen'

export const missing = (
  overrides: Partial<MissingLinkedDatabase>
): MissingLinkedDatabase => ({
  project_id: 1,
  project_name: 'Shop',
  project_slug: 'shop',
  environment_name: 'production',
  expected_database: 'shop_production',
  database_provisioning_mode: 'project_environment',
  environment_names: ['production', 'staging'],
  suggested_database: 'shop_prod',
  ...overrides,
})

export const report: RestoredDatabasesReport = {
  restore_run_id: 21,
  restored_at: '2026-11-26T10:00:00Z',
  truncated: false,
  databases: [
    {
      name: 'app',
      kind: 'service',
      used_by: [],
      used_by_other_projects: false,
      usable_by_link: true,
    },
    {
      name: 'blog',
      kind: 'linked',
      used_by: [
        { project_id: 2, project_name: 'Blog', environment_name: null },
      ],
      used_by_other_projects: true,
      usable_by_link: true,
    },
    {
      name: 'Legacy-Data',
      kind: 'unused',
      used_by: [],
      used_by_other_projects: false,
      usable_by_link: false,
    },
    {
      name: 'shop_prod',
      kind: 'unused',
      used_by: [],
      used_by_other_projects: false,
      usable_by_link: true,
    },
  ],
  roles: [{ name: 'origin-admin', superuser: true }],
  missing: [
    missing({}),
    missing({
      environment_name: 'staging',
      expected_database: 'shop_staging',
    }),
  ],
}
