import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { renderToStaticMarkup } from 'react-dom/server'
import { getEnvironmentsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type { DeploymentResponse, ProjectResponse } from '@/api/client'
import { ProjectDetailHeader } from './ProjectDetailHeader'

function renderHeader(status: string, currentId: number | null | undefined) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  if (currentId !== undefined) {
    client.setQueryData(getEnvironmentsQueryKey({ path: { project_id: 1 } }), [
      { id: 1, current_deployment_id: currentId },
    ])
  }
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ProjectDetailHeader
          project={
            { id: 1, slug: 'temps-cloud-api', name: 'Temps' } as ProjectResponse
          }
          lastDeployment={
            { id: 3513, status, is_current: false } as DeploymentResponse
          }
          onDeploy={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return html
}

for (const status of ['running', 'failed', 'completed', 'stopped']) {
  test(`current deployment remains Deployed when latest is ${status}`, () => {
    const html = renderHeader(status, 3500)
    expect(html).toContain('>Deployed<')
    expect(html).not.toContain('Not deployed')
  })
}
test('a completed historical build does not imply a current deployment', () => {
  expect(renderHeader('completed', null)).toContain('Not deployed')
})
test('loading environments does not flash Not deployed', () => {
  const html = renderHeader('running', undefined)
  expect(html).toContain('Checking deployment')
  expect(html).not.toContain('Not deployed')
})
