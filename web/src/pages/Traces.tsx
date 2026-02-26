import { Routes, Route } from 'react-router-dom'
import { ProjectResponse } from '@/api/client'
import TracesList from './TracesList'
import TraceDetail from './TraceDetail'

interface TracesProps {
  project: ProjectResponse
}

export default function Traces({ project }: TracesProps) {
  return (
    <Routes>
      <Route index element={<TracesList project={project} />} />
      <Route path=":traceId" element={<TraceDetail project={project} />} />
    </Routes>
  )
}
