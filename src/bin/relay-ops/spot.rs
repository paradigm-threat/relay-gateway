use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct SpotAssignment {
    pub cloudspace: String,
    pub ip: String,
}

pub async fn list_assignments(token: &str) -> Result<Vec<SpotAssignment>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .danger_accept_invalid_certs(true)
        .build()?;
    let urls = [
        "https://spot.rackspace.com/apis/ngpc.rxt.io/v1/vmcloudspaces",
        "https://spot.rackspace.com/apis/ngpc.rxt.io/v1/virtualmachinecloudspaces",
        "https://api.spot.rackspace.com/apis/ngpc.rxt.io/v1/vmcloudspaces",
    ];
    let mut last_err = None;
    for url in urls {
        match fetch_json(&client, token, url).await {
            Ok(v) => {
                let found = walk_assignments(&v);
                if !found.is_empty() {
                    return Ok(found);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    if let Ok(ns) = std::env::var("RXTSPOT_ORG") {
        let url = format!(
            "https://spot.rackspace.com/apis/ngpc.rxt.io/v1/namespaces/{ns}/vmcloudspaces"
        );
        if let Ok(v) = fetch_json(&client, token, &url).await {
            let found = walk_assignments(&v);
            if !found.is_empty() {
                return Ok(found);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Spot VM API returned no assigned IPs")))
}

async fn fetch_json(client: &reqwest::Client, token: &str, url: &str) -> Result<Value> {
    let resp = client
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("{url} -> {}", resp.status());
    }
    Ok(resp.json().await?)
}

fn walk_assignments(v: &Value) -> Vec<SpotAssignment> {
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out
}

fn walk(v: &Value, cloudspace: &str, out: &mut Vec<SpotAssignment>) {
    match v {
        Value::Object(map) => {
            let name = map
                .get("name")
                .and_then(|x| x.as_str())
                .or_else(|| {
                    map.get("metadata")
                        .and_then(|m| m.get("name"))
                        .and_then(|x| x.as_str())
                })
                .unwrap_or(cloudspace);
            if let Some(ip) = map.get("ipAddress").and_then(|x| x.as_str()) {
                if !ip.is_empty() {
                    out.push(SpotAssignment {
                        cloudspace: name.to_string(),
                        ip: ip.to_string(),
                    });
                }
            }
            for val in map.values() {
                walk(val, name, out);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                walk(item, cloudspace, out);
            }
        }
        _ => {}
    }
}
