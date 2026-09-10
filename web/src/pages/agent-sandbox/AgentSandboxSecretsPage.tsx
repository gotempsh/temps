// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AgentSecrets } from '@/components/agents/ProjectSecrets'
import { usePageTitle } from '@/hooks/usePageTitle'

// Thin wrapper — AgentSecrets already renders a row-per-secret table view,
// which is exactly the shape we want for this sub-page. No reason to dupe
// the component just to host it under its own route.
export function AgentSandboxSecretsPage() {
  usePageTitle('Agent Secrets')
  return <AgentSecrets />
}
