// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Gauge } from 'lucide-react'
import { Link } from 'react-router'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Button } from '@/components/ui/button'
import { usePageTitle } from '@/hooks/usePageTitle'
import { UsageAnalytics } from './AiGateway'

export function AiGatewayUsagePage() {
  usePageTitle('AI usage')

  return (
    <PageContainer>
      <PageHeader
        title="AI usage"
        description="Token usage and cost per model, across every configured provider."
        actions={
          <Button asChild variant="outline" size="sm">
            <Link to="/ai-gateway/limits">
              <Gauge className="size-4 sm:mr-2" />
              <span className="max-sm:sr-only">Rate limits &amp; budgets</span>
            </Link>
          </Button>
        }
      />
      <UsageAnalytics />
    </PageContainer>
  )
}
