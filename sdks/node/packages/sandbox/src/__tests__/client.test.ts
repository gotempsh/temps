// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it, vi } from 'vitest';
import { Sandbox } from '../client.js';
import { SandboxError } from '../errors.js';

// These tests only cover shape-translation and transport plumbing.
// End-to-end behaviour (container lifecycle, exec, fs) lives in the
// Rust integration suite — no reason to duplicate it in JS.

function mockFetch(handler: (url: string, init?: RequestInit) => Response) {
  return vi.fn(async (url: string | URL, init?: RequestInit) =>
    handler(url.toString(), init)
  ) as unknown as typeof fetch;
}

const baseCfg = {
  apiUrl: 'https://api.temps.test',
  apiToken: 'temps_pat_fake',
};

describe('Sandbox.create', () => {
  it('posts camelCase → snake_case body and returns a live handle', async () => {
    let capturedBody: unknown;
    const fetch = mockFetch((url, init) => {
      expect(url).toBe('https://api.temps.test/v1/sandbox');
      expect(init?.method).toBe('POST');
      capturedBody = JSON.parse(init?.body as string);
      return new Response(
        JSON.stringify({
          id: 'sbx_abc',
          name: 'my-sbx',
          status: 'running',
          image: null,
          work_dir: '/workspace',
          created_at: '2026-04-19T00:00:00Z',
          expires_at: '2026-04-19T02:00:00Z',
          preview_url_template: 'https://sbx-abc-{port}.preview.example.com',
        }),
        { status: 201, headers: { 'Content-Type': 'application/json' } }
      );
    });

    const sbx = await Sandbox.create({
      ...baseCfg,
      fetch,
      name: 'my-sbx',
      timeoutSecs: 7200,
      source: {
        type: 'git',
        url: 'https://github.com/example/repo.git',
        gitConnectionId: 42,
      },
    });

    expect(sbx.id).toBe('sbx_abc');
    expect(sbx.info.previewUrlTemplate).toContain('{port}');
    expect(sbx.domain(3000)).toBe('https://sbx-abc-3000.preview.example.com');
    expect(capturedBody).toMatchObject({
      name: 'my-sbx',
      timeout_secs: 7200,
      source: {
        type: 'git',
        url: 'https://github.com/example/repo.git',
        git_connection_id: 42,
      },
    });
  });

  it('throws a SandboxError with the RFC 7807 detail', async () => {
    const fetch = mockFetch(
      () =>
        new Response(
          JSON.stringify({
            title: 'Validation Error',
            detail: 'git source: url must not contain embedded credentials',
            status: 400,
          }),
          {
            status: 400,
            headers: { 'Content-Type': 'application/problem+json' },
          }
        )
    );

    await expect(
      Sandbox.create({
        ...baseCfg,
        fetch,
        source: { type: 'git', url: 'https://u:p@host/r.git' },
      })
    ).rejects.toMatchObject({
      name: 'SandboxError',
      status: 400,
      detail: expect.stringContaining('embedded credentials'),
    });
  });

  it('requires apiUrl and apiToken', async () => {
    await expect(Sandbox.create({})).rejects.toBeInstanceOf(SandboxError);
  });
});

describe('Sandbox.exec', () => {
  it('unwraps the exec response', async () => {
    const fetch = mockFetch((url) => {
      if (url.endsWith('/v1/sandbox')) {
        return new Response(
          JSON.stringify({
            id: 'sbx_abc',
            name: '',
            status: 'running',
            image: null,
            work_dir: '/workspace',
            created_at: '',
            expires_at: '',
            preview_url_template: '',
          }),
          { status: 201 }
        );
      }
      if (url.endsWith('/v1/sandbox/sbx_abc/exec')) {
        return new Response(
          JSON.stringify({ exit_code: 0, stdout: 'v20.1.0\n', stderr: '' }),
          { status: 200 }
        );
      }
      return new Response('no route', { status: 404 });
    });

    const sbx = await Sandbox.create({ ...baseCfg, fetch });
    const result = await sbx.exec(['node', '--version']);
    expect(result).toEqual({ exitCode: 0, stdout: 'v20.1.0\n', stderr: '' });
  });
});

describe('Sandbox.domain', () => {
  it('returns null when the install has no preview template configured', async () => {
    const fetch = mockFetch(
      () =>
        new Response(
          JSON.stringify({
            id: 'sbx_abc',
            name: '',
            status: 'running',
            image: null,
            work_dir: '/workspace',
            created_at: '',
            expires_at: '',
            preview_url_template: '',
          }),
          { status: 201 }
        )
    );
    const sbx = await Sandbox.create({ ...baseCfg, fetch });
    expect(sbx.domain(3000)).toBeNull();
  });
});
