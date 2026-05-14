import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  timeout: 30_000,
  use: {
    baseURL: 'http://127.0.0.1:9323',
  },
  webServer: {
    command: 'npx --yes serve wasm -l 9323 --cors',
    url: 'http://127.0.0.1:9323',
    reuseExistingServer: true,
    timeout: 60_000,
  },
})
