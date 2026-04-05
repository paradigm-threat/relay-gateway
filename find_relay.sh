#!/bin/bash
TOKEN=$(grep -oP 'accessToken: \K\S+' ~/.spot_config)
REGIONS="us-east-ord-1 us-west-sjc-1 us-east-iad-1 us-central-dfw-1 us-central-dfw-2 hkg-hkg-1 aus-syd-1 uk-lon-1"
for r in $REGIONS; do
  echo "--- Region $r ---"
  # We use the fact that the API might not strictly enforce region in the token/config if we hit the right endpoint
  # But we'll try to find it.
  curl -s -H "Authorization: Bearer $TOKEN" "https://api.rackspacespot.com/v1/cloudspaces?org_id=org_5bVgfd7fI7iYYj2X" | jq .
done
