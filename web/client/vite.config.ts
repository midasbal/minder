import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// In development the page is served by Vite and the relay runs on its own port;
// these paths go to the relay, as they are the same origin in a deployment.
const relay = 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/health': relay,
      '/api': relay,
    },
  },
});
