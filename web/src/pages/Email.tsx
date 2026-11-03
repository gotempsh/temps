// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { EmailDomainsManagement } from '@/components/email/EmailDomainsManagement'
import { EmailEventLog } from '@/components/email/EmailEventLog'
import {
  EmailGetStarted,
  EmailStatsStrip,
} from '@/components/email/EmailOverview'
import { EmailProvidersManagement } from '@/components/email/EmailProvidersManagement'
import { EmailsSentList } from '@/components/email/EmailsSentList'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { SdkDocumentation } from '@/components/email/SdkDocumentation'
import { PublicIngressNotice } from '@/components/settings/PublicIngressNotice'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'
import { useSearchParams } from 'react-router'
import { Code, Mail } from 'lucide-react'

/** Where the page's former tabs live now, so old links land on the content. */
const SECTION_FOR_OLD_TAB: Record<string, string> = {
  providers: 'email-providers',
  domains: 'email-domains',
  emails: 'email-sent',
  analytics: 'email-events',
}

export function Email() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams, setSearchParams] = useSearchParams()
  const tabParam = searchParams.get('tab')
  const activeTab = tabParam === 'sdk' ? 'sdk' : 'overview'

  useEffect(() => {
    setBreadcrumbs([{ label: 'Email' }])
  }, [setBreadcrumbs])

  usePageTitle('Email')

  useEffect(() => {
    const section = tabParam ? SECTION_FOR_OLD_TAB[tabParam] : undefined
    if (section) document.getElementById(section)?.scrollIntoView?.()
  }, [tabParam])

  return (
    <PageContainer>
      <PageHeader
        title="Email"
        description="Send email from your own domains, and see what happened to each one."
      />

      <PublicIngressNotice title="Opens, clicks and delivery events will not be recorded" />

      <Tabs
        value={activeTab}
        onValueChange={(value) =>
          setSearchParams(value === 'sdk' ? { tab: 'sdk' } : {})
        }
      >
        <TabsList className="w-full lg:w-auto">
          <TabsTrigger value="overview" icon={Mail}>
            Overview
          </TabsTrigger>
          <TabsTrigger value="sdk" icon={Code}>
            SDK
          </TabsTrigger>
        </TabsList>

        <TabsContent value="overview" className="mt-6 space-y-10">
          <EmailGetStarted />
          <EmailStatsStrip />

          {/* Domains lead: they are what people send from. A provider is what
              a domain sends through, so it sits beside them while there is
              room, measured by the page's own width. */}
          <div className="@container">
            <div className="grid gap-10 @4xl:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
              <section id="email-domains" className="min-w-0 scroll-mt-4">
                <EmailDomainsManagement />
              </section>
              <section id="email-providers" className="min-w-0 scroll-mt-4">
                <EmailProvidersManagement />
              </section>
            </div>
          </div>

          <section id="email-sent" className="scroll-mt-4">
            <EmailsSentList />
          </section>

          <section id="email-events" className="scroll-mt-4">
            <EmailEventLog />
          </section>
        </TabsContent>

        <TabsContent value="sdk" className="mt-6">
          <SdkDocumentation />
        </TabsContent>
      </Tabs>
    </PageContainer>
  )
}
