import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  // Required for the M4 AudioWorklet/SharedArrayBuffer path (PLAN.md §5's
  // "Web audio I/O" note) -- copied verbatim from latent-mixing's
  // vite.config.ts, which is where this pattern was proven working.
  server: {
    // 5173 unless something already has it (`cargo tauri dev` expects that
    // port -- see WORKFLOW.md). PORT lets a second dev server run alongside.
    port: Number(process.env.PORT) || 5173,
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    },
  },
  preview: {
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    },
  },
})
