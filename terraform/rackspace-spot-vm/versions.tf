terraform {
  required_version = ">= 1.5.0"
  required_providers {
    external = {
      source  = "hashicorp/external"
      version = "~> 2.3"
    }
    null = {
      source  = "hashicorp/null"
      version = "~> 3.2"
    }
  }
}

# Rackspace Spot VM Cloudspaces are not yet in rackerlabs/spot Terraform (May 2026).
# Provisioning uses scripts/spot-vm-provision.sh + vm-state.json (gitignored).
