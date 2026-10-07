// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { Navigate, Route, Routes } from 'react-router'
import { CronJobDetail } from './settings/CronJobDetail'
import { LegacyProjectRouteRedirect } from './LegacyProjectRouteRedirect'
import { CombinedProjectSettings } from './settings/CombinedProjectSettings'
import { ProjectAccessSettings } from './settings/ProjectAccessSettings'
import { ProjectSecuritySettings } from './settings/ProjectSecuritySettings'
import { SecretPage } from './settings/SecretPage'
import { TelemetrySettings } from './settings/TelemetrySettings'
import { CreateWebhookPage } from './settings/webhooks/CreateWebhookPage'
import { EditWebhookPage } from './settings/webhooks/EditWebhookPage'
import { WebhookDetail } from './settings/webhooks/WebhookDetail'

interface ProjectSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

export function ProjectSettings({ project, refetch }: ProjectSettingsProps) {
  return (
    <div>
      <Routes>
        {(['delivery', 'variables', 'automation', 'integrations'] as const).map(
          (page) => (
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
          )
        )}
        <Route index element={<Navigate to="general" replace />} />
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
            <ProjectSecuritySettings project={project} refetch={refetch} />
          }
        />
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
          path="deployment-tokens"
          element={
            <LegacyProjectRouteRedirect
              projectSlug={project.slug}
              route="settings/deployment-tokens"
            />
          }
        />
        <Route
          path="telemetry"
          element={<TelemetrySettings project={project} />}
        />
        <Route path="*" element={<Navigate to="." replace />} />
      </Routes>
    </div>
  )
}
