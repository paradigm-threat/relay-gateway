#!/bin/bash
REGIONS=$(spotctl regions list | jq -r '.[].name')
for REGION in $REGIONS; do
    echo "Checking region: $REGION"
    # Update config manually since there's no --region flag for most commands
    sed -i "s/region: .*/region: $REGION/" ~/.spot_config
    spotctl cloudspaces get --name relay-network --org clevertree 2>/dev/null
    if [ $? -eq 0 ]; then
        echo "FOUND in region: $REGION"
        spotctl cloudspaces get --name relay-network --org clevertree
        exit 0
    fi
done
echo "Not found in any region."
