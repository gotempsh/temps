// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader } from '@/components/layout/PageContainer'

import { NotificationRoutesManagement } from '@/components/monitoring/NotificationRoutesManagement'
import { ProvidersManagement } from '@/components/monitoring/ProvidersManagement'
import { Button } from '@/components/ui/button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Route, Webhook } from 'lucide-react'
import { useEffect } from 'react'
import { useNavigate, useSearchParams } from 'react-router'
import { returnNavigation, safeReturnTo } from '@/lib/safe-return-to'

export function Notifications() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const activeTab =
    searchParams.get('tab') === 'routes' ? 'routes' : 'providers'
  // Set when a task (an alert rule form) sent the user here to fix delivery.
  const returnTo = safeReturnTo(searchParams.get('returnTo'))

  useEffect(() => {
    setBreadcrumbs([{ label: 'Notifications' }])
  }, [setBreadcrumbs])

  usePageTitle('Notifications')

  return (
    <div className="w-full min-w-0 space-y-6">
      <PageHeader
        title="Notifications"
        description="Providers and routes apply to all projects on this instance. Alert rules are set per project and deliver through these routes."
        actions={
          returnTo ? (
            <Button onClick={() => navigate(returnTo, returnNavigation())}>
              Return to your task
            </Button>
          ) : undefined
        }
      />
      <div className="w-full">
        <Tabs
          value={activeTab}
          onValueChange={(tab) =>
            setSearchParams({
              ...(tab === 'routes' ? { tab: 'routes' } : {}),
              ...(returnTo ? { returnTo } : {}),
            })
          }
        >
          <TabsList className="mb-6">
            <TabsTrigger value="providers" className="gap-2">
              <Webhook className="h-4 w-4" />
              Providers
            </TabsTrigger>
            <TabsTrigger value="routes" className="gap-2">
              <Route className="h-4 w-4" />
              Routes
            </TabsTrigger>
          </TabsList>
          <TabsContent value="providers">
            <ProvidersManagement returnTo={returnTo} />
          </TabsContent>
          <TabsContent value="routes">
            <NotificationRoutesManagement returnTo={returnTo} />
          </TabsContent>
        </Tabs>
      </div>
    </div>
  )
}
