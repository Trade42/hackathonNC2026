import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// base stays '/' so deep links like /r/run_demo resolve assets correctly.
// Capacitor and Tauri both serve the build from their own origin root.
export default defineConfig({
  base: process.env.VITE_BASE ?? '/',
  plugins: [react()],
  server: { host: true, port: 5173 },
  preview: { host: true, port: 4173 },
  build: { target: 'es2020', sourcemap: false },
})
