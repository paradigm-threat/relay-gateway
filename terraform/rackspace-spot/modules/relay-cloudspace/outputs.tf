output "cloudspace_name" {
  value = spot_cloudspace.cs.cloudspace_name
}

output "region" {
  value = spot_cloudspace.cs.region
}

output "server_class" {
  value = spot_spotnodepool.pool.server_class
}

output "kubeconfig_path" {
  value = local_file.kubeconfig.filename
}
