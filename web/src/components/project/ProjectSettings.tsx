// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { RedirectWithQuery } from '@/components/routing/RedirectWithQuery'
import { ProjectResponse } from '@/api/client'
import { Route, Routes } from 'react-router'
import { CronJobDetail } from './settings/CronJobDetail'
import { EdgeRuleEditor } from './settings/edge-rules/EdgeRuleEditor'
import { EdgeRulesPage } from './settings/edge-rules/EdgeRulesPage'
import { LegacyProjectRouteRedirect } from './LegacyProjectRouteRedirect'
import { CombinedProjectSettings } from './settings/CombinedProjectSettings'
import { ProjectAccessSettings } from './settings/ProjectAccessSettings'
import { ProjectIpAccessCard } from './settings/ProjectIpAccessCard'
import { ProjectSecuritySettings } from './settings/ProjectSecuritySettings'
import { SecretPage } from './settings/SecretPage'
import { TelemetrySettings } from './settings/TelemetrySettings'
import { CreateWebhookPage } from './settings/webhooks/CreateWebhookPage'
import { EditWebhookPage } from './settings/webhooks/EditWebhookPage'
import { WebhookDetail } from './settings/webhooks/WebhookDetail'
import { PageHeader } from '@temps-sdk/ds'

interface ProjectSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

export function ProjectSettings({ project, refetch }: ProjectSettingsProps) {
  return (
    <div>
      <Routes>
        {(
          [
            'delivery',
            'variables',
            'deployment-tokens',
            'automation',
            'integrations',
          ] as const
        ).map((page) => (
          <Route
            key={page}
            path={page}
            element={
              <CombinedProjectSettings
                page={page}
                project={project}
                refetch={refetch}
              />
            }
          />
        ))}
        <Route index element={<RedirectWithQuery to="general" />} />
        <Route
          path="general"
          element={
            <CombinedProjectSettings
              page="general"
              project={project}
              refetch={refetch}
            />
          }
        />
        <Route
          path="domains"
          element={
            <LegacyProjectRouteRedirect
              projectSlug={project.slug}
              route="settings/domains"
            />
          }
        />
        <Route
          path="environment-variables"
          element={
            <LegacyProjectRouteRedirect
              projectSlug={project.slug}
              route="settings/environment-variables"
            />
          }
        />
        <Route
          path="secrets"
          element={
            <LegacyProjectRouteRedirect
              projectSlug={project.slug}
              route="settings/secrets"
            />
          }
        />
        <Route
          path="secrets/:secretId"
          element={<SecretPage project={project} />}
        />
        <Route
          path="secrets/:secretId/checks"
          element={<SecretPage project={project} configure />}
        />
        <Route
          path="git"
          element={
            <LegacyProjectRouteRedirect
              projectSlug={project.slug}
              route="settings/git"
            />
          }
        />
        <Route
          path="build"
          element={
            <LegacyProjectRouteRedirect
              projectSlug={project.slug}
              route="settings/build"
            />
          }
        />
        <Route
          path="security"
          element={
            <div className="space-y-6">
              <PageHeader
                title="Protection"
                description="Shield, security headers, rate limits and IP access for this service. Empty fields inherit the instance-wide settings."
              />
              <ProjectSecuritySettings project={project} refetch={refetch} />
              <ProjectIpAccessCard
                projectId={project.id}
                scopeName={project.name}
              />
            </div>
          }
        />
        <Route path="edge-rules">
          <Route index element={<EdgeRulesPage project={project} />} />
          <Route path="new" element={<EdgeRuleEditor project={project} />} />
          <Route
            path=":ruleId"
            element={<EdgeRuleEditor project={project} />}
          />
        </Route>
        <Route
          path="access"
          element={<ProjectAccessSettings project={project} />}
        />
        <Route path="cron-jobs">
          <Route
            index
            element={
              <LegacyProjectRouteRedirect
                projectSlug={project.slug}
                route="settings/cron-jobs"
              />
            }
          />
          <Route
            path=":environmentId/:cronId"
            element={<CronJobDetail project={project} />}
          />
        </Route>
        <Route path="webhooks">
          <Route
            index
            element={
              <LegacyProjectRouteRedirect
                projectSlug={project.slug}
                route="settings/webhooks"
              />
            }
          />
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
        <Route
          path="skills"
          element={
            <LegacyProjectRouteRedirect
              projectSlug={project.slug}
              route="settings/skills"
            />
          }
        />
        <Route
          path="mcp-servers"
          element={
            <LegacyProjectRouteRedirect
              projectSlug={project.slug}
              route="settings/mcp-servers"
            />
          }
        />
        <Route
          path="telemetry"
          element={<TelemetrySettings project={project} />}
        />
        <Route path="*" element={<RedirectWithQuery to="." />} />
      </Routes>
    </div>
  )
}
