import { fileURLToPath } from 'node:url';

import { buildApp } from './app.js';

const port = Number(process.env.PORT ?? 8080);
const host = process.env.HOST ?? '0.0.0.0';
const clientDir =
  process.env.CLIENT_DIR ?? fileURLToPath(new URL('../../client/dist', import.meta.url));

if (!Number.isInteger(port) || port < 1 || port > 65535) {
  console.error(`PORT must be an integer from 1 to 65535, got "${process.env.PORT}"`);
  process.exit(1);
}

const app = await buildApp({ clientDir });

// Hosting platforms stop a service with SIGTERM: finish in-flight requests, then exit.
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.once(signal, () => {
    app.close().then(
      () => process.exit(0),
      (error: unknown) => {
        app.log.error(error);
        process.exit(1);
      },
    );
  });
}

try {
  await app.listen({ port, host });
} catch (error) {
  app.log.error(error);
  process.exit(1);
}
