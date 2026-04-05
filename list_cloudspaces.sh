#!/bin/bash
REGIONS="us-central-dfw-1 us-central-dfw-2 us-east-ord-1 us-west-ord-1 us-east-iad-1 us-central-dfw-1 ap-south-bom-1 eu-central-fra-1 eu-west-lon-1 eu-west-lon-2 us-west-sjc-1 hkg-hkg-1 aus-syd-1"
for r in $REGIONS; do
  echo "Checking region $r..."
  sed -i "s/region: .*/region: $r/" ~/.spot_config
  spotctl cloudspaces list --org org_5bVgfd7fI7iYYj2X 2>/dev/null | grep -v null
  spotctl cloudspaces list --org clevertree 2>/dev/null | grep -v null
done
