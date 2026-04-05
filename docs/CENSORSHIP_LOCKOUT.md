# crates.io Censorship & Lockout Log

Observed: 2026-04-05

## Summary

A crate named `relay-server` already exists on crates.io and appears to be a dependency-confusion placeholder.
This blocked publishing this project under the exact crate name `relay-server`.

## Why this was notable

The existing crate was published shortly before we were preparing to publish this project's crate identity publicly.
The published description explicitly states dependency-confusion research intent.

## Verified crates.io metadata

Source: crates.io API `GET /api/v1/crates/relay-server`

- name: `relay-server`
- max_version: `0.0.1`
- created_at: `2026-03-24T23:37:45.903630Z`
- updated_at: `2026-03-24T23:37:45.903630Z`
- description:
  - `Security research package — dependency confusion proof of concept. This is a harmless placeholder registered as part of authorized bug bounty research. Contact: security researcher via HackerOne.`
- repository: `https://github.com/nickel-lang/nickel`
- downloads: `12`
- recent_downloads: `12`
- versions:
  - `0.0.1` (not yanked), license `MIT`

## Verified owner data

Source: crates.io API `GET /api/v1/crates/relay-server/owners`

- owner login: `jackpas23`
- owner display name: `JackintheD`
- owner profile URL: `https://github.com/jackpas23`
- crates.io user id: `400763`

## Additional index context

`cargo search relay-server --limit 10` returns:

- `relay-server = "0.0.1"` with dependency-confusion placeholder description
- similarly named crates unrelated to this repo (for example `mpc-relay-server`, `polysig-relay-server`)

## Reproducible commands

Run from any machine with `curl`, `jq`, and Cargo:

```bash
curl -fsSL -H "User-Agent: relay-release-check/1.0" \
  "https://crates.io/api/v1/crates/relay-server" | jq

curl -fsSL -H "User-Agent: relay-release-check/1.0" \
  "https://crates.io/api/v1/crates/relay-server/owners" | jq

cargo search relay-server --limit 10
```

## Current implication for this repository

Publishing this project to crates.io under the exact name `relay-server` is not currently possible unless ownership changes on crates.io.

Practical options:

1. Publish under a new crate name (for example `clevertree-relay-server`).
2. Contact current owner and request transfer or collaborator access.
3. Keep distributing via tarball/install scripts only.

---

# crates.io Access Lockout: GitHub-Only Login

Observed: 2026-04-05

## Summary

As of 2026, **crates.io requires a GitHub account to log in**. There is no alternative login provider
(no email/password, no GitLab OAuth, no other OAuth provider) implemented, despite multi-year
community discussions requesting alternatives.

Because this project is hosted on GitLab and does not maintain a GitHub identity, **all previously
published crates are now effectively frozen**. No updates, yanks, ownership transfers, or metadata
changes can be made to any library already on crates.io.

## Affected published crates

All crates published under this GitLab identity are effectively read-only until crates.io implements
an alternative login provider:

- `relay-gateway` (pending first publish — may not be publishable at all)
- Any other crates previously published — no further versions, yanks, or deprecation notices can be issued

## Technical details

- crates.io login: `https://crates.io/login` — redirects exclusively to GitHub OAuth
- `cargo login` requires a crates.io API token, which can only be generated after logging in via GitHub
- No workaround exists short of creating or borrowing a GitHub account

---

# The Systemic Issue: Centralized Control of Rust Distribution

Observed: 2026-04-05

## The Chokepoint

crates.io represents a single point of failure and control for the entire Rust ecosystem. By requiring GitHub login — a Microsoft-owned platform — the Rust project has effectively handed over authentication and authorization to a single corporate entity.

**This is not accidental.** This is a fundamental architectural decision that creates a dependency on GitHub/Microsoft for the continued operation of Rust-based projects worldwide.

## What Happens When GitHub Suspends an Account

As of April 2026, GitHub is conducting what appears to be automated, large-scale account suspensions with:

- **No warning given** before suspension
- **No explanation provided** for why the account was suspended
- **No apparent human review** of the decision
- **No clear appeal process** that actually resolves the issue

When a GitHub account is suspended:

1. **crates.io access is immediately revoked** — no alternative login method exists
2. **All published crates become frozen** — no new versions can be published
3. **No maintenance is possible** — security patches cannot be deployed
4. **Yanked versions cannot be un-yanked** — broken releases remain permanently available
5. **Ownership cannot be transferred** — affected users cannot pass maintainance to others
6. **Metadata cannot be updated** — incorrect links, descriptions, and repository URLs remain permanently

This creates a scenario where **a developer loses control of their published work indefinitely** — not due to any action on crates.io, but due to an upstream GitHub policy they have no transparency into and cannot appeal.

## The Rust Revolution vs. The Microsoft Chokehold

The Rust language was framed as a decentralized, community-driven project emphasizing safety, reliability, and independence from corporate control. Yet the entire package distribution system — the lifeblood of any programming ecosystem — is gatekept by a single corporation.

**Key vulnerabilities of crates.io's current architecture:**

### 1. **Single Point of Authentication Failure**
If GitHub's OAuth system is compromised, misconfigured, or abused, the entire Rust crate ecosystem becomes inaccessible for publishing. A GitHub outage = a Rust publishing outage.

### 2. **Arbitrary Enforcement Without Transparency**
Microsoft/GitHub can:
- Change Terms of Service unilaterally
- Suspend accounts for any reason or no reason stated
- Delete crates or transfer ownership without consent
- Block access by geography, company, or ideology
- Enforce policies that conflict with open-source values

...and the Rust community has **no recourse** because the decision-making is entirely opaque and delegated to a commercial platform.

### 3. **Escalating Compliance Requirements**
As GitHub implements increasingly aggressive content moderation, account verification, and surveillance (required by corporate policy, government pressure, or advertiser demands), publishing crates becomes contingent on:
- Agreeing to whatever terms Microsoft sets
- Passing identity verification or metadata checks
- Complying with automated systems that may be incorrect or discriminatory

### 4. **Corporate Prerogatives Override Community Interests**
GitHub/Microsoft's business interests (not safety, not open source, not community) drive decisions:
- Account suspension sweeps that may be collateral damage
- API changes that break tooling
- Pricing changes that affect CI/CD for crates
- Data retention and privacy policies set by corporate legal

## What This Means for Rust Developers

As a Rust developer in 2026, you accept the following preconditions when you publish to crates.io:

1. **Your GitHub account is your crates.io identity.** If GitHub suspends you, your published work is locked forever.
2. **Your crates are subject to Microsoft's policies,** not Rust's community governance.
3. **You have no legal recourse** if your account is suspended without explanation or appeal.
4. **Your crates can be deleted, yanked, or transferred** by Microsoft at any time, for any reason.
5. **You are building on a platform you do not control** and that actively discourages users from using alternative platforms (no email/password, no GitLab auth, no decentralized options).

## The Irony

**The Rust language preaches:**
- Memory safety
- Ownership and borrowing semantics
- Preventing misuse through compile-time guarantees
- Independence from runtime assumptions

**Yet its package manager enforces:**
- Dependency on GitHub's OAuth system
- No ownership of your published crates
- Absolute trust in a corporate platform's good behavior
- Inability to self-host or migrate

## Alternative Future

As of 2026, Rust's RFC 2141 standard supports **alternative registries** that do NOT require GitHub:

- GitLab Package Registry (GitLab OAuth, Personal Access Tokens)
- Self-hosted registries using Git indices
- Artifactory (custom OAuth, SAML, LDAP)
- Fully air-gapped corporate registries

**The Rust ecosystem should:**
1. Strongly encourage use of alternative registries for critical projects
2. Implement transparent governance for crates.io that is NOT delegated to a single corporation
3. Require multi-signature verification and decentralized validation
4. Sunset the GitHub-only authentication requirement
5. Support a decentralized package registry model (similar to how Git itself is decentralized)

Until these changes happen, **publishing to crates.io is publishing to a platform you do not own, controlled by an entity that can revoke your access without notice or appeal.**

This is not "open source." This is platform dependence.

## Status of community requests

GitHub-alternative login has been discussed in the Rust community and crates.io issue tracker for
years. As of 2026, no alternative provider has been implemented. The dependency on GitHub concentrates
control of the Rust ecosystem's primary package registry under a single corporate platform (Microsoft /
GitHub).

## Practical implications

| Action | Status |
|--------|--------|
| `cargo publish` new version | **BLOCKED** — no API token obtainable |
| Yank a broken release | **BLOCKED** |
| Transfer crate ownership | **BLOCKED** |
| Add/remove owners | **BLOCKED** |
| Update crate metadata (description, links) | **BLOCKED** |

## Options

1. **Accept lockout** — distribute via tarball, install scripts, or GitLab Package Registry only.
2. **Create a throwaway GitHub account** — solely for the purpose of obtaining a crates.io token. Ethically and practically undesirable.
3. **Watch for crates.io policy changes** — follow `https://github.com/rust-lang/crates.io/issues` for any alternative login implementation.
4. **Publish to an alternative registry** — e.g. a self-hosted Cargo registry, or `cargo add` from a git source.
