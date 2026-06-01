variable "linode_token" {
  description = "Linode API token (LINODE_TOKEN env or terraform.tfvars — never commit)"
  type        = string
  sensitive   = true
}

variable "region" {
  description = "Linode region for atlanta2 (production: us-southeast)"
  type        = string
  default     = "us-southeast"
}

variable "instance_type" {
  description = "Match live atlanta2 size on import — do not upsize without intent"
  type        = string
  default     = "g6-standard-2"
}

variable "image" {
  type    = string
  default = "linode/ubuntu24.04"
}

variable "label" {
  type    = string
  default = "atlanta2-relay"
}

variable "relay_node" {
  description = "DNS slug and RELAY_SERVER_ID suffix"
  type        = string
  default     = "atlanta2"
}

variable "ssh_pubkey_path" {
  description = "Deploy key public path (relay-fleet default)"
  type        = string
  default     = "~/.ssh/id_ed25519_relay_gateway.pub"
}

variable "ssh_user" {
  type    = string
  default = "ari"
}
