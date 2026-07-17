//! Let's Encrypt HTTP-01 via `instant-acme` for park-plugin domains.

use super::fanout::{spawn_fanout_challenge, spawn_fanout_cert};
use super::store::{persist_cert_pair, persist_challenge, remove_challenge};
use crate::types::AppState;
use anyhow::{anyhow, Context};
use instant_acme::{
    Account, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder, OrderStatus,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use std::time::Duration;

pub async fn ensure_certs_for_domains(state: &AppState, domains: &[String]) -> anyhow::Result<()> {
    for d in domains {
        issue_one(state, d)
            .await
            .with_context(|| format!("issue cert for {d}"))?;
    }
    Ok(())
}

async fn issue_one(state: &AppState, domain: &str) -> anyhow::Result<()> {
    let contact = std::env::var("RELAY_ACME_EMAIL")
        .or_else(|_| std::env::var("RELAY_TLS_EMAIL"))
        .unwrap_or_else(|_| "mailto:ops@relaygateway.net".into());
    let contact = if contact.starts_with("mailto:") {
        contact
    } else {
        format!("mailto:{contact}")
    };

    let directory = if std::env::var("RELAY_ACME_STAGING").ok().as_deref() == Some("1") {
        LetsEncrypt::Staging.url()
    } else {
        LetsEncrypt::Production.url()
    };

    let (account, _creds) = Account::create(
        &NewAccount {
            contact: &[contact.as_str()],
            terms_of_service_agreed: true,
            only_return_existing: false,
        },
        directory,
        None,
    )
    .await
    .context("create ACME account")?;

    let identifiers = [Identifier::Dns(domain.to_string())];
    let mut order = account
        .new_order(&NewOrder {
            identifiers: &identifiers,
        })
        .await
        .context("new_order")?;

    let authorizations = order.authorizations().await.context("authorizations")?;
    let mut challenge_tokens: Vec<String> = Vec::new();

    for authz in &authorizations {
        let challenge = authz
            .challenges
            .iter()
            .find(|c| c.r#type == ChallengeType::Http01)
            .ok_or_else(|| anyhow!("no HTTP-01 challenge for {domain}"))?;
        let token = challenge.token.clone();
        let key_auth = order.key_authorization(challenge);
        let key_auth = key_auth.as_str().to_string();
        persist_challenge(&token, &key_auth).context("persist challenge")?;
        spawn_fanout_challenge(state, &token, &key_auth);
        challenge_tokens.push(token.clone());
        order
            .set_challenge_ready(&challenge.url)
            .await
            .context("set_challenge_ready")?;
    }

    // Poll order until ready / invalid.
    let mut attempts = 0u32;
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let st = order.refresh().await.context("refresh order")?;
        match st.status {
            OrderStatus::Ready => break,
            OrderStatus::Invalid => {
                for t in &challenge_tokens {
                    remove_challenge(t);
                }
                return Err(anyhow!(
                    "ACME order invalid for {domain}: {:?}",
                    st.error
                ));
            }
            OrderStatus::Pending | OrderStatus::Processing => {
                attempts += 1;
                if attempts > 40 {
                    for t in &challenge_tokens {
                        remove_challenge(t);
                    }
                    return Err(anyhow!("ACME order timed out for {domain}"));
                }
            }
            other => {
                attempts += 1;
                if attempts > 40 {
                    for t in &challenge_tokens {
                        remove_challenge(t);
                    }
                    return Err(anyhow!("ACME unexpected status {other:?} for {domain}"));
                }
            }
        }
    }

    let mut params = CertificateParams::new(vec![domain.to_string()])?;
    params.distinguished_name = DistinguishedName::new();
    let key_pair = KeyPair::generate()?;
    let csr_der = params.serialize_request(&key_pair)?.der().to_vec();

    order
        .finalize(csr_der.as_slice())
        .await
        .context("finalize")?;

    let cert_chain_pem = loop {
        match order.certificate().await.context("certificate")? {
            Some(pem) => break pem,
            None => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    };

    let privkey_pem = key_pair.serialize_pem();
    persist_cert_pair(domain, &cert_chain_pem, &privkey_pem).context("persist cert")?;
    spawn_fanout_cert(state, domain, &cert_chain_pem, &privkey_pem);

    for t in &challenge_tokens {
        remove_challenge(t);
    }

    tracing::info!(%domain, "park_tls: certificate issued");
    Ok(())
}
