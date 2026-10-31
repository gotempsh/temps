// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { EmailAnalytics } from '@/components/email/EmailAnalytics'
import { EmailDomainsManagement } from '@/components/email/EmailDomainsManagement'
import { EmailProvidersManagement } from '@/components/email/EmailProvidersManagement'
import { EmailsSentList } from '@/components/email/EmailsSentList'
import { PageHeader } from '@/components/layout/PageContainer'
import { SdkDocumentation } from '@/components/email/SdkDocumentation'
import { PublicIngressNotice } from '@/components/settings/PublicIngressNotice'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'
import { useSearchParams } from 'react-router'
import { BarChart3, Code, Globe, Send, Server } from 'lucide-react'

export function Email() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams, setSearchParams] = useSearchParams()
  const activeTab = searchParams.get('tab') || 'providers'

  useEffect(() => {
    setBreadcrumbs([{ label: 'Email' }])
  }, [setBreadcrumbs])

  usePageTitle('Email')

  const handleTabChange = (value: string) => {
    setSearchParams({ tab: value })
  }

  return (
    <div className="w-full px-4 sm:px-6 lg:px-8 py-8">
      <div className="w-full">
        <div className="space-y-6">
          <PageHeader
            title="Email"
            description="Configure email providers, manage domains, and view sent emails."
          />

          <PublicIngressNotice title="Opens, clicks and delivery events will not be recorded" />

          <Tabs value={activeTab} onValueChange={handleTabChange}>
            <TabsList className="w-full lg:w-auto">
              <TabsTrigger value="providers" icon={Server}>
                Providers
              </TabsTrigger>
              <TabsTrigger value="domains" icon={Globe}>
                Domains
              </TabsTrigger>
              <TabsTrigger value="emails" icon={Send}>
                Sent emails
              </TabsTrigger>
              <TabsTrigger value="analytics" icon={BarChart3}>
                Analytics
              </TabsTrigger>
              <TabsTrigger value="sdk" icon={Code}>
                SDK
              </TabsTrigger>
            </TabsList>

            <TabsContent value="providers" className="mt-6">
              <EmailProvidersManagement />
            </TabsContent>

            <TabsContent value="domains" className="mt-6">
              <EmailDomainsManagement />
            </TabsContent>

            <TabsContent value="emails" className="mt-6">
              <EmailsSentList />
            </TabsContent>

            <TabsContent value="analytics" className="mt-6">
              <EmailAnalytics />
            </TabsContent>

            <TabsContent value="sdk" className="mt-6">
              <SdkDocumentation />
            </TabsContent>
          </Tabs>
        </div>
      </div>
    </div>
  )
}
