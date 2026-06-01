output "public_ip" {
  value       = linode_instance.atlanta2.ip_address
  description = "Public IPv4 for DNS and SSH"
}

output "fqdn" {
  value       = "${var.relay_node}.relaygateway.net"
  description = "Node apex hostname"
}

output "relay_server_id" {
  value = "relay-${var.relay_node}"
}

output "ssh_user" {
  value = var.ssh_user
}

output "linode_id" {
  value       = linode_instance.atlanta2.id
  description = "Use for terraform import"
}
