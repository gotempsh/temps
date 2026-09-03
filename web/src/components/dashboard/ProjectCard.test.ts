// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  deploymentLabel,
  projectBuildSource,
  projectPresetLabel,
  projectRepository,
} from './project-card-data'

describe('deploymentLabel', () => {
  test('calls only completed runs deployed', () => {
    expect(deploymentLabel('completed')).toBe('Deployed')
    expect(deploymentLabel('deployed')).toBe('Deployed')
    expect(deploymentLabel('failed')).toBe('Last attempt')
    expect(deploymentLabel('cancelled')).toBe('Last attempt')
    expect(deploymentLabel(undefined)).toBe('Last attempt')
  })

  test('describes in-flight runs as started', () => {
    expect(deploymentLabel('pending')).toBe('Deploying, started')
    expect(deploymentLabel('running')).toBe('Deploying, started')
  })
})

describe('projectPresetLabel', () => {
  test('formats known and custom presets for display', () => {
    expect(projectPresetLabel('nextjs')).toBe('Next.js')
    expect(projectPresetLabel('docker-compose')).toBe('Docker Compose')
    expect(projectPresetLabel('rust-cargo')).toBe('Rust Cargo')
  })

  test('makes missing configuration explicit', () => {
    expect(projectPresetLabel(null)).toBe('Not configured')
    expect(projectPresetLabel(undefined)).toBe('Not configured')
  })
})

describe('projectRepository', () => {
  test('uses configured repository metadata', () => {
    expect(
      projectRepository({
        git_url: 'https://github.com/gotempsh/temps.git',
        repo_owner: 'gotempsh',
        repo_name: 'temps',
        git_provider_type: 'github',
      })
    ).toEqual({ label: 'gotempsh/temps', provider: 'github' })
  })

  test('derives GitLab repository metadata from its clone URL', () => {
    expect(
      projectRepository({
        git_url: 'git@gitlab.com:team/api.git',
        repo_owner: null,
        repo_name: null,
        git_provider_type: null,
      })
    ).toEqual({ label: 'team/api', provider: 'gitlab' })
  })

  test('trusts the connected provider over the clone URL', () => {
    // Self-hosted instance on a hostname that names no vendor: the connection
    // knows what it is, the URL never will.
    expect(
      projectRepository({
        git_url: 'https://code.example.internal/team/api.git',
        repo_owner: 'team',
        repo_name: 'api',
        git_provider_type: 'gitlab',
      })
    ).toEqual({ label: 'team/api', provider: 'gitlab' })

    expect(
      projectRepository({
        git_url: null,
        repo_owner: 'team',
        repo_name: 'api',
        git_provider_type: 'gitea',
      })
    ).toEqual({ label: 'team/api', provider: 'gitea' })
  })

  test('never guesses GitHub for a project with no clone URL', () => {
    expect(
      projectRepository({
        git_url: null,
        repo_owner: 'team',
        repo_name: 'api',
        git_provider_type: null,
      })
    ).toEqual({ label: 'team/api', provider: 'git' })
  })

  test('returns no repository when the project is not connected to Git', () => {
    expect(
      projectRepository({
        git_url: null,
        repo_owner: null,
        repo_name: null,
        git_provider_type: null,
      })
    ).toBeNull()
  })
})

describe('projectBuildSource', () => {
  test('identifies GitHub and GitLab repository builds', () => {
    expect(
      projectBuildSource({
        source_type: 'git',
        git_url: 'https://github.com/gotempsh/temps.git',
        repo_owner: 'gotempsh',
        repo_name: 'temps',
        git_provider_type: 'github',
      })
    ).toEqual({ kind: 'github', label: 'GitHub' })

    expect(
      projectBuildSource({
        source_type: 'git',
        git_url: 'https://gitlab.com/team/api.git',
        repo_owner: 'team',
        repo_name: 'api',
        git_provider_type: 'gitlab',
      })
    ).toEqual({ kind: 'gitlab', label: 'GitLab' })
  })

  test('reports the connected provider, whatever the clone URL looks like', () => {
    // The regression this guards: a GitLab-connected project with no stored
    // clone URL used to be labelled — and badged — as GitHub.
    expect(
      projectBuildSource({
        source_type: 'git',
        git_url: null,
        repo_owner: 'team',
        repo_name: 'api',
        git_provider_type: 'gitlab',
      })
    ).toEqual({ kind: 'gitlab', label: 'GitLab' })

    expect(
      projectBuildSource({
        source_type: 'git',
        git_url: 'https://code.example.internal/team/api.git',
        repo_owner: 'team',
        repo_name: 'api',
        git_provider_type: 'gitea',
      })
    ).toEqual({ kind: 'gitea', label: 'Gitea' })

    // The GitHub App flavour is still GitHub.
    expect(
      projectBuildSource({
        source_type: 'git',
        git_url: null,
        repo_owner: 'team',
        repo_name: 'api',
        git_provider_type: 'github_app',
      })
    ).toEqual({ kind: 'github', label: 'GitHub' })

    // Self-hosted plain Git has no brand of its own.
    expect(
      projectBuildSource({
        source_type: 'git',
        git_url: 'https://code.example.internal/team/api.git',
        repo_owner: 'team',
        repo_name: 'api',
        git_provider_type: 'generic',
      })
    ).toEqual({ kind: 'git', label: 'Git repository' })
  })

  test('stays generic when nothing identifies the provider', () => {
    expect(
      projectBuildSource({
        source_type: 'git',
        git_url: null,
        repo_owner: 'team',
        repo_name: 'api',
        git_provider_type: null,
      })
    ).toEqual({ kind: 'git', label: 'Git repository' })
  })

  test('distinguishes images from uploaded source', () => {
    expect(
      projectBuildSource({
        source_type: 'docker_image',
        git_url: null,
        repo_owner: null,
        repo_name: null,
        git_provider_type: null,
      })
    ).toEqual({ kind: 'docker', label: 'Docker image' })

    for (const source_type of [
      'static_files',
      'uploaded_source',
      'manual',
    ] as const) {
      expect(
        projectBuildSource({
          source_type,
          git_url: null,
          repo_owner: null,
          repo_name: null,
          git_provider_type: null,
        })
      ).toEqual({ kind: 'source', label: 'Source upload' })
    }
  })
})
