# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x (current pre-release) | ✅ |

Once v1.0.0 is released, the two most recent minor versions will receive security fixes.

## Reporting a Vulnerability

**Please do not report security vulnerabilities via public GitHub Issues.**

Use GitHub's built-in **private vulnerability reporting**:

1. Go to the [Security tab](../../security) of this repository
2. Click **"Report a vulnerability"**
3. Fill in the details — affected version, description, reproduction steps, and potential impact

We aim to acknowledge reports within **48 hours** and provide a fix or mitigation within **14 days** for critical issues.

## What to Include

A useful report includes:

- PostgreSQL version and `pg_stream` version
- Minimal reproduction SQL or Rust code
- Description of the unintended behaviour and its security impact
- Whether the vulnerability requires a trusted (superuser) or untrusted role to trigger

## Scope

In-scope:

- SQL injection or privilege escalation via `pgstream.*` functions
- Memory safety issues in the Rust extension code (buffer overflows, use-after-free, etc.)
- Denial-of-service caused by a low-privilege user triggering runaway resource usage
- Information disclosure through change buffers (`pgstream_changes.*`) or monitoring views

Out-of-scope:

- Vulnerabilities in PostgreSQL itself (report to the [PostgreSQL security team](https://www.postgresql.org/support/security/))
- Vulnerabilities in pgrx (report to [pgcentralfoundation/pgrx](https://github.com/pgcentralfoundation/pgrx/security))
- Issues requiring physical access to the database host

## Disclosure Policy

We follow coordinated disclosure. Once a fix is released we will publish a security advisory on GitHub with a CVE if applicable.
