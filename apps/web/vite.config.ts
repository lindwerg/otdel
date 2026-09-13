import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

// OTDEL frontend (phase 1A).
//
// Dev server binds to a fixed port (15173) with strictPort so it never
// silently falls back to a different port (docs/implementation-contract.md).
// /api is proxied to the local Rust API (127.0.0.1:18480) so the browser
// sees same-origin requests: the HttpOnly session cookie set by the API is
// therefore usable without any cross-origin credential dance.
export default defineConfig({
  plugins: [react()],
  server: {
    host: '127.0.0.1',
    port: 15173,
    strictPort: true,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:18480',
        changeOrigin: true,
      },
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    css: false,
  },
})
