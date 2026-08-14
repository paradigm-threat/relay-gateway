use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;

pub struct Vercel {
    token: String,
    team_id: Option<String>,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct RecordList {
    records: Option<Vec<DnsRecord>>,
}

#[derive(Debug, Deserialize)]
struct DnsRecord {
    id: Option<String>,
    uid: Option<String>,
    name: Option<String>,
    #[serde(rename = "type")]
    rtype: Option<String>,
    value: Option<String>,
}

impl DnsRecord {
    fn id(&self) -> Option<&str> {
        self.id.as_deref().or(self.uid.as_deref())
    }
}

impl Vercel {
    pub fn new(token: &str, team_id: Option<&str>) -> Result<Self> {
        Ok(Self {
            token: token.to_string(),
            team_id: team_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()?,
        })
    }

    fn team_q(&self) -> String {
        match &self.team_id {
            Some(id) => format!("?teamId={id}"),
            None => String::new(),
        }
    }

    fn team_and(&self) -> String {
        match &self.team_id {
            Some(id) => format!("?teamId={id}&"),
            None => "?".into(),
        }
    }

    pub async fn point_a(&self, fqdn: &str, ip: &str) -> Result<String> {
        let (name, domain) = split_fqdn(fqdn)?;
        let list = self.list_records(&domain, "A").await?;
        let mut log = Vec::new();
        for rec in list.iter().filter(|r| r.name.as_deref() == Some(name) && r.rtype.as_deref() == Some("A")) {
            if rec.value.as_deref() == Some(ip) {
                log.push(format!("{fqdn} already -> {ip}"));
                return Ok(log.join("\n"));
            }
            if let Some(id) = rec.id() {
                self.delete_record(&domain, id).await?;
                log.push(format!("deleted stale {fqdn} A {}", rec.value.as_deref().unwrap_or("?")));
            }
        }
        self.create_record(&domain, name, ip, "A").await?;
        log.push(format!("created {fqdn} -> {ip}"));
        Ok(log.join("\n"))
    }

    pub async fn reconcile_fleet_a(&self, fleet_fqdn: &str, desired: &[String]) -> Result<String> {
        if desired.is_empty() {
            return Err(anyhow!("refusing to empty {fleet_fqdn} A set"));
        }
        let (name, domain) = split_fqdn(fleet_fqdn)?;
        let list = self.list_records(&domain, "A").await?;
        let current: Vec<&DnsRecord> = list
            .iter()
            .filter(|r| r.name.as_deref() == Some(name) && r.rtype.as_deref() == Some("A"))
            .collect();
        let mut log = Vec::new();
        for rec in &current {
            let val = rec.value.as_deref().unwrap_or("");
            if !desired.iter().any(|d| d == val) {
                if let Some(id) = rec.id() {
                    self.delete_record(&domain, id).await?;
                    log.push(format!("deleted stale {fleet_fqdn} A {val}"));
                }
            }
        }
        let current_vals: Vec<String> = current
            .iter()
            .filter_map(|r| r.value.clone())
            .collect();
        for ip in desired {
            if !current_vals.iter().any(|c| c == ip) {
                self.create_record(&domain, name, ip, "A").await?;
                log.push(format!("created {fleet_fqdn} A {ip}"));
            }
        }
        if log.is_empty() {
            log.push(format!("{fleet_fqdn} A set already matches"));
        }
        Ok(log.join("\n"))
    }

    async fn list_records(&self, domain: &str, rtype: &str) -> Result<Vec<DnsRecord>> {
        let url = format!(
            "https://api.vercel.com/v4/domains/{domain}/records{}type={rtype}&limit=100",
            self.team_and()
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .with_context(|| url.clone())?;
        if !resp.status().is_success() {
            return Err(anyhow!("vercel list {}: {}", domain, resp.status()));
        }
        let body: RecordList = resp.json().await?;
        Ok(body.records.unwrap_or_default())
    }

    async fn delete_record(&self, domain: &str, id: &str) -> Result<()> {
        let url = format!(
            "https://api.vercel.com/v4/domains/{domain}/records/{id}{}",
            self.team_q()
        );
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("vercel delete {id}: {}", resp.status()));
        }
        Ok(())
    }

    async fn create_record(&self, domain: &str, name: &str, value: &str, rtype: &str) -> Result<()> {
        let url = format!(
            "https://api.vercel.com/v4/domains/{domain}/records{}",
            self.team_q()
        );
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&json!({ "name": name, "value": value, "type": rtype, "ttl": 60 }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let t = resp.text().await.unwrap_or_default();
            return Err(anyhow!("vercel create {name}: {t}"));
        }
        Ok(())
    }
}

fn split_fqdn(fqdn: &str) -> Result<(&str, &str)> {
    let (name, domain) = fqdn
        .split_once('.')
        .ok_or_else(|| anyhow!("FQDN must be a subdomain: {fqdn}"))?;
    if name.is_empty() || domain.is_empty() {
        return Err(anyhow!("FQDN must be a subdomain: {fqdn}"));
    }
    Ok((name, domain))
}
