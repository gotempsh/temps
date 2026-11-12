// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client/types.gen'
import { Routes, Route, useParams } from 'react-router'
import {
  useListReturnNavigation,
  type ListReturnState,
} from '@/components/table/list-navigation'
import { VisitorsList } from './VisitorsList'
import { VisitorDetail } from './VisitorDetail'
import { SessionDetail } from './SessionDetail'
import { SessionReplayDetail } from '@/pages/SessionReplayDetail'

interface VisitorAnalyticsProps {
  project: ProjectResponse
}

// Wrapper components to inject params
function VisitorDetailWrapper({
  project,
  listState,
}: {
  project: ProjectResponse
  listState: ListReturnState | null
}) {
  const { visitorId } = useParams<{ visitorId: string }>()
  if (!visitorId) return null
  return (
    <VisitorDetail
      project={project}
      visitorId={Number(visitorId)}
      listState={listState}
    />
  )
}

function SessionDetailWrapper({ project }: { project: ProjectResponse }) {
  const { visitorId, sessionId } = useParams<{
    visitorId: string
    sessionId: string
  }>()
  if (!visitorId || !sessionId) return null
  return (
    <SessionDetail
      project={project}
      visitorId={Number(visitorId)}
      sessionId={Number(sessionId)}
    />
  )
}

export default function VisitorAnalytics({ project }: VisitorAnalyticsProps) {
  const { listState, setListState, containerRef } = useListReturnNavigation(
    `/projects/${project.slug}/analytics/visitors`
  )
  return (
    <div ref={containerRef} className="w-full min-w-0">
      <Routes>
        <Route
          index
          element={
            <VisitorsList
              project={project}
              listState={listState}
              onRememberList={setListState}
            />
          }
        />
        <Route
          path=":visitorId"
          element={
            <VisitorDetailWrapper project={project} listState={listState} />
          }
        />
        <Route
          path=":visitorId/sessions/:sessionId"
          element={<SessionDetailWrapper project={project} />}
        />
        <Route
          path=":visitorId/session-replay/:sessionId"
          element={<SessionReplayDetail project={project} />}
        />
      </Routes>
    </div>
  )
}
