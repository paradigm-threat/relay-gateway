#!/usr/bin/env node
/**
 * Relay MCP smoke check (health + initialize + tools/list + search).
 * Usage: node scripts/validate-relay-mcp.mjs [baseUrl] [repo]
 */
const base = (process.argv[2] || 'http://127.0.0.1:8080').replace(/\/$/, '')
const repo = process.argv[3] || 'paradigm-threat-files'

async function rpc (method, params, id = 1) {
  const res = await fetch(`${base}/api/mcp/${repo}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id, method, params: params || {} }),
  })
  const body = await res.json()
  return { status: res.status, body }
}

async function main () {
  const health = await fetch(`${base}/api/mcp/${repo}?health=1`)
  const hj = await health.json()
  console.log('health', health.status, hj)
  if (!health.ok || hj.mcpEnabled === false) {
    process.exitCode = 1
    return
  }

  const del = await fetch(`${base}/api/mcp/${repo}`, { method: 'DELETE' })
  console.log('delete', del.status)
  if (del.status !== 204) process.exitCode = 1

  const init = await rpc('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'validate', version: '0' } }, 1)
  console.log('initialize', init.status, init.body?.result?.serverInfo)
  if (init.status !== 200 || !init.body?.result?.serverInfo) process.exitCode = 1

  const list = await rpc('tools/list', {}, 2)
  const tools = list.body?.result?.tools || []
  console.log('tools/list', list.status, tools.map((t) => t.name).join(', '))
  if (!tools.length) process.exitCode = 1

  const call = await rpc('tools/call', { name: 'search', arguments: { query: 'test', limit: 5 } }, 3)
  console.log('tools/call search', call.status, JSON.stringify(call.body).slice(0, 400))
  if (call.status !== 200 || call.body?.error) process.exitCode = 1
  if (call.body?.result?.isError) {
    console.warn('search returned isError (index may be cold):', call.body.result.content?.[0]?.text)
  }
}

main().catch((e) => {
  console.error(e)
  process.exitCode = 1
})
