// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { Route, Routes } from 'react-router'
import { RedirectWithSearch } from '@/components/layout/RedirectWithSearch'
import { KvService } from './services/KvService'
import { BlobService } from './services/BlobService'

interface ProjectServicesProps {
  project: ProjectResponse
}

export function ProjectServices({ project }: ProjectServicesProps) {
  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-4 space-y-6 md:p-6">
        <Routes>
          <Route
            index
            element={
              <RedirectWithSearch to={`/projects/${project.slug}/storage`} />
            }
          />
          <Route path="kv" element={<KvService project={project} />} />
          <Route path="blob" element={<BlobService project={project} />} />
          <Route
            path="*"
            element={
              <RedirectWithSearch to={`/projects/${project.slug}/storage`} />
            }
          />
        </Routes>
      </div>
    </div>
  )
}
