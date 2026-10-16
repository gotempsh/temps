// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Gauge } from 'lucide-react'
import { Link } from 'react-router'
import { Button } from '@/components/ui/button'
import { usePageTitle } from '@/hooks/usePageTitle'
import { UsageAnalytics } from './AiGateway'

export function AiGatewayUsagePage() {
  usePageTitle('AI Usage')

  return (
    <div className="w-full space-y-4 px-4 py-4 sm:space-y-6 sm:px-6 sm:py-6 lg:px-8">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h1 className="text-2xl sm:text-3xl font-bold">AI Usage</h1>
          <p className="text-muted-foreground mt-1 sm:mt-2 text-sm">
            Token usage and cost per model, across every configured provider.
          </p>
        </div>
        <Button asChild variant="outline" size="sm">
          <Link to="/ai-gateway/limits">
            <Gauge className="mr-2 size-4" />
            Rate limits &amp; budgets
          </Link>
        </Button>
      </div>
      <UsageAnalytics />
    </div>
  )
}
