# Minder web

The demo frontend and its relay, served by one process on one port.

- `client/`: Vite, React, TypeScript, Framer Motion, hand-written CSS. A shell for now.
- `server/`: Node, TypeScript, Fastify. Serves the built frontend and the API. Today the API is `GET /health`.

There are no keys, no chain access and no contract calls in this directory yet.

```
web/
  package.json            workspace root: build, start, dev, test scripts
  package-lock.json
  Dockerfile              one image: builds both parts, runs the relay
  .dockerignore
  client/
    index.html
    vite.config.ts        dev server proxies /health and /api to the relay
    src/                  main.tsx, App.tsx, useRelayHealth.ts, styles.css
    public/favicon.svg
  server/
    src/app.ts            the app: /health, static files, JSON 404
    src/index.ts          reads PORT and HOST, starts, shuts down on SIGTERM
    src/app.test.ts
```

## Run it locally

Needs Node 22 or newer.

```
cd web
npm ci
npm run build        # builds client/dist and server/dist
npm start            # one process: http://localhost:8080
```

- Page: <http://localhost:8080/>
- Relay health: <http://localhost:8080/health>, which answers `{"status":"ok"}`

`PORT` (default `8080`) and `HOST` (default `0.0.0.0`) set where it listens.

For development, run two terminals. Vite reloads the page and proxies `/health` and `/api` to the relay:

```
npm run dev:server   # relay on http://localhost:8080
npm run dev:client   # page on http://localhost:5173
```

Checks: `npm run typecheck` and `npm test` (the relay's tests).

## Get a public URL

Any host that builds a Dockerfile and sets `PORT` works. The service needs no environment variables of its own. Push the repository to a Git host the platform can read, then follow one of these. Menu labels vary a little between platform updates.

### Railway

1. Create a project and choose **Deploy from GitHub repo**; select this repository.
2. Open the service's **Settings** and set **Root Directory** to `web`. Railway builds the `Dockerfile` it finds there.
3. Under **Deploy**, set the **Healthcheck Path** to `/health`.
4. Under **Networking**, choose **Generate Domain**. The result is the public URL, `https://<service>.up.railway.app`.

### Render

1. Choose **New**, then **Web Service**, and connect this repository.
2. Set **Language** to **Docker** and **Root Directory** to `web`.
3. Set **Health Check Path** to `/health`, then create the service.
4. The public URL, `https://<service>.onrender.com`, is shown on the service page once the first deploy finishes.

### Anywhere else (Fly.io, a VPS, and so on)

```
cd web
docker build -t minder-web .
docker run --rm -p 8080:8080 -e PORT=8080 minder-web
```

Put TLS in front of it with the platform's proxy or your own. The container listens on `$PORT` on all interfaces, answers `/health`, and exits cleanly on `SIGTERM`.

### Check a deployment

```
curl https://<your-public-url>/health        # {"status":"ok"}
```

Then open the URL. The footer reads `relay: online` when the page has reached the relay.

## Notes

- The service holds no state, so any number of restarts is fine. Free tiers that put a service to sleep will delay the first request after idle time, and will not suit the always-on monitor planned for later.
- When keys are added, they go in the platform's secret environment variables, never in the repository or the image.
