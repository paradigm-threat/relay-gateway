locals {
  state_file = "${path.module}/vm-state.json"
}

resource "null_resource" "spot_vm" {
  triggers = {
    cloudspace_name = var.cloudspace_name
    server_class    = var.server_class
    bid_price       = var.bid_price
    ip_override     = var.public_ip_override
  }

  provisioner "local-exec" {
    command     = "${path.module}/scripts/spot-vm-provision.sh apply"
    environment = {
      SPOT_VM_STATE_FILE      = local.state_file
      SPOT_CLOUDSPACE_NAME    = var.cloudspace_name
      SPOT_VM_POOL_NAME       = var.vm_pool_name
      SPOT_REGION             = var.region
      SPOT_SERVER_CLASS       = var.server_class
      SPOT_BID_PRICE          = tostring(var.bid_price)
      SPOT_DESIRED_VM_COUNT   = tostring(var.desired_vm_count)
      SPOT_OS_IMAGE           = var.os_image
      SPOT_SSH_PUBKEY_PATH    = var.ssh_pubkey_path
      RXTSPOT_TOKEN           = var.rackspace_spot_token
      SPOT_PUBLIC_IP_OVERRIDE = var.public_ip_override
      RELAY_NODE              = var.relay_node
    }
  }
}

data "external" "vm" {
  depends_on = [null_resource.spot_vm]
  program    = ["bash", "${path.module}/scripts/spot-vm-read-state.sh"]
}
