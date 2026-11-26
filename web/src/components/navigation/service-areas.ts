// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectSectionId } from '@/lib/project-navigation'

/**
 * What is missing while a view of a service holds nothing, said beside it
 * under "Add" on the service's page. A view with no entry needs no set-up
 * and is always a tab.
 */
export const SERVICE_VIEW_MISSING: Partial<Record<ProjectSectionId, string>> = {
  analytics: 'No page view received yet.',
}
