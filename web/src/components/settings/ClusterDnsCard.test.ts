// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildMultiNodeSetupCommand } from '@/lib/cluster-network-command'

describe('buildMultiNodeSetupCommand', () => {
  test('uses the selected cluster pool and per-node prefix', () => {
    expect(buildMultiNodeSetupCommand('10.240.0.0/16', '24')).toBe(
      'temps network setup-multi-node --compute-pool-cidr 10.240.0.0/16 --node-prefix-len 24'
    )
  })
})
