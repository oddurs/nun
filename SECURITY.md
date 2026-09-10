# Security policy

## Supported versions

nun is pre-release. Only the tip of `main` receives fixes. Once there is a
tagged release, this table will list the versions that still get them.

| Version | Supported |
|---|---|
| `main` | yes |
| everything else | no |

## Reporting a vulnerability

Report privately through a
[GitHub Security Advisory](https://github.com/oddurs/nun/security/advisories/new).
Do not open a public issue, and do not disclose the details anywhere else until
a fix has shipped.

If GitHub advisories are not workable for you, email <oddurs@gmail.com> with
`nun security` in the subject.

Please include what an attacker gains, the steps to reproduce it, the affected
commit, and your terminal and platform.

## What to expect

- **Within 3 days** — acknowledgement that the report arrived.
- **Within 10 days** — an assessment: whether it is a vulnerability, its
  severity, and a rough fix timeline.
- **On release** — a published advisory crediting you, unless you would rather
  stay anonymous.

## Scope

nun reads files you point it at, runs the language servers you configure, and
spawns the shell you configure in its terminal panel. Reports involving a
language server or shell that you deliberately configured are usually a matter
for that program rather than nun.

Things that are firmly in scope: escaping the sandbox around untrusted file
content, anything a malicious repository can do to you through a project-local
`.nun.toml`, terminal escape-sequence injection through file content or LSP
responses, and path traversal in the file tree.
