// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getPlatformFeaturesOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'

/**
 * Which capabilities this server process actually provides.
 *
 * A `control-plane` profile runs no local workloads of its own — application
 * containers, image builds, agent sandboxes, and workload importers all run
 * on worker nodes joined with `temps join` instead. The console cannot infer
 * this from a 404/500 on those endpoints, so it must ask `/platform/features`
 * directly and render an honest "not available in this profile" state rather
 * than a dead button or a silently-empty page.
 *
 * Cached for the session — the profile a server was started with never
 * changes without a restart.
 */
export function usePlatformFeatures() {
  return useQuery({
    ...getPlatformFeaturesOptions(),
    staleTime: 5 * 60_000,
    retry: false,
  })
}
