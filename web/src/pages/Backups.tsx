import { BackupsManagement } from '@/components/backups/BackupsManagement'
import { S3SourcesManagement } from '@/components/backups/S3SourcesManagement'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'

export function Backups() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Backups' }])
  }, [setBreadcrumbs])

  usePageTitle('Backups')

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <Tabs defaultValue="s3" className="space-y-6">
          <TabsList>
            <TabsTrigger value="s3">S3 Sources</TabsTrigger>
            <TabsTrigger value="backups">Backups</TabsTrigger>
          </TabsList>
          <TabsContent value="backups">
            <BackupsManagement />
          </TabsContent>
          <TabsContent value="s3">
            <S3SourcesManagement />
          </TabsContent>
        </Tabs>
      </div>
    </div>
  )
}
