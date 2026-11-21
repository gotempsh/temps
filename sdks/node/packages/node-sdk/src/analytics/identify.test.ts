// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { analytics, visitorIdFromRequest } from './index';

const config = { apiUrl: 'https://temps.example.test/api/', apiToken: 'dt_test' };

function fetchRequest(cookie?: string): Request {
  return new Request('https://app.example.test/signup', {
    headers: cookie ? { cookie } : {},
  });
}

describe('analytics.identify (ADR 109 D4)', () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ applied: true, outcome: 'updated', message: 'ok' }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
    );
    vi.stubGlobal('fetch', fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  });

  it('reads the visitor cookie and calls the token-scoped route', async () => {
    const result = await analytics.identify(fetchRequest('a=1; _temps_visitor_id=abc-_1; b=2'), {
      ...config,
      userId: 'user-42',
      traits: { plan: 'pro' },
    });

    expect(result).toEqual({ applied: true, outcome: 'updated', message: 'ok' });
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe('https://temps.example.test/api/analytics/visitors/enc_abc-_1/identify');
    expect(init.method).toBe('PUT');
    expect((init.headers as Record<string, string>).Authorization).toBe('Bearer dt_test');
    expect(JSON.parse(init.body as string)).toEqual({ user_id: 'user-42', traits: { plan: 'pro' } });
  });

  it('uses the injected environment by default', async () => {
    vi.stubEnv('TEMPS_API_URL', 'https://temps.example.test/api');
    vi.stubEnv('TEMPS_API_TOKEN', 'dt_env');
    await analytics.identify(
      { headers: { cookie: '_temps_visitor_id=node' } },
      { userId: 'user-42' },
    );
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe('https://temps.example.test/api/analytics/visitors/enc_node/identify');
    expect((init.headers as Record<string, string>).Authorization).toBe('Bearer dt_env');
  });

  it('uses the project route when a project id is given', async () => {
    await analytics.identify(null, { ...config, userId: 'u', visitorId: 'v-1', projectId: 7 });
    expect(fetchMock.mock.calls[0][0]).toBe(
      'https://temps.example.test/api/projects/7/analytics/visitors/v-1/identify',
    );
  });

  it('sends nothing without a visitor, a user id or credentials', async () => {
    expect((await analytics.identify(fetchRequest(), { ...config, userId: 'u' })).outcome).toBe(
      'no_visitor',
    );
    expect(
      (await analytics.identify(fetchRequest('_temps_visitor_id=enc_x'), { ...config, userId: ' ' }))
        .outcome,
    ).toBe('invalid_user_id');
    vi.stubEnv('TEMPS_API_URL', '');
    vi.stubEnv('TEMPS_API_TOKEN', '');
    expect(
      (await analytics.identify(fetchRequest('_temps_visitor_id=enc_x'), { userId: 'u' })).outcome,
    ).toBe('not_configured');
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('reports an HTTP error with the server detail instead of throwing', async () => {
    fetchMock.mockResolvedValue(
      new Response(JSON.stringify({ detail: 'Deployment tokens can only enrich a visitor identified by its encrypted visitor ID' }), {
        status: 403,
        headers: { 'Content-Type': 'application/problem+json' },
      }),
    );
    const result = await analytics.identify(null, { ...config, userId: 'u', visitorId: 'raw-id' });
    expect(result).toMatchObject({ applied: false, outcome: 'http_403' });
    expect(result.message).toContain('encrypted visitor ID');
  });

  it('reports a network failure instead of throwing', async () => {
    fetchMock.mockRejectedValue(new TypeError('fetch failed'));
    const result = await analytics.identify(fetchRequest('_temps_visitor_id=enc_x'), { ...config, userId: 'u' });
    expect(result).toMatchObject({ applied: false, outcome: 'network_error' });
  });
});

describe('visitorIdFromRequest', () => {
  it('finds the cookie among others in both header shapes', () => {
    expect(visitorIdFromRequest(fetchRequest('x=1; _temps_visitor_id=a'))).toBe('enc_a');
    // The proxy stores the sealed value bare, and it can begin with `enc_` itself.
    expect(visitorIdFromRequest(fetchRequest('_temps_visitor_id=enc_q'))).toBe('enc_enc_q');
    expect(visitorIdFromRequest({ headers: { cookie: ['_temps_visitor_id=b'] } })).toBe('enc_b');
    expect(visitorIdFromRequest({ headers: {} })).toBeUndefined();
    expect(visitorIdFromRequest(fetchRequest('_temps_visitor_idx=nope'))).toBeUndefined();
  });
});
