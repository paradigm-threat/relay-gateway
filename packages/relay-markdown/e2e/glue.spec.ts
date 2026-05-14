import { test, expect } from '@playwright/test'

/**
 * Package-level glue tests (relay-markdown dist + wasm on a static file server).
 * Run from relay-markdown: npx playwright test
 */
test.describe('@clevertree/relay-markdown static glue', () => {
  test('WASM bundle files exist on disk', async () => {
    const { existsSync } = await import('node:fs')
    const { join, dirname } = await import('node:path')
    const { fileURLToPath } = await import('node:url')
    const pkg = join(dirname(fileURLToPath(import.meta.url)), '..')
    expect(existsSync(join(pkg, 'dist/diagnostics.js'))).toBe(true)
    expect(existsSync(join(pkg, 'dist/MarkdownRenderer.js'))).toBe(true)
    expect(existsSync(join(pkg, 'wasm/md2ast.js'))).toBe(true)
    expect(existsSync(join(pkg, 'wasm/md2ast_bg.wasm'))).toBe(true)
  })

  test('served WASM entry returns 200', async ({ request, baseURL }) => {
    const js = await request.get(`${baseURL}/md2ast.js`)
    expect(js.status()).toBe(200)
    const wasm = await request.get(`${baseURL}/md2ast_bg.wasm`)
    expect(wasm.status()).toBe(200)
  })
})
