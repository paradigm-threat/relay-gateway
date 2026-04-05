#!/bin/bash
TOKEN=$(grep -oP 'accessToken: \K\S+' ~/.spot_config)
ENDPOINTS=("cloudspaces" "nodepools" "virtual-machines" "instances" "servers")
for e in "${ENDPOINTS[@]}"; do
  echo "--- Testing $e ---"
  curl -s -H "Authorization: Bearer $TOKEN" "https://api.rackspacespot.com/v1/$e?org_id=org_5bVgfd7fI7iYYj2X" | jq . | head -n 20
done
