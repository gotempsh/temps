import { describe, expect, test } from 'bun:test'
import { projectSettingsSections } from './project-settings-view'

describe('project settings separation', () => {
  test('Git settings contain only repository and automation concerns', () => {
    expect(projectSettingsSections('git', 'git')).toEqual({
      showRepository: true,
      showUploadedSource: false,
      showBuildConfiguration: false,
      showGitAutomation: true,
    })
  })

  test('Build settings are independent of a Git connection', () => {
    expect(projectSettingsSections('build', 'uploaded_source')).toEqual({
      showRepository: false,
      showUploadedSource: false,
      showBuildConfiguration: true,
      showGitAutomation: false,
    })
  })

  test('Git settings explain an uploaded source instead of showing build fields', () => {
    expect(projectSettingsSections('git', 'uploaded_source')).toEqual({
      showRepository: false,
      showUploadedSource: true,
      showBuildConfiguration: false,
      showGitAutomation: false,
    })
  })
})
