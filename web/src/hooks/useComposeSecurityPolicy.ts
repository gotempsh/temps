// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { getComposeSecurityOptions } from '@/api/client/@tanstack/react-query.gen'

/**
 * The project's Docker Compose security policy.
 *
 * Only Compose projects have one: the endpoint answers 400 "does not use
 * Docker Compose" for every other preset, so the request is not sent at all
 * unless `isComposePreset` is true.
 */
export function useComposeSecurityPolicy(
  projectId: number,
  isComposePreset: boolean
) {
  return useQuery({
    ...getComposeSecurityOptions({ path: { id: projectId } }),
    enabled: isComposePreset,
  })
}
