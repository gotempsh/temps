// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { getSettingsOptions } from '@/api/client/@tanstack/react-query.gen'

/** The active cluster CA fingerprint, `null` while it is initializing. */
export function useClusterCaFingerprint() {
  const settingsQuery = useQuery(getSettingsOptions())
  return {
    fingerprint: settingsQuery.data?.multi_node.cluster_ca_fingerprint ?? null,
    isLoading: settingsQuery.isLoading,
  }
}
