// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { NotificationRoutesManagement } from '@/components/monitoring/NotificationRoutesManagement'
import { ProvidersManagement } from '@/components/monitoring/ProvidersManagement'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Route, Webhook } from 'lucide-react'
import { useEffect } from 'react'
import { useSearchParams } from 'react-router'

export function Notifications() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams, setSearchParams] = useSearchParams()
  const activeTab =
    searchParams.get('tab') === 'routes' ? 'routes' : 'providers'

  useEffect(() => {
    setBreadcrumbs([{ label: 'Notifications' }])
  }, [setBreadcrumbs])

  usePageTitle('Notifications')

  return (
    <div className="w-full px-4 sm:px-6 lg:px-8 py-8">
      <div className="w-full">
        <Tabs
          value={activeTab}
          onValueChange={(tab) =>
            setSearchParams(tab === 'routes' ? { tab: 'routes' } : {})
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
            <ProvidersManagement />
          </TabsContent>
          <TabsContent value="routes">
            <NotificationRoutesManagement />
          </TabsContent>
        </Tabs>
      </div>
    </div>
  )
}
