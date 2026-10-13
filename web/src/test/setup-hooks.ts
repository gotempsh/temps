// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach } from 'bun:test'
import { restoreAllMockApis } from './mock-registry'

// Preloaded hooks apply to every test file: a failing assertion must not leave
// a test's fake API routes installed on the shared client.
afterEach(restoreAllMockApis)
