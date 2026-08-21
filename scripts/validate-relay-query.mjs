#!/usr/bin/env node
/**
 * Probe HTTP QUERY envelope: indexFresh, docCount, status.
 * Usage: node scripts/validate-relay-query.mjs https://iad1.relaygateway.net paradigm-threat-files [query]
 */
const base = (process.argv[2] || "").replace(/\/$/, "");
const repo = process.argv[3] || "paradigm-threat-files";
const q = process.argv[4] || "venus";
const collection = process.argv[5] || (repo.includes("timeline") ? "events" : "index");

if (!base) {
  console.error("usage: validate-relay-query.mjs <baseUrl> <repo> [query] [collection]");
  process.exit(2);
}

async function main() {
  const url = `${base}/${repo}`;
  const res = await fetch(url, {
    method: "QUERY",
    headers: {
      "content-type": "application/json",
      accept: "application/json",
    },
    body: JSON.stringify({ collection, query: q }),
  });
  const text = await res.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    console.error("QUERY", res.status, text.slice(0, 400));
    process.exit(1);
  }
  console.log("QUERY", res.status, {
    status: body.status,
    indexFresh: body.indexFresh,
    docCount: body.docCount,
    count: body.count ?? (body.results || []).length,
    indexedHead: (body.indexedHead || "").slice(0, 12),
    tookMs: body.tookMs,
  });
  if (res.status !== 200) process.exit(1);
  if (body.status === "not_indexed") {
    console.error("index not ready (status=not_indexed)");
    process.exit(1);
  }
  if (!body.indexFresh) {
    console.warn("warn: indexFresh=false (tip ahead of indexedHead)");
  }
  if (!(body.docCount > 0)) {
    console.error("expected docCount > 0");
    process.exit(1);
  }
  if (!Array.isArray(body.results)) {
    console.error("missing results array");
    process.exit(1);
  }
  console.log("ok");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
