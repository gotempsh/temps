// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { EmailDomainsManagement } from '@/components/email/EmailDomainsManagement'
import { EmailEventLog } from '@/components/email/EmailEventLog'
import {
  EmailGetStarted,
  EmailStatsStrip,
} from '@/components/email/EmailOverview'
import { useEmailOverviewSections } from '@/components/email/useEmailSetup'
import { EmailProvidersManagement } from '@/components/email/EmailProvidersManagement'
import { EmailsSentList } from '@/components/email/EmailsSentList'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { SdkDocumentation } from '@/components/email/SdkDocumentation'
import { PublicIngressNotice } from '@/components/settings/PublicIngressNotice'
import { Skeleton } from '@/components/ui/skeleton'
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
  const sections = useEmailOverviewSections()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Email' }])
  }, [setBreadcrumbs])

  usePageTitle('Email')

  // Waits for the sections: until it is known which show, the one asked for
  // is hidden and there is nothing to scroll to.
  const sectionsKnown = sections !== undefined
  useEffect(() => {
    const section = tabParam ? SECTION_FOR_OLD_TAB[tabParam] : undefined
    if (section && sectionsKnown) {
      document.getElementById(section)?.scrollIntoView?.()
    }
  }, [tabParam, sectionsKnown])

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

          {/* While setup is unfinished the checklist above asks for each
              missing piece, so sections that would only repeat it are hidden
              (emailOverviewSections). They stay mounted, so a dialog or form
              open in one survives the section being hidden. */}
          {!sections && <Skeleton className="h-48 w-full rounded-lg" />}
          {/* Domains lead: they are what people send from. A provider is what
              a domain sends through, so it sits beside them while there is
              room, measured by the page's own width. */}
          <div
            className="@container"
            hidden={!sections?.domains && !sections?.providers}
          >
            <div
              className={
                sections?.domains && sections.providers
                  ? 'grid gap-10 @4xl:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]'
                  : 'grid gap-10'
              }
            >
              <section
                id="email-domains"
                className="min-w-0 scroll-mt-4"
                hidden={!sections?.domains}
              >
                <EmailDomainsManagement />
              </section>
              <section
                id="email-providers"
                className="min-w-0 scroll-mt-4"
                hidden={!sections?.providers}
              >
                <EmailProvidersManagement />
              </section>
            </div>
          </div>

          <section
            id="email-sent"
            className="scroll-mt-4"
            hidden={!sections?.sent}
          >
            <EmailsSentList />
          </section>

          <section
            id="email-events"
            className="scroll-mt-4"
            hidden={!sections?.events}
          >
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
