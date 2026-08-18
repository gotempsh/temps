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
      })
    ).toEqual({ label: 'gotempsh/temps', provider: 'github' })
  })

  test('derives GitLab repository metadata from its clone URL', () => {
    expect(
      projectRepository({
        git_url: 'git@gitlab.com:team/api.git',
        repo_owner: null,
        repo_name: null,
      })
    ).toEqual({ label: 'team/api', provider: 'gitlab' })
  })

  test('returns no repository when the project is not connected to Git', () => {
    expect(
      projectRepository({
        git_url: null,
        repo_owner: null,
        repo_name: null,
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
      })
    ).toEqual({ kind: 'github', label: 'GitHub' })

    expect(
      projectBuildSource({
        source_type: 'git',
        git_url: 'https://gitlab.com/team/api.git',
        repo_owner: 'team',
        repo_name: 'api',
      })
    ).toEqual({ kind: 'gitlab', label: 'GitLab' })
  })

  test('distinguishes images from uploaded source', () => {
    expect(
      projectBuildSource({
        source_type: 'docker_image',
        git_url: null,
        repo_owner: null,
        repo_name: null,
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
        })
      ).toEqual({ kind: 'source', label: 'Source upload' })
    }
  })
})
