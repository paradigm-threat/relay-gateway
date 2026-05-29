variable "cloudspace_name" {
  type    = string
  default = "relay-lon1"
}

variable "region" {
  type    = string
  default = "eu-west-lon-1"
}

variable "server_class" {
  type    = string
  default = "gp.vs1.small-lon"
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
