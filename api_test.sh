#!/bin/bash
TOKEN=$(grep -oP 'accessToken: \K\S+' ~/.spot_config)
IP="104.130.255.148"
HOST="api.rackspacespot.com"
echo "Fetching organizations..."
curl -s -k -H "Host: $HOST" -H "Authorization: Bearer $TOKEN" "https://$IP/v1/organizations" | jq .
echo "Fetching cloudspaces..."
curl -s -k -H "Host: $HOST" -H "Authorization: Bearer $TOKEN" "https://$IP/v1/organizations/org_5bVgfd7fI7iYYj2X/cloudspaces" | jq .
echo "Fetching pricing..."
curl -s -k -H "Host: $HOST" -H "Authorization: Bearer $TOKEN" "https://$IP/v1/pricing" | jq .
