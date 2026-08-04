import { describe, expect, it, vi } from 'vitest';
import { FlagsClient } from './client';
import type { FlagSnapshotResponse } from './types';

/**
 * Exposure reporting is what gives "last evaluated" a real meaning, but it is
 * also the one thing in this client that writes back to the control plane —
 * so its bounds matter more than its happy path.
 */

const SNAPSHOT: FlagSnapshotResponse = {
  environment_id: 1,
  flags: [
    { key: 'checkout.v2', value_type: 'bool', default_value: false, enabled: true },
    { key: 'api.rate_limit', value_type: 'number', default_value: 100, enabled: true },
  ],
};

function mockFetch() {
  const calls: { url: string; body?: unknown }[] = [];
  const fetchMock = vi.fn(async (url: URL | string, init?: RequestInit) => {
    const href = typeof url === 'string' ? url : url.toString();
    calls.push({
      url: href,
      body: init?.body ? JSON.parse(init.body as string) : undefined,
    });
    if (href.includes('/flags/exposure')) {
      return new Response(JSON.stringify({ recorded: 1 }), { status: 200 });
    }
    return new Response(JSON.stringify(SNAPSHOT), { status: 200 });
  });
  vi.stubGlobal('fetch', fetchMock);
  return calls;
}

async function loadedClient(overrides = {}) {
  const client = new FlagsClient({
    apiUrl: 'http://temps.test/api',
    apiToken: 'dt_test',
    refreshIntervalMs: 0,
    ...overrides,
  });
  await client.init();
  return client;
}

describe('exposure reporting', () => {
  it('reports only the keys that were actually evaluated', async () => {
    const calls = mockFetch();
    const client = await loadedClient();

    client.get('checkout.v2', {}, false);
    await client.flushExposure();

    const exposure = calls.find((c) => c.url.includes('/flags/exposure'));
    expect(exposure?.body).toEqual({ keys: ['checkout.v2'] });
  });

  it('does not report a flag that was never read', async () => {
    const calls = mockFetch();
    const client = await loadedClient();

    client.get('checkout.v2', {}, false);
    await client.flushExposure();

    const keys = (calls.find((c) => c.url.includes('/flags/exposure'))?.body as { keys: string[] })
      .keys;
    expect(keys).not.toContain('api.rate_limit');
  });

  // The bound that matters: without this, get('user-' + id) would grow the set
  // without limit and ship unbounded garbage to the control plane.
  it('ignores keys that are not in the snapshot', async () => {
    const calls = mockFetch();
    const client = await loadedClient();

    for (let i = 0; i < 1000; i++) {
      client.get(`dynamic-key-${i}`, {}, false);
    }
    await client.flushExposure();

    expect(calls.some((c) => c.url.includes('/flags/exposure'))).toBe(false);
  });

  it('sends nothing when no flag was evaluated', async () => {
    const calls = mockFetch();
    const client = await loadedClient();

    await client.flushExposure();

    expect(calls.some((c) => c.url.includes('/flags/exposure'))).toBe(false);
  });

  it('does not re-report the same key twice', async () => {
    const calls = mockFetch();
    const client = await loadedClient();

    client.get('checkout.v2', {}, false);
    await client.flushExposure();
    await client.flushExposure();

    const reports = calls.filter((c) => c.url.includes('/flags/exposure'));
    expect(reports).toHaveLength(1);
  });

  it('can be turned off', async () => {
    const calls = mockFetch();
    const client = await loadedClient({ reportExposure: false });

    client.get('checkout.v2', {}, false);
    await client.flushExposure();

    expect(calls.some((c) => c.url.includes('/flags/exposure'))).toBe(false);
  });

  // A usage report failing must never surface as a flag problem.
  it('stays silent and leaves lastError alone when reporting fails', async () => {
    mockFetch();
    const client = await loadedClient();
    client.get('checkout.v2', {}, false);

    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('network down');
      })
    );

    await expect(client.flushExposure()).resolves.toBeUndefined();
    expect(client.lastError).toBeNull();
  });
});
