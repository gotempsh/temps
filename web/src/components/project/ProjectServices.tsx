// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { Route, Routes } from 'react-router'
import { RedirectWithQuery } from '@/components/routing/RedirectWithQuery'
import { KvService } from './services/KvService'
import { BlobService } from './services/BlobService'

interface ProjectServicesProps {
  project: ProjectResponse
}

export function ProjectServices({ project }: ProjectServicesProps) {
  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <Routes>
          <Route
            index
            element={
              <RedirectWithQuery to={`/projects/${project.slug}/storage`} />
            }
          />
          <Route path="kv" element={<KvService project={project} />} />
          <Route path="blob" element={<BlobService project={project} />} />
          <Route
            path="*"
            element={
              <RedirectWithQuery to={`/projects/${project.slug}/storage`} />
            }
          />
        </Routes>
      </div>
    </div>
  )
}
