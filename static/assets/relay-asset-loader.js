/**
 * Minimal loader for markdown embeds: data-relay-asset + data-relay-wait on <img>.
 * Full API: relay-client-web relayAssetFetch.ts
 */
(function () {
  const RETRYABLE = new Set([
    'ipfs-pending',
    'ipfs-gateway-timeout',
    'ipfs-gateway-error',
    'ipfs-busy',
    'internal',
  ])

  function backoff(attempt) {
    return [2000, 5000, 15000, 45000][Math.min(attempt, 3)] || 45000
  }

  async function fetchAsset(url, waitSec, maxRetries, signal) {
    let attempt = 0
    while (true) {
      const u = new URL(url, location.origin)
      if (waitSec > 0) u.searchParams.set('wait', String(waitSec))
      const res = await fetch(u.toString(), { cache: 'no-store', signal })
      if (res.ok) {
        const blob = await res.blob()
        return URL.createObjectURL(blob)
      }
      const code = res.headers.get('X-Relay-Asset-Error') || ''
      let retryable = RETRYABLE.has(code)
      try {
        const j = await res.json()
        if (typeof j.retryable === 'boolean') retryable = j.retryable
      } catch (_) {}
      if (code === 'ipfs-pending' && waitSec > 0) {
        await new Promise((r) => setTimeout(r, (parseInt(res.headers.get('Retry-After') || '5', 10) || 5) * 1000))
        continue
      }
      if (retryable && attempt < maxRetries) {
        await new Promise((r) => setTimeout(r, backoff(attempt)))
        attempt++
        continue
      }
      throw new Error(code || String(res.status))
    }
  }

  function upgradeImages(root) {
    root.querySelectorAll('img[data-relay-asset]').forEach((img) => {
      const url = img.getAttribute('data-relay-asset')
      if (!url) return
      const wait = parseInt(img.getAttribute('data-relay-wait') || '60', 10)
      const ph = img.getAttribute('data-relay-placeholder') || '/assets/missing-image.svg'
      fetchAsset(url, wait, 4)
        .then((blobUrl) => {
          img.addEventListener('load', () => URL.revokeObjectURL(blobUrl), { once: true })
          img.src = blobUrl
        })
        .catch(() => {
          img.src = ph
        })
    })
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => upgradeImages(document))
  } else {
    upgradeImages(document)
  }
})()
