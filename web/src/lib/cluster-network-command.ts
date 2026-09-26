// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function buildMultiNodeSetupCommand(poolCidr: string, prefix: string) {
  return `temps network setup-multi-node --compute-pool-cidr ${poolCidr} --node-prefix-len ${prefix}`
}
