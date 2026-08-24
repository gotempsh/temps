import { ProjectResponse } from '@/api/client'
import { Navigate, Route, Routes } from 'react-router'
import { CronJobDetail } from './settings/CronJobDetail'
import { CronJobsSettings } from './settings/CronJobsSettings'
import { DomainsSettings } from './settings/DomainsSettings'
import { EnvironmentVariablesSettings } from './settings/EnvironmentVariablesSettings'
import { GeneralSettings } from './settings/GeneralSettings'
import { GitSettings } from './settings/GitSettings'
import { BuildDeploySettings } from './settings/BuildDeploySettings'
import { ProjectAccessSettings } from './settings/ProjectAccessSettings'
import { ProjectSecuritySettings } from './settings/ProjectSecuritySettings'
import { McpServersSettings } from './settings/McpServersSettings'
import { SecretsSettings } from './settings/SecretsSettings'
import { SkillsSettings } from './settings/SkillsSettings'
import { WebhooksSettings } from './settings/WebhooksSettings'
import { CreateWebhookPage } from './settings/webhooks/CreateWebhookPage'
import { EditWebhookPage } from './settings/webhooks/EditWebhookPage'
import { WebhookDetail } from './settings/webhooks/WebhookDetail'
import { ProjectSettingsOverview } from './settings/ProjectSettingsOverview'
import { DeploymentTokensSettings } from './settings/DeploymentTokensSettings'

interface ProjectSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

export function ProjectSettings({ project, refetch }: ProjectSettingsProps) {
  return (
    <div>
      <Routes>
        <Route index element={<ProjectSettingsOverview project={project} />} />
        <Route
          path="general"
          element={<GeneralSettings project={project} refetch={refetch} />}
        />
        <Route path="domains" element={<DomainsSettings project={project} />} />
        <Route
          path="environment-variables"
          element={<EnvironmentVariablesSettings project={project} />}
        />
        <Route path="secrets" element={<SecretsSettings project={project} />} />
        <Route
          path="git"
          element={<GitSettings project={project} refetch={refetch} />}
        />
        <Route
          path="build"
          element={<BuildDeploySettings project={project} refetch={refetch} />}
        />
        <Route
          path="security"
          element={
            <ProjectSecuritySettings project={project} refetch={refetch} />
          }
        />
        <Route
          path="access"
          element={<ProjectAccessSettings project={project} />}
        />
        <Route path="cron-jobs">
          <Route index element={<CronJobsSettings project={project} />} />
          <Route
            path=":environmentId/:cronId"
            element={<CronJobDetail project={project} />}
          />
        </Route>
        <Route path="webhooks">
          <Route index element={<WebhooksSettings project={project} />} />
          <Route path="new" element={<CreateWebhookPage project={project} />} />
          <Route
            path=":webhookId/edit"
            element={<EditWebhookPage project={project} />}
          />
        </Route>
        <Route
          path="webhooks/:webhookId"
          element={<WebhookDetail project={project} />}
        />
        <Route path="skills" element={<SkillsSettings project={project} />} />
        <Route
          path="mcp-servers"
          element={<McpServersSettings project={project} />}
        />
        <Route
          path="deployment-tokens"
          element={<DeploymentTokensSettings project={project} />}
        />
        <Route path="*" element={<Navigate to="." replace />} />
      </Routes>
    </div>
  )
}
