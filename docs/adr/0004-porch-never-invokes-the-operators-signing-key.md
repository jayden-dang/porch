# 4. Porch never invokes the operator's signing key

Porch creates commits at two identity-pinned sites and replays them at three rebase sites, and
none of the five passes a signing override, so all five inherit `commit.gpgsign`. On a signing
host with an interactive signer this hangs without bound: `porch_git::run_c` is a bare
`Command::output()` with no deadline, and `certify_timeout()` covers only the adapter, not the
commit that follows it. Measured, the block survives an eight-second kill; the same argv with
`-c commit.gpgsign=false` completes in 71 ms. The daemon meanwhile keeps answering `ready`,
because runs execute on a per-run thread while `health` is answered from a literal, so the
mitigation the roadmap credited to `DFAULT` does not reach the ordinary case.

The roadmap defended the status quo with "unsigning them would break a remote that requires
signed commits." That premise is inverted. GitHub's rulesets confirm a signature with
`verified_signature?` rather than accepting its mere presence, and `Verified` requires the
committer email to match a verified account address. Porch pins the committer to
`porch@example.com`, an RFC 2606 reserved domain that can never receive a confirmation mail, so
porch's signed commits are structurally `Unverified` and a signed-commits ruleset already rejects
them exactly as it rejects unsigned ones — on the squash path too, since GitHub checks the head
branch's commits. Nothing is being taken away from a consumer who requires signatures; that
consumer is already blocked, by the identity rather than the signature.

Discovery then found a harm running the other way. Because `porch_git::rebase` passes no identity
overrides, the deliver-repair rebase relabels porch's own correction commit to
`committer=<operator> author=porch@example.com` and re-signs it with the operator's key — a
commit GitHub *will* mark `Verified`, carrying the operator's cryptographic identity on content
no human wrote.

**Every git operation porch performs on the operator's behalf passes
`-c commit.gpgsign=false`** — both porch-identity commit sites and all three rebase sites. The
apparent trilemma, that replayed operator commits must stay verifiable, dissolves because that
leg was never real: a rebase destroys the original signature and manufactures a new one, so what
is forfeited is a signature porch fabricated with the operator's key while the operator was
absent, not a signature the operator made. Separately and unconditionally, git plumbing gets a
deadline; that is defect repair with no policy content, and the gate already bounds its review,
fixer, and certify children.

Two consequences are accepted and recorded rather than solved here. Porch cannot deliver to a
consumer that requires verified signatures, and that is now a stated limitation rather than an
unexamined assumption. And `porch@example.com` ships in consumers' permanent history under a
reserved domain; replacing it is a durable-history decision that no milestone currently owns, so
it is carried as a named follow-up rather than settled in passing.
