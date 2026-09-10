// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { usePageTitle } from '@/hooks/usePageTitle'
import { AgentActivity } from './AiGateway'

export function AiGatewayActivityPage() {
  usePageTitle('AI Traces')

  return (
    <div className="container mx-auto px-4 sm:px-6 py-4 sm:py-6 space-y-4 sm:space-y-6">
      <div>
        <h1 className="text-2xl sm:text-3xl font-bold">AI Traces</h1>
        <p className="text-muted-foreground mt-1 sm:mt-2 text-sm">
          OpenTelemetry traces from your AI workloads (gen_ai.* spans).
        </p>
      </div>
      <AgentActivity />
    </div>
  )
}
