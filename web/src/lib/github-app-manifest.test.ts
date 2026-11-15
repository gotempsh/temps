// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'

import { installBrowserWindow } from '@/test/dom'
import {
  createManifestSetupTracker,
  GITHUB_APP_WINDOW,
  githubAppWindowName,
  githubManifestAction,
  githubWindowUsable,
  openGithubAppWindow,
  submitGitHubManifest,
} from './github-app-manifest'

installBrowserWindow()

describe('openGithubAppWindow', () => {
  test('opens the named window and cuts its link back to the console', () => {
    const opened: Array<[string, string]> = []
    const githubWindow = { opener: {} as unknown }

    const result = openGithubAppWindow(
      'temps-github-app-abc',
      (url, target) => {
        opened.push([url, target])
        return githubWindow
      }
    )

    expect(result).toBe(githubWindow)
    expect(opened).toEqual([['', 'temps-github-app-abc']])
    expect(githubWindow.opener).toBeNull()
  })

  test('a window already on GitHub is returned, not thrown on', () => {
    // A cross-origin WindowProxy rejects writes to `opener`.
    const crossOrigin = {
      closed: false,
      get opener(): unknown {
        return null
      },
      set opener(_value: unknown) {
        throw new DOMException('Blocked a frame', 'SecurityError')
      },
    }

    expect(openGithubAppWindow('temps-github-app-abc', () => crossOrigin)).toBe(
      crossOrigin
    )
  })

  test('a reused cross-origin window does not break the next attempt', () => {
    const tracker = createManifestSetupTracker<{
      closed: boolean
      opener: unknown
      close(): void
    }>()
    const crossOrigin = {
      closed: false,
      get opener(): unknown {
        return null
      },
      set opener(_value: unknown) {
        throw new DOMException('Blocked a frame', 'SecurityError')
      },
      close() {},
    }

    const first = tracker.begin(() =>
      openGithubAppWindow('temps-github-app-abc', () => crossOrigin)
    )
    tracker.manifestPosted()
    const second = tracker.begin(() =>
      openGithubAppWindow('temps-github-app-abc', () => crossOrigin)
    )

    expect(second.window).toBe(crossOrigin)
    expect(tracker.isCurrent(second.attempt)).toBe(true)
    expect(tracker.isCurrent(first.attempt)).toBe(false)
  })

  test('a blocked popup is passed through as null', () => {
    expect(
      openGithubAppWindow<{ opener: unknown }>(
        'temps-github-app-abc',
        () => null
      )
    ).toBeNull()
  })
})

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

function fakeWindow() {
  const window = {
    closed: false,
    closeCalls: 0,
    close() {
      window.closed = true
      window.closeCalls += 1
    },
  }
  return window
}

describe('createManifestSetupTracker', () => {
  test('an attempt is current until it is cancelled or replaced', () => {
    const setup = createManifestSetupTracker<ReturnType<typeof fakeWindow>>()
    const first = setup.begin(() => fakeWindow())
    expect(setup.isCurrent(first.attempt)).toBe(true)

    setup.cancel()

    expect(setup.isCurrent(first.attempt)).toBe(false)
    expect(setup.isCurrent(setup.begin(() => fakeWindow()).attempt)).toBe(true)
  })

  test('Back while the state is pending: the stale attempt closes its own blank window', () => {
    const setup = createManifestSetupTracker<ReturnType<typeof fakeWindow>>()
    const first = setup.begin(() => fakeWindow())

    setup.cancel()
    setup.closeWindowFor(first.attempt)

    expect(first.window?.closed).toBe(true)
  })

  test('Back and start again: the stale attempt cannot close the window the new one uses', () => {
    const setup = createManifestSetupTracker<ReturnType<typeof fakeWindow>>()
    const first = setup.begin(() => fakeWindow())
    setup.cancel()
    // Back closed the first blank window, so starting again opens a new one.
    const second = setup.begin(() => fakeWindow())
    expect(first.window?.closed).toBe(true)
    expect(second.window?.closed).toBe(false)

    // The first request answers (or fails) after the second attempt began.
    setup.closeWindowFor(first.attempt)

    expect(second.window?.closed).toBe(false)
    expect(setup.isCurrent(first.attempt)).toBe(false)
    expect(setup.isCurrent(second.attempt)).toBe(true)

    // The new attempt owns its window and may close it if it fails.
    setup.closeWindowFor(second.attempt)
    expect(second.window?.closed).toBe(true)
  })

  test('a window shared by name is not closed by the attempt that lost it', () => {
    const setup = createManifestSetupTracker<ReturnType<typeof fakeWindow>>()
    const shared = fakeWindow()
    const first = setup.begin(() => shared)
    // A second attempt starts without a cancel in between and reuses the window.
    const second = setup.begin(() => shared)

    setup.closeWindowFor(first.attempt)

    expect(shared.closed).toBe(false)
    expect(setup.isCurrent(second.attempt)).toBe(true)
  })

  test('a window with a manifest in it is never closed by a later failing attempt', () => {
    const setup = createManifestSetupTracker<ReturnType<typeof fakeWindow>>()
    const shared = fakeWindow()
    const first = setup.begin(() => shared)
    setup.manifestPosted()

    const retry = setup.begin(() => shared)
    setup.closeWindowFor(retry.attempt)
    setup.closeWindowFor(first.attempt)

    expect(shared.closed).toBe(false)
  })

  test('once the operator closed the window, a new attempt may close its fresh one', () => {
    const setup = createManifestSetupTracker<ReturnType<typeof fakeWindow>>()
    const old = fakeWindow()
    setup.begin(() => old)
    setup.manifestPosted()
    old.close()

    const fresh = fakeWindow()
    const retry = setup.begin(() => fresh)
    setup.closeWindowFor(retry.attempt)

    expect(fresh.closed).toBe(true)
  })

  test('cancelling closes the blank window of the attempt in flight', () => {
    const setup = createManifestSetupTracker<ReturnType<typeof fakeWindow>>()
    const first = setup.begin(() => fakeWindow())

    setup.cancel()

    expect(first.window?.closed).toBe(true)
  })

  test('cancelling leaves a window that already holds a manifest open', () => {
    const setup = createManifestSetupTracker<ReturnType<typeof fakeWindow>>()
    const first = setup.begin(() => fakeWindow())
    setup.manifestPosted()

    setup.cancel()

    expect(first.window?.closed).toBe(false)
  })

  test('a blocked popup (null) is handed back as null', () => {
    const setup = createManifestSetupTracker<ReturnType<typeof fakeWindow>>()

    expect(setup.begin(() => null).window).toBeNull()
  })
})

describe('githubWindowUsable', () => {
  test('a blocked popup (null) cannot receive the manifest', () => {
    expect(githubWindowUsable(null)).toBe(false)
  })

  test('a window the operator closed cannot receive the manifest', () => {
    expect(githubWindowUsable({ closed: true })).toBe(false)
  })

  test('an open window can', () => {
    expect(githubWindowUsable({ closed: false })).toBe(true)
  })
})

describe('githubAppWindowName', () => {
  test('each flow instance gets its own window name', () => {
    const first = githubAppWindowName()
    const second = githubAppWindowName()

    expect(first.startsWith(GITHUB_APP_WINDOW)).toBe(true)
    expect(first).not.toBe(second)
  })
})
