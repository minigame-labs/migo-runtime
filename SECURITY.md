# Security Policy

[English](SECURITY.md) | [中文](SECURITY.zh-CN.md)

## Reporting a Vulnerability

We take the security of **Migo** seriously. If you believe you have found a security vulnerability, please report it responsibly.

**Please DO NOT open a public GitHub issue for security vulnerabilities.**

### How to Report (Preferred)

- Open a **Private Security Advisory** on GitHub:
  https://github.com/minigame-labs/migo-runtime/security/advisories/new

### Alternative Contact

- Email: **security@minigame-labs.com**

> If you are unsure whether an issue is security-related, please use a private advisory.

## What to Include

Please include as much of the following as possible:

1. **Description**: What the issue is and where it occurs
2. **Impact**: What an attacker could achieve (e.g., sandbox escape, RCE, data exfiltration)
3. **Reproduction**: Steps to reproduce, PoC, and any relevant logs
4. **Affected Versions**: Which release versions and platforms are impacted
5. **Mitigations / Fix Ideas**: Any suggested fix or workaround (optional)

## Response Targets

We aim to respond on a best-effort basis:

| Stage | Target |
|------|--------|
| Initial response | within 2 business days |
| Status update | within 7 days |
| Fix & release | depends on severity and complexity |

## Severity Guidance (Runtime / SDK)

| Severity | Examples | Target Fix Time |
|----------|----------|-----------------|
| Critical | RCE, sandbox escape, arbitrary code execution via JS bridge | 24–72 hours |
| High | arbitrary file read/write, auth bypass (if applicable), major data exfiltration | ~7 days |
| Medium | limited-scope info leak, DoS with realistic impact | ~30 days |
| Low | hardening improvements, low-impact issues | next release |

## Supported Versions

| Version | Supported |
|---------|-----------|
| Latest release | ✅ Yes |
| Previous minor | ✅ Security fixes only |
| Older versions | ❌ No |

## Security Best Practices for Integrators

When integrating Migo into your application:

1. **Keep Updated**: Use the latest stable release
2. **Validate Inputs**: Sanitize inputs passed into the runtime and bridges
3. **Sandboxing**: Apply OS-level sandboxing/permissions as appropriate
4. **Network Security**: Use HTTPS and validate certificates
5. **Content Trust**: Only load mini-games from trusted sources; verify integrity/signatures if possible

## Acknowledgments

We appreciate security researchers who help keep Migo safe. With your permission, we will acknowledge your contribution in our advisories.
