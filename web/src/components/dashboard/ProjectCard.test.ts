import { describe, expect, test } from 'bun:test'

import { deploymentLabel } from './ProjectCard'

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
