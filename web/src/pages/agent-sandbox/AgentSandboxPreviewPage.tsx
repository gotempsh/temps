// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PreviewGatewayCard } from '@/components/settings/PreviewGatewayCard'
import { usePageTitle } from '@/hooks/usePageTitle'

// Thin wrapper — PreviewGatewayCard already owns its status fetching, image
// upgrade flow, and logs viewer. No need to fragment it further.
export function AgentSandboxPreviewPage() {
  usePageTitle('Preview Gateway')
  return <PreviewGatewayCard />
}
