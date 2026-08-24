import { ProjectResponse } from '@/api/client'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useSearchParams } from 'react-router'
import { BuildSettings } from './GitSettings'
import { DeployDefaultsCard } from './DeployDefaultsCard'
import { DeploymentSourceCard } from './DeploymentSourceCard'
import { EnvironmentPortOverrideCard } from './EnvironmentPortOverrideCard'
import { ImageRetentionCard } from './ImageRetentionCard'
import { PreviewEnvironmentsCard } from './PreviewEnvironmentsCard'

const TABS = ['source', 'build', 'deploy', 'previews'] as const
type TabValue = (typeof TABS)[number]

function isTab(value: string | null): value is TabValue {
  return value !== null && (TABS as readonly string[]).includes(value)
}

/**
 * Everything about how a project turns into a running deployment, in pipeline
 * order: where the code comes from, how it is built, how it is rolled out, and
 * how throwaway branch environments behave.
 *
 * These cards used to be split between this page and General, which meant the
 * page that owns `preset`/`directory` said nothing about the deployment source
 * that can rewrite them. Observability toggles deliberately stay on General —
 * they describe what a deployment reports, not how it ships.
 *
 * The active tab lives in the query string so a tab is linkable and survives a
 * reload, rather than silently resetting to the first one.
 */
export function BuildDeploySettings({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const [searchParams, setSearchParams] = useSearchParams()
  const requested = searchParams.get('tab')
  const active: TabValue = isTab(requested) ? requested : 'source'

  const selectTab = (value: string) => {
    const next = new URLSearchParams(searchParams)
    next.set('tab', value)
    setSearchParams(next, { replace: true })
  }

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h2 className="text-xl font-semibold text-balance">
          Build and deployment
        </h2>
        <p className="max-w-[72ch] text-pretty text-base/7 text-muted-foreground sm:text-sm/6">
          Configure how Temps turns your source into a running application.
        </p>
      </div>

      <Tabs value={active} onValueChange={selectTab} className="space-y-6">
        <TabsList>
          <TabsTrigger value="source">Source</TabsTrigger>
          <TabsTrigger value="build">Build</TabsTrigger>
          <TabsTrigger value="deploy">Deploy</TabsTrigger>
          <TabsTrigger value="previews">Previews</TabsTrigger>
        </TabsList>

        <TabsContent value="source" className="space-y-6">
          <DeploymentSourceCard project={project} refetch={refetch} />
        </TabsContent>

        <TabsContent value="build" className="space-y-6">
          <BuildSettings project={project} refetch={refetch} embedded />
        </TabsContent>

        <TabsContent value="deploy" className="space-y-6">
          <DeployDefaultsCard project={project} refetch={refetch} />
          <EnvironmentPortOverrideCard project={project} />
          <ImageRetentionCard project={project} refetch={refetch} />
        </TabsContent>

        <TabsContent value="previews" className="space-y-6">
          <PreviewEnvironmentsCard project={project} refetch={refetch} />
        </TabsContent>
      </Tabs>
    </div>
  )
}
