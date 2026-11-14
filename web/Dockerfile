# One image, one process: the relay serves the built frontend and the API on a
# single port. Build context is this directory (web/).

# One Node version for both stages. React Router 8 needs Node 22.22 or newer, so
# this is pinned rather than left to a floating tag that a build cache could hold
# at an older release. Raise it deliberately, with the tests.
ARG NODE_VERSION=22.23.5

FROM node:${NODE_VERSION}-alpine AS build
WORKDIR /app
COPY package.json package-lock.json ./
COPY client/package.json client/
COPY server/package.json server/
RUN npm ci
COPY client client
COPY server server
RUN npm run build

FROM node:${NODE_VERSION}-alpine AS runtime
ENV NODE_ENV=production
ENV PORT=8080
WORKDIR /app
COPY package.json package-lock.json ./
COPY client/package.json client/
COPY server/package.json server/
RUN npm ci --omit=dev --workspace server && npm cache clean --force
COPY --from=build /app/server/dist server/dist
COPY --from=build /app/client/dist client/dist
USER node
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD node -e "fetch('http://127.0.0.1:'+process.env.PORT+'/health').then(r=>process.exit(r.ok?0:1),()=>process.exit(1))"
CMD ["node", "server/dist/index.js"]
