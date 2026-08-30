# Triage label mapping (agent-facing)

Written by `configure-repo`. Maps canonical triage roles to this repo's actual labels.

All seven labels exist on `jayden-dang/porch`; the mapping is 1:1 with the
canonical role names.

| Canonical role | Label in this repo |
|---|---|
| needs-triage | `needs-triage` |
| needs-info | `needs-info` |
| ready-for-agent | `ready-for-agent` |
| ready-for-human | `ready-for-human` |
| wontfix | `wontfix` |
| bug | `bug` |
| enhancement | `enhancement` |

Rule: every triaged issue carries exactly one category role (bug/enhancement)
and one state role.

Not part of this mapping, but present on the repo: `documentation`, `duplicate`,
`good first issue`, `help wanted`, `invalid`, `question`, `accessibility`. Apply
them freely alongside the canonical roles.
