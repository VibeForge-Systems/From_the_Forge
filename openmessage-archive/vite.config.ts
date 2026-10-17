/// <reference types="vitest/config" />
import { fileURLToPath, URL } from 'node:url';
import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

// Tauri expects a fixed dev port and must see Rust compiler output, so the
// screen is not cleared and a busy port fails fast instead of moving.
export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: 'localhost',
    watch: {
      ignored: ['**/src-tauri/**', '**/crates/**', '**/target/**'],
    },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    // WebView2 (Chromium) on Windows and WebKitGTK 2.40+ on Linux.
    target: ['es2022', 'chrome110', 'safari16'],
    sourcemap: false,
    // Fonts and images ship as files; inlining them as data: URIs would
    // need a looser CSP for no benefit in a desktop app.
    assetsInlineLimit: 0,
  },
  test: {
    environment: 'jsdom',
    include: ['tests/frontend/**/*.spec.ts'],
    restoreMocks: true,
  },
});
