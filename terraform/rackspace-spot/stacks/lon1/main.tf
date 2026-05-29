terraform {
  required_version = ">= 1.6.0"

  required_providers {
    spot = {
      source  = "rackerlabs/spot"
      version = ">= 0.1.0"
    }
    local = {
      source  = "hashicorp/local"
      version = ">= 2.0.0"
    }
  }
}

module "relay" {
  source = "../../modules/relay-cloudspace"

  stack_name       = "lon1"
  cloudspace_name  = var.cloudspace_name
  region           = var.region
  server_class     = var.server_class
  bid_price        = var.bid_price
  relay_node_label = "lon1"
  rackspace_spot_token = var.rackspace_spot_token
}

output "cloudspace_name" {
  value = module.relay.cloudspace_name
}

output "region" {
  value = module.relay.region
}

output "server_class" {
  value = module.relay.server_class
}
