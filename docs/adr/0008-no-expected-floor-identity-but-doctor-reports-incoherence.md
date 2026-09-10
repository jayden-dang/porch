# 8. Porch holds no expected floor identity, but `doctor` reports an incoherent pair

Framing proposed that porch pin an expected identity for the floor and refuse a sibling that does
not match, as enforcement of ARCH-9 and ARCH-12. It is not enforcement of either. ARCH-12 forbids
substituting *an external judgment producer* for the floor; ARCH-9 forbids *vendoring or wrapping
a third-party CLI as the engine*; and `docs/specs/2026-09-02-mandatory-floor/requirements.md:283`
states that FLOOR-9.2's no-redirect guarantee "covers configuration and environment only; it does
not extend to an owner who replaces the installed `porch` or `porch-quality` binaries." Binding
that case would extend a guarantee a shipped spec bounded on purpose, which is why it needed this
ADR rather than a wave arriving at it sideways.

**Declined: a gate-time refusal, and an expected identity in the durable descriptor.** The threat
model does not carry it. An owner who can overwrite `~/.cargo/bin/porch-quality` can equally
overwrite `~/.cargo/bin/porch`, so an identity check inside porch defends against exactly one
attacker — one who can write a file but not its neighbour in the same directory under the same
permissions. Meanwhile a trust-on-first-use pin would fire on every legitimate
`cargo install porch --locked --force`, which is the documented upgrade, training the operator to
clear it. That is the failure mode `ESCAPE` rejected its runner-up predicate for. FLOOR-9.2's
bound stands as written, and porch's guarantee remains about redirection, not replacement.

**Accepted instead: `doctor` reports an incoherent pair.** Declining outright would leave a
reachable, silent wrong-floor case that ADR-0006 does not drain — `install.sh` installs the floor
only when the release artifact exists, and the dogfood loop copies a single file — so a stale
sibling can sit next to a much newer `porch` while `doctor` prints `[ok]`. Once one package owns
the name, both files come from one build and their version strings are identical by construction,
so a cross-version stale sibling is detectable by comparing strings. `doctor` escalates from
report to warning on a mismatch, using the arm it already has for the replaced-launch condition.
This is blind to same-version substitution, which is precisely the case porch declines to defend,
and it changes nothing about what is spawned, so FLOOR-9.2 is untouched and no invariant moves.

That requires giving the floor a version flag, which `ONEBIN-3.3` blocked. The attribution behind
that block was wrong and is corrected here: `ONEBIN-7.2`'s descriptor hazard only fires if the
version enters `ProducerDescriptor.reported_version`. A flag that `doctor` reads and the
descriptor ignores is inert, so the audit contract stays closed and MILE-2's read path is not
touched. `tripwire_a_foreign_executable_of_the_right_name_is_accepted_as_the_floor` keeps
pinning the declined half.
