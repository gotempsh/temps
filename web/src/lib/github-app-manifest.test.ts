// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'

import { installBrowserWindow } from '@/test/dom'
import {
  githubManifestAction,
  submitGitHubManifest,
} from './github-app-manifest'

installBrowserWindow()

describe('githubManifestAction', () => {
  test('carries the server-issued state to GitHub', () => {
    expect(githubManifestAction('0b3f4c9e-1d6a-4f5e-9a41-6f1c2a7d8e90')).toBe(
      'https://github.com/settings/apps/new?state=0b3f4c9e-1d6a-4f5e-9a41-6f1c2a7d8e90'
    )
  })

  test('encodes a state so it cannot change the URL', () => {
    const action = githubManifestAction('a&b=c#d')
    expect(action).toBe(
      'https://github.com/settings/apps/new?state=a%26b%3Dc%23d'
    )
    expect(new URL(action).searchParams.get('state')).toBe('a&b=c#d')
  })
})

describe('submitGitHubManifest', () => {
  let submitted: HTMLFormElement[] = []
  const realSubmit = HTMLFormElement.prototype.submit

  beforeEach(() => {
    submitted = []
    HTMLFormElement.prototype.submit = function (this: HTMLFormElement) {
      // Capture what would be sent; the form is still attached at this point.
      submitted.push(this.cloneNode(true) as HTMLFormElement)
      expect(document.body.contains(this)).toBe(true)
    }
  })

  afterEach(() => {
    HTMLFormElement.prototype.submit = realSubmit
  })

  test('posts the manifest with the given state into the named window', () => {
    const manifest = { name: 'temps-example', public: true }

    submitGitHubManifest(manifest, 'issued-state', 'temps-github-app')

    expect(submitted).toHaveLength(1)
    const form = submitted[0]
    expect(form.method.toLowerCase()).toBe('post')
    expect(form.action).toBe(
      'https://github.com/settings/apps/new?state=issued-state'
    )
    expect(form.target).toBe('temps-github-app')
    const field = form.querySelector(
      'input[name="manifest"]'
    ) as HTMLInputElement
    expect(JSON.parse(field.value)).toEqual(manifest)
  })

  test('leaves no form behind, even when the submit fails', () => {
    HTMLFormElement.prototype.submit = () => {
      throw new Error('blocked')
    }

    expect(() =>
      submitGitHubManifest({}, 'issued-state', 'temps-github-app')
    ).toThrow('blocked')
    expect(document.querySelectorAll('form')).toHaveLength(0)
  })
})
