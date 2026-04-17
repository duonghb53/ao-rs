# 7.5 github enterprise remote parsing

Status: done (#110)

## Decision

Make `parse_github_remote` **host-agnostic** — accept any hostname in
the three GitHub-shaped URL forms rather than hard-coding `github.com`.
Route-to-host is `gh`'s job (its own auth config / `GH_HOST`), so the
parser only needs to extract `owner/repo`. This mirrors what the
GitLab SCM plugin already does (`parse_gitlab_remote`) and matches
ao-ts, which never parses remotes at all — it trusts the
`ProjectConfig.repo` field.

The safety rule (reject extra path segments beyond `owner/repo`) is
preserved: exotic GHE path prefixes like `https://ghe/orgs/.../owner/repo`
still fail closed. Users with those layouts must set
`projects.<id>.repo` explicitly in `ao-rs.yaml`, which `resolve_pr`
already honors.

## What landed

- `crates/plugins/scm-github/src/lib.rs`
  - `parse_github_remote` now accepts any `<host>` for all three URL
    shapes: `https://<host>/owner/repo[.git]`,
    `git@<host>:owner/repo[.git]`, and
    `ssh://git@<host>/owner/repo[.git]`. Empty hosts and missing paths
    are rejected.
  - Updated the `discover_origin` doc comment to list GHE URLs
    alongside github.com.
  - New unit tests covering GHE HTTPS/SSH acceptance and extra-segment
    rejection on GHE hosts. The previous `rejects_non_github` test
    (which asserted `gitlab.com` URLs fail) was replaced with
    `rejects_non_url_inputs` — the parser is now host-agnostic, so a
    plain `https://gitlab.com/...` URL *does* parse; AutoScm's
    GitLab-first fallback handles the routing.

## Why no explicit-config-override plumbing

`ProjectConfig.repo` is already required and validated (`ao-core`
config.rs `validate_projects`), and `resolve_pr` already reads it.
The only place that auto-detects repo from the workspace remote is
`detect_pr` discovery, which now handles the common GHE shapes
directly. Threading `ProjectConfig.repo` into `detect_pr` would
require either (a) adding repo to `Session` (breaks persistence) or
(b) widening the `Scm::detect_pr` signature (breaks the plugin trait).
Neither is justified by a rare exotic-path-prefix edge case — users
in that position can set `projects.<id>.repo` and use
`resolve_pr`-based flows.

## Parity status

- GitHub-shaped URL parsing is now on feature parity with the GitLab
  plugin.
- ao-ts parity: ao-ts relies on `projectRepo` string without parsing
  remotes at all, so this change brings ao-rs closer by being more
  permissive with host.

## Follow-ups

If a user reports an exotic GHE path layout (e.g. `/orgs/<org>/<repo>`)
that a host-agnostic parse still can't handle, the right fix is to
plumb `ProjectConfig.repo` into `detect_pr` — likely via a thin
`ScmContext` wrapper — rather than making the parser progressively
looser.
