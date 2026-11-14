# Minder web

The demo frontend and its relay, served by one process on one port.

- `client/`: Vite, React, TypeScript, React Router, Framer Motion, hand-written CSS.
- `server/`: Node, TypeScript, Fastify. Serves the built frontend and the API from one port.

The pages are `/` (the overview), `/app` (the live demo surface, a placeholder for now) and `/docs`. The relay's API is
`GET /health` today. Any other GET that is not under `/api/` and does not name a file is answered with the app's
`index.html`, so a deep link or a reload on `/app` or `/docs` works; an unknown `/api/*` path or a missing asset is a JSON
404. Nothing in this directory touches a key, a chain or a contract yet.

```
web/
  package.json            workspace root: build, start, dev, typecheck, test scripts
  package-lock.json
  Dockerfile              one image: builds both parts, runs the relay
  .dockerignore
  client/
    index.html
    vite.config.ts        dev server proxies /health and /api to the relay
    public/favicon.svg
    src/
      main.tsx            entry: router, fonts, styles
      App.tsx             layout and routes
      links.ts            repository, explorer and account links
      motion.ts           shared Framer Motion variants
      useRelayHealth.ts   the footer's relay status
      components/         Header, Footer, Rail, Buttons, ExternalLink, ScrollManager
      pages/              Landing, AppShell, NotFound
      docs/               Docs and its sections (Overview ... Reference), contents, reading helpers
      styles.css, landing.css, docs.css
      App.test.tsx        renders every route to HTML and checks the chrome and the content
  server/
    src/app.ts            /health, static files, the app catch-all, JSON 404
    src/index.ts          reads PORT and HOST, starts, shuts down on SIGTERM
    src/app.test.ts
```

## Run it locally

Needs Node 22.22 or newer (React Router 8 requires it); the Dockerfile pins 22.23.5.

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

Checks: `npm run typecheck`, and `npm test` (the relay's tests and the client's render tests).

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
