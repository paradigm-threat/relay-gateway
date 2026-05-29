variable "cloudspace_name" {
  type    = string
  default = "relay-ord1"
}

variable "region" {
  type    = string
  default = "us-central-ord-1"
}

variable "server_class" {
  type    = string
  default = "gp.vs1.small-ord"
}

variable "bid_price" {
  type    = number
  default = 0.002
}

variable "rackspace_spot_token" {
  type      = string
  sensitive = true
  default   = null
}
