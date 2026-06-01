output "public_ip" {
  value       = data.external.vm.result.public_ip
  description = "VM public IPv4"
}

output "fqdn" {
  value = "${var.relay_node}.relaygateway.net"
}

output "relay_server_id" {
  value = "relay-${var.relay_node}"
}

output "ssh_user" {
  value = var.ssh_user
}

output "cloudspace_name" {
  value = var.cloudspace_name
}
