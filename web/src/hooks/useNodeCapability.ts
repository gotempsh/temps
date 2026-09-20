// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  nodeCapabilityOptions,
  type NodeCapability,
} from '@/api/nodeCapability'
import { useQuery } from '@tanstack/react-query'

/**
 * Whether this installation can run a workload (locally or on a worker node).
 *
 * Swap `nodeCapabilityOptions()` for the generated
 * `getNodeCapabilityOptions()` once the SDK has been regenerated; nothing
 * else here changes.
 */
export function useNodeCapability() {
  return useQuery<NodeCapability>(nodeCapabilityOptions())
}
