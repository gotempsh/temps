// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  confidenceTone,
  decisionsFor,
  confidenceMeaning,
  CONFIDENCE_LEVELS,
  MAX_CONFIDENCE,
  signalLevel,
  deliveryLabel,
  incidentPhase,
  incidentStatus,
  isActive,
} from './autofix-incident'

describe('autofix incident view helpers', () => {
  it('labels every status and falls back to the raw value', () => {
    expect(incidentStatus('awaiting_review')).toEqual({
      label: 'Waiting for review',
      tone: 'review',
    })
    expect(incidentStatus('failed').tone).toBe('error')
    expect(incidentStatus('something_new')).toEqual({
      label: 'something_new',
      tone: 'muted',
    })
  })

  it('keeps polling only while queued or working', () => {
    expect(isActive('open')).toBe(true)
    expect(isActive('working')).toBe(true)
    expect(isActive('awaiting_review')).toBe(false)
    expect(isActive(undefined)).toBe(false)
  })

  it('offers the decision that matches the deliverable', () => {
    expect(decisionsFor('pull_request').map((d) => d.decision)).toEqual([
      'merged',
      'rejected',
    ])
    expect(decisionsFor('patch').map((d) => d.decision)).toEqual([
      'applied',
      'rejected',
    ])
    expect(decisionsFor(null).map((d) => d.decision)).toEqual([
      'applied',
      'rejected',
    ])
  })

  it('says what a fix becomes', () => {
    expect(deliveryLabel('pull_request')).toContain('pull request')
    expect(deliveryLabel('patch')).toContain('patch')
    expect(deliveryLabel('report')).toContain('analysis')
  })

  it('bands confidence out of 5 at 4 and 2', () => {
    expect(confidenceTone(5)).toBe('high')
    expect(confidenceTone(4)).toBe('high')
    expect(confidenceTone(3)).toBe('medium')
    expect(confidenceTone(2)).toBe('medium')
    expect(confidenceTone(1)).toBe('low')
    expect(confidenceTone(0)).toBe('low')
  })

  it('describes every level, and 0 as nothing verified', () => {
    expect(CONFIDENCE_LEVELS.map((entry) => entry.level)).toEqual([
      1, 2, 3, 4, 5,
    ])
    expect(CONFIDENCE_LEVELS).toHaveLength(MAX_CONFIDENCE)
    expect(confidenceMeaning(4)).toContain('test command')
    expect(confidenceMeaning(0)).toBe('Nothing was verified')
  })

  it('labels a check by its level, and a dash when it never scores', () => {
    expect(signalLevel({ level: 3 })).toBe('L3')
    expect(signalLevel({ level: null })).toBe('–')
    expect(signalLevel({})).toBe('–')
  })

  it('explains phases and passes unknown ones through', () => {
    expect(incidentPhase('verify')).toContain('reproduction')
    expect(incidentPhase('later')).toBe('later')
  })
})
