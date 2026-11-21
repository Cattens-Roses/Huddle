# Security policy

## Supported versions

Security fixes are applied to the current `main` branch and the latest published
release of Huddle. Older releases are generally not patched separately; please
upgrade when a fixed release is available.

| Version | Supported |
| --- | --- |
| Latest release / `main` | Yes |
| Older releases | No |

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Prefer one of these private channels:

1. **GitHub Security Advisories** - [Report a vulnerability](https://github.com/Cattens-Roses/Huddle/security/advisories/new)
2. Contact the maintainers privately via GitHub

Include as much detail as you can:

- Affected version or commit
- Impact (e.g. credential leak, unexpected network behavior, local file access)
- Steps to reproduce or a proof of concept
- Whether you plan to disclose publicly and on what timeline

## What to expect

- We aim to acknowledge reports within a few days.
- We will discuss severity, a fix plan, and coordinated disclosure when appropriate.
- Credit is welcome unless you ask to remain anonymous.

## Scope notes

Huddle is a Matrix client. It talks to **your** homeserver and related Matrix
services you configure. Reports about Matrix server software itself should go to
those projects, not this repo. Client bugs that mishandle credentials, session
keys, media caches, or local files in this app are in scope here.
