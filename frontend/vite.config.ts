import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  test: { environment: 'jsdom' },
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      '/api': {
        target: 'http://localhost:3000',
        changeOrigin: true,
        ws: true,
      },
      '/oauth': {
        target: 'http://localhost:3000',
        changeOrigin: true,
      },
      '/setup': {
        target: 'http://localhost:3000',
        changeOrigin: true,
      },
    },
  },
})
