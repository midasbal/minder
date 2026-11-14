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
 * Builds the single-port app: the relay API and, when a build is present, the
 * frontend it serves. Nothing here touches a key, a chain or a contract.
 */
export async function buildApp({ clientDir, logger = true }: AppOptions): Promise<FastifyInstance> {
  const app = Fastify({ logger });

  app.addHook('onSend', async (_request, reply) => {
    reply.header('x-content-type-options', 'nosniff');
    reply.header('referrer-policy', 'no-referrer');
    reply.header('x-frame-options', 'DENY');
  });

  app.get('/health', async () => ({ status: 'ok' }));

  if (existsSync(clientDir)) {
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

  app.setNotFoundHandler(async (_request, reply) => reply.code(404).send({ error: 'not found' }));

  return app;
}
