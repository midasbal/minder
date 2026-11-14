import { existsSync } from 'node:fs';
import { sep } from 'node:path';

import fastifyStatic from '@fastify/static';
import Fastify, { type FastifyInstance } from 'fastify';

export interface AppOptions {
  /** Directory holding the built frontend. Served when it exists. */
  clientDir: string;
  /** Request logging. On by default; the tests turn it off. */
  logger?: boolean;
}

const IMMUTABLE = 'public, max-age=31536000, immutable';
const REVALIDATE = 'no-cache';

/**
 * Whether a request that matched nothing is a page of the single-page app (which
 * the browser router resolves) rather than a missing API route or asset. Only a
 * GET or HEAD can be a page; `/api` is the API's; `/assets/` and anything that
 * names a file (it has an extension) is an asset, and a missing asset must stay
 * a 404, never an HTML page standing in for a script or a style.
 */
function isAppPage(method: string, url: string): boolean {
  if (method !== 'GET' && method !== 'HEAD') return false;
  const pathname = url.split(/[?#]/, 1)[0] ?? '';
  if (pathname === '/api' || pathname.startsWith('/api/')) return false;
  if (pathname.startsWith('/assets/')) return false;
  return !/\.[a-z0-9]+$/i.test(pathname);
}

/**
 * Builds the single-port app: the relay API and, when a build is present, the
 * frontend it serves, with every page of the app answered by the HTML shell.
 * Nothing here touches a key, a chain or a contract.
 */
export async function buildApp({ clientDir, logger = true }: AppOptions): Promise<FastifyInstance> {
  const app = Fastify({ logger });

  app.addHook('onSend', async (_request, reply) => {
    reply.header('x-content-type-options', 'nosniff');
    reply.header('referrer-policy', 'no-referrer');
    reply.header('x-frame-options', 'DENY');
  });

  app.get('/health', async () => ({ status: 'ok' }));

  const hasFrontend = existsSync(clientDir);
  if (hasFrontend) {
    // Built assets carry a content hash in their name, so they never change;
    // everything else (the page itself) is revalidated on each load.
    await app.register(fastifyStatic, {
      root: clientDir,
      cacheControl: false,
      setHeaders: (reply, filePath) => {
        const hashed = filePath.includes(`${sep}assets${sep}`);
        reply.header('cache-control', hashed ? IMMUTABLE : REVALIDATE);
      },
    });
  } else {
    app.log.warn({ clientDir }, 'no frontend build found: serving the API only');
  }

  // Routes and files are matched first, so /health, /api/* and real assets keep
  // their own answers. What is left is either a page of the app, which gets the
  // HTML shell (the router takes it from there, so a deep link or a reload works),
  // or a plain 404.
  app.setNotFoundHandler(async (request, reply) => {
    if (hasFrontend && isAppPage(request.method, request.url)) {
      return reply.sendFile('index.html');
    }
    return reply.code(404).send({ error: 'not found' });
  });

  return app;
}
