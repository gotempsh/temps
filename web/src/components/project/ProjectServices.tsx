import { ProjectResponse } from '@/api/client'
import { Navigate, Route, Routes } from 'react-router-dom'
import { ServicesOverview } from './services/ServicesOverview'
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
          <Route index element={<ServicesOverview project={project} />} />
          <Route path="kv" element={<KvService project={project} />} />
          <Route path="blob" element={<BlobService project={project} />} />
          <Route path="*" element={<Navigate to="" replace />} />
        </Routes>
      </div>
    </div>
  )
}
