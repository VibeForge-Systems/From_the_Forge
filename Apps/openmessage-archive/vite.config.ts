/// <reference types="vitest/config" />
import { fileURLToPath, URL } from 'node:url';
import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

/** The zones the DST Vitest projects run in (M5 §13.8). */
// Pacific/Apia skipped a whole local day (2011-12-30) when Samoa moved
// across the date line.
const DST_ZONES = ['Europe/London', 'America/Sao_Paulo', 'Australia/Lord_Howe', 'Pacific/Apia'];

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
    restoreMocks: true,
    // The main suite, plus one project per time zone for the DST tests of
    // the day-boundary builder (M5 §13.8). Each DST project sets `TZ` in its
    // workers' environment; the spec's first assertion checks that the zone
    // took effect, so a runner that ignores `TZ` fails loudly.
    projects: [
      {
        extends: true as const,
        test: {
          name: 'frontend',
          include: ['tests/frontend/**/*.spec.ts'],
          exclude: ['tests/frontend/dst/**'],
        },
      },
      ...DST_ZONES.map((zone) => ({
        extends: true as const,
        test: {
          name: `dst ${zone}`,
          include: ['tests/frontend/dst/**/*.spec.ts'],
          env: { TZ: zone },
        },
      })),
    ],
  },
});
