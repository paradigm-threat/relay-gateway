variable "stack_name" {
  description = "Stack id (dfw1, lon1, ord1) — used for kubeconfig output path"
  type        = string
}

variable "cloudspace_name" {
  description = "Rackspace Spot cloudspace name"
  type        = string
}

variable "region" {
  description = "Rackspace Spot region identifier"
  type        = string
}

variable "server_class" {
  description = "Spot server class (e.g. gp.vs1.small-lon)"
  type        = string
}

variable "bid_price" {
  description = "Spot bid price USD/hour"
  type        = number
  default     = 0.002
}

variable "relay_node_label" {
  description = "Kubernetes node label value for relay-node selector"
  type        = string
}

variable "kubernetes_version" {
  type    = string
  default = "1.30.10"
}

variable "rackspace_spot_token" {
  type      = string
  sensitive = true
  default   = null
}
