import { ProjectResponse } from '@/api/client'
import { Card } from '@/components/ui/card'
import { Navigate, Route, Routes } from 'react-router-dom'
import { CronJobDetail } from './settings/CronJobDetail'
import { CronJobsSettings } from './settings/CronJobsSettings'
import { DomainsSettings } from './settings/DomainsSettings'
import { EnvironmentsSettings } from './settings/EnvironmentsSettings'
import { EnvironmentVariablesSettings } from './settings/EnvironmentVariablesSettings'
import { GeneralSettings } from './settings/GeneralSettings'
import { GitSettings } from './settings/GitSettings'

interface ProjectSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

export function ProjectSettings({ project, refetch }: ProjectSettingsProps) {
  return (
    <Card className="p-4 sm:p-6">
      <Routes>
        <Route index element={<Navigate to="general" replace />} />
        <Route
          path="general"
          element={<GeneralSettings project={project} refetch={refetch} />}
        />
        <Route path="domains" element={<DomainsSettings project={project} />} />
        <Route
          path="environments/*"
          element={<EnvironmentsSettings project={project} />}
        />
        <Route
          path="environment-variables"
          element={<EnvironmentVariablesSettings project={project} />}
        />
        <Route
          path="git"
          element={<GitSettings project={project} refetch={refetch} />}
        />
        <Route path="cron-jobs">
          <Route index element={<CronJobsSettings project={project} />} />
          <Route
            path=":environmentId/:cronId"
            element={<CronJobDetail project={project} />}
          />
        </Route>
        <Route path="*" element={<Navigate to="general" replace />} />
      </Routes>
    </Card>
  )
}
