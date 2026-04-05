#!/bin/bash
TOKEN=$(grep -oP 'accessToken: \K\S+' ~/.spot_config)
ORGS="org_5bVgfd7fI7iYYj2X"
URL="https://spot.rackspace.com/apis/ngpc.rxt.io/v1/namespaces/$ORGS/cloudspaces"
echo "Fetching cloudspaces from $URL..."
curl -s -k -H "Authorization: Bearer $TOKEN" "$URL" | jq .
