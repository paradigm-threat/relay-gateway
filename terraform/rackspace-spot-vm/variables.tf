variable "relay_node" {
  type    = string
  default = "iad1"
}

variable "region" {
  type    = string
  default = "us-east-iad-1"
}

variable "server_class" {
  type    = string
  default = "gp.vs1.medium-iad"
}

variable "bid_price" {
  type    = number
  default = 0.01
}

variable "cloudspace_name" {
  type    = string
  default = "relay-network-iad"
}

variable "vm_pool_name" {
  type    = string
  default = ""
}

variable "desired_vm_count" {
  type    = number
  default = 1
}

variable "os_image" {
  description = "Spot VM image name (UI: ubuntu2404)"
  type        = string
  default     = "ubuntu2404"
}

variable "ssh_pubkey_path" {
  type    = string
  default = "~/.ssh/id_ed25519_relay_gateway.pub"
}

variable "ssh_user" {
  type    = string
  default = "ubuntu"
}

variable "rackspace_spot_token" {
  type      = string
  sensitive = true
  default   = ""
}

variable "public_ip_override" {
  description = "Skip Spot API; use existing VM IP (install-only / UI-created cloudspace)"
  type        = string
  default     = ""
}
