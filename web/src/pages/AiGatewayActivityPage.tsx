// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader } from '@/components/layout/PageContainer'
import { usePageTitle } from '@/hooks/usePageTitle'
import { AgentActivity } from './AiGateway'

export function AiGatewayActivityPage() {
  usePageTitle('AI traces')

  return (
    <div className="w-full space-y-4 px-4 py-4 sm:space-y-6 sm:px-6 sm:py-6 lg:px-8">
      <PageHeader
        title="AI traces"
        description="OpenTelemetry traces from your AI workloads (gen_ai.* spans)."
      />
      <AgentActivity />
    </div>
  )
}
