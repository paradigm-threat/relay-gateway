variable "relay_node" {
  type    = string
  default = "dfw1"
}

variable "region" {
  type    = string
  default = "us-central-dfw-1"
}

variable "server_class" {
  type    = string
  default = "gp.vs1.medium-dfw"
}

variable "bid_price" {
  type    = number
  default = 0.005
}

variable "cloudspace_name" {
  type    = string
  default = "relay-dfw1-vm"
}

variable "vm_pool_name" {
  type    = string
  default = "dfw1-pool"
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
