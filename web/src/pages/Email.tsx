import { EmailDomainsManagement } from '@/components/email/EmailDomainsManagement'
import { EmailProvidersManagement } from '@/components/email/EmailProvidersManagement'
import { EmailsSentList } from '@/components/email/EmailsSentList'
import { SdkDocumentation } from '@/components/email/SdkDocumentation'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'
import { useSearchParams } from 'react-router-dom'

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
      <div className="max-w-7xl mx-auto">
        <div className="space-y-6">
          <div>
            <h1 className="text-3xl font-bold tracking-tight">Email</h1>
            <p className="text-muted-foreground mt-2">
              Configure email providers, manage domains, and view sent emails.
            </p>
          </div>

          <Tabs value={activeTab} onValueChange={handleTabChange}>
            <TabsList className="grid w-full grid-cols-4 lg:w-auto lg:grid-cols-none lg:inline-flex">
              <TabsTrigger value="providers">Providers</TabsTrigger>
              <TabsTrigger value="domains">Domains</TabsTrigger>
              <TabsTrigger value="emails">Sent Emails</TabsTrigger>
              <TabsTrigger value="sdk">SDK</TabsTrigger>
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

            <TabsContent value="sdk" className="mt-6">
              <SdkDocumentation />
            </TabsContent>
          </Tabs>
        </div>
      </div>
    </div>
  )
}
