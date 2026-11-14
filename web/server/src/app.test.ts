import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, describe, it } from 'node:test';

import type { FastifyInstance } from 'fastify';

import { buildApp } from './app.js';

describe('the relay with a frontend build', () => {
  let app: FastifyInstance;
  let dir: string;

  before(async () => {
    dir = mkdtempSync(join(tmpdir(), 'minder-client-'));
    mkdirSync(join(dir, 'assets'));
    writeFileSync(join(dir, 'index.html'), '<!doctype html><title>minder</title>');
    writeFileSync(join(dir, 'assets', 'app-abc123.js'), 'export {};');
    app = await buildApp({ clientDir: dir, logger: false });
  });

  after(async () => {
    await app.close();
    rmSync(dir, { recursive: true, force: true });
  });

  it('answers /health', async () => {
    const response = await app.inject({ method: 'GET', url: '/health' });
    assert.equal(response.statusCode, 200);
    assert.deepEqual(response.json(), { status: 'ok' });
  });

  it('serves the page, revalidated on every load', async () => {
    const response = await app.inject({ method: 'GET', url: '/' });
    assert.equal(response.statusCode, 200);
    assert.match(response.body, /<title>minder<\/title>/);
    assert.equal(response.headers['cache-control'], 'no-cache');
  });

  it('serves hashed assets as immutable', async () => {
    const response = await app.inject({ method: 'GET', url: '/assets/app-abc123.js' });
    assert.equal(response.statusCode, 200);
    assert.equal(response.headers['cache-control'], 'public, max-age=31536000, immutable');
  });

  it('sets the basic hardening headers', async () => {
    const response = await app.inject({ method: 'GET', url: '/health' });
    assert.equal(response.headers['x-content-type-options'], 'nosniff');
    assert.equal(response.headers['referrer-policy'], 'no-referrer');
    assert.equal(response.headers['x-frame-options'], 'DENY');
  });

  it('answers the pages of the app with the HTML shell, so deep links and reloads work', async () => {
    for (const url of ['/app', '/app/', '/docs', '/docs/', '/docs/anything/deeper', '/app?x=1', '/docs#proofs']) {
      const response = await app.inject({ method: 'GET', url });
      assert.equal(response.statusCode, 200, url);
      assert.match(response.headers['content-type'] as string, /^text\/html/, url);
      assert.match(response.body, /<title>minder<\/title>/, url);
      assert.equal(response.headers['cache-control'], 'no-cache', url);
    }
  });

  it('answers a HEAD for a page too', async () => {
    const response = await app.inject({ method: 'HEAD', url: '/app' });
    assert.equal(response.statusCode, 200);
    assert.match(response.headers['content-type'] as string, /^text\/html/);
  });

  it('keeps the API as JSON: an unknown /api path is a JSON 404, not the shell', async () => {
    for (const url of ['/api', '/api/', '/api/unknown', '/api/unknown/deeper?x=1']) {
      const response = await app.inject({ method: 'GET', url });
      assert.equal(response.statusCode, 404, url);
      assert.match(response.headers['content-type'] as string, /^application\/json/, url);
      assert.deepEqual(response.json(), { error: 'not found' }, url);
    }
  });

  it('keeps /health as JSON above the catch-all', async () => {
    const response = await app.inject({ method: 'GET', url: '/health' });
    assert.match(response.headers['content-type'] as string, /^application\/json/);
    assert.deepEqual(response.json(), { status: 'ok' });
  });

  it('never answers a missing asset or file with the HTML shell', async () => {
    for (const url of ['/assets/missing.js', '/assets/nested/missing.css', '/missing.png', '/wp-login.php']) {
      const response = await app.inject({ method: 'GET', url });
      assert.equal(response.statusCode, 404, url);
      assert.deepEqual(response.json(), { error: 'not found' }, url);
    }
  });

  it('serves only GET and HEAD as pages', async () => {
    for (const method of ['POST', 'PUT', 'DELETE'] as const) {
      const response = await app.inject({ method, url: '/app' });
      assert.equal(response.statusCode, 404, method);
      assert.deepEqual(response.json(), { error: 'not found' }, method);
    }
  });
});

describe('the relay with no frontend build', () => {
  it('still answers /health and has no page', async () => {
    const app = await buildApp({ clientDir: join(tmpdir(), 'minder-no-such-dir'), logger: false });
    try {
      const health = await app.inject({ method: 'GET', url: '/health' });
      assert.equal(health.statusCode, 200);
      for (const url of ['/', '/app', '/docs']) {
        const page = await app.inject({ method: 'GET', url });
        assert.equal(page.statusCode, 404, url);
        assert.deepEqual(page.json(), { error: 'not found' }, url);
      }
    } finally {
      await app.close();
    }
  });
});
