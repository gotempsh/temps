// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, it, vi } from 'vitest';
import { FlagsClient } from './client';
import type { FlagSnapshotResponse } from './types';

/**
 * The refresh interval is the one setting an operator is likely to reach for
 * without touching app code — via `TEMPS_FLAGS_REFRESH_INTERVAL_MS` on the
 * deployment, not a `FlagsClient` constructor argument. These tests cover the
 * env var path specifically: the constructor option path is exercised
 * incidentally by every other test in this package that passes
 * `refreshIntervalMs` directly.
 */

const SNAPSHOT: FlagSnapshotResponse = {
  environment_id: 1,
  flags: [{ key: 'checkout.v2', value_type: 'bool', default_value: false, enabled: true }],
};

function mockFetch() {
  const fetchMock = vi.fn(async () => new Response(JSON.stringify(SNAPSHOT), { status: 200 }));
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('refresh interval from TEMPS_FLAGS_REFRESH_INTERVAL_MS', () => {
  it('polls on the env-provided interval instead of the 30s default', async () => {
    vi.useFakeTimers();
    vi.stubEnv('TEMPS_FLAGS_REFRESH_INTERVAL_MS', '5000');
    const fetchMock = mockFetch();

    const client = new FlagsClient({ apiUrl: 'http://temps.test/api', apiToken: 'dt_test' });
    await client.init();
    expect(fetchMock).toHaveBeenCalledTimes(1); // initial load

    await vi.advanceTimersByTimeAsync(5000);
    expect(fetchMock).toHaveBeenCalledTimes(2); // fired at 5s, not 30s

    client.close();
  });

  it('an explicit refreshIntervalMs option wins over the env var', async () => {
    vi.useFakeTimers();
    vi.stubEnv('TEMPS_FLAGS_REFRESH_INTERVAL_MS', '5000');
    const fetchMock = mockFetch();

    const client = new FlagsClient({
      apiUrl: 'http://temps.test/api',
      apiToken: 'dt_test',
      refreshIntervalMs: 1000,
    });
    await client.init();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchMock).toHaveBeenCalledTimes(2); // the option's 1s, not the env's 5s

    client.close();
  });

  it('falls back to the 30s default and warns when the env value is not a valid number', async () => {
    vi.useFakeTimers();
    vi.stubEnv('TEMPS_FLAGS_REFRESH_INTERVAL_MS', 'not-a-number');
    mockFetch();
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});

    const client = new FlagsClient({ apiUrl: 'http://temps.test/api', apiToken: 'dt_test' });
    await client.init();

    expect(warn).toHaveBeenCalledWith(expect.stringContaining('TEMPS_FLAGS_REFRESH_INTERVAL_MS'));

    client.close();
  });

  it('falls back to the 30s default and warns when the env value is negative', async () => {
    vi.useFakeTimers();
    vi.stubEnv('TEMPS_FLAGS_REFRESH_INTERVAL_MS', '-1');
    mockFetch();
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});

    const client = new FlagsClient({ apiUrl: 'http://temps.test/api', apiToken: 'dt_test' });
    await client.init();

    expect(warn).toHaveBeenCalledWith(expect.stringContaining('TEMPS_FLAGS_REFRESH_INTERVAL_MS'));

    client.close();
  });

  it('an env value of 0 disables background polling, same as the option would', async () => {
    vi.useFakeTimers();
    vi.stubEnv('TEMPS_FLAGS_REFRESH_INTERVAL_MS', '0');
    const fetchMock = mockFetch();

    const client = new FlagsClient({ apiUrl: 'http://temps.test/api', apiToken: 'dt_test' });
    await client.init();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(60_000);
    expect(fetchMock).toHaveBeenCalledTimes(1); // no timer ever started

    client.close();
  });
});
