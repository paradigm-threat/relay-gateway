data "local_file" "ssh_pubkey" {
  filename = pathexpand(var.ssh_pubkey_path)
}

resource "linode_sshkey" "relay_deploy" {
  label  = "relay-gateway-deploy"
  ssh_key = chomp(data.local_file.ssh_pubkey.content)
}

resource "linode_instance" "atlanta2" {
  label           = var.label
  region          = var.region
  type            = var.instance_type
  image           = var.image
  authorized_keys = [linode_sshkey.relay_deploy.ssh_key]
  tags            = ["relay-master", var.relay_node]
  # StackScript / cloud-init can add `ari` + sudo — production atlanta2 already has `ari`.
}

resource "linode_firewall" "relay_master" {
  label = "${var.relay_node}-relay-master"

  inbound {
    label    = "ssh"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "22"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }
  inbound {
    label    = "relay-http"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "8080"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }
  inbound {
    label    = "relay-git"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "9418"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }
  inbound {
    label    = "http-https"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "80-443"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }
  inbound {
    label    = "piper"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = "5590"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }

  outbound_policy = "ACCEPT"
  linodes         = [linode_instance.atlanta2.id]
}
