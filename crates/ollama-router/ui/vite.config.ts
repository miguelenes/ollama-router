import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  base: '/router/ui/',
  plugins: [react()],
  build: {
    rollupOptions: {
      output: { entryFileNames: 'assets/app.js', assetFileNames: 'assets/[name][extname]' },
    },
  },
})
