// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { CreateSandboxAction } from './ChatSandbox'
import { ChangesOnboarding, TrackChangesPrompt } from './ChatChangesTab'
import { FilesOnboarding } from './ChatFilesTab'

test('without a sandbox the Files tab says what is missing and what it shows', () => {
  const markup = renderToStaticMarkup(
    <FilesOnboarding reason="No AI sandbox runtime is configured." />
  )
  expect(markup).toContain('No files yet')
  expect(markup).toContain('No AI sandbox runtime is configured.')
  expect(markup).toContain('src/app/page.tsx')
})

test('without a sandbox the Changes tab says what is missing and what it shows', () => {
  const markup = renderToStaticMarkup(
    <ChangesOnboarding reason="This chat has no sandbox yet." />
  )
  expect(markup).toContain('No changes to review yet')
  expect(markup).toContain('This chat has no sandbox yet.')
  expect(markup).toContain('diff against the last commit')
})

test('an untracked workspace offers to start tracking it', () => {
  const markup = renderToStaticMarkup(
    <TrackChangesPrompt gitAvailable pending={false} onTrack={() => {}} />
  )
  expect(markup).toContain('Changes are not tracked yet')
  expect(markup).toContain('Start tracking changes')
  expect(markup).not.toContain('disabled=""')
})

test('the tracking button is disabled while it runs', () => {
  const markup = renderToStaticMarkup(
    <TrackChangesPrompt gitAvailable pending onTrack={() => {}} />
  )
  expect(markup).toContain('disabled=""')
})

test('without git the tab explains how to get it instead of offering to track', () => {
  const markup = renderToStaticMarkup(
    <TrackChangesPrompt
      gitAvailable={false}
      pending={false}
      onTrack={() => {}}
    />
  )
  expect(markup).toContain('Git is not installed')
  expect(markup).toContain('apt-get install -y git')
  expect(markup).not.toContain('Start tracking changes')
})

test('a chat without a sandbox offers to create one from the tab itself', () => {
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <CreateSandboxAction conversationPublicId="conversation-1" />
    </QueryClientProvider>
  )
  expect(markup).toContain('Create sandbox')
  expect(markup).not.toContain('disabled=""')
})
