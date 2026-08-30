# 1. Review inference is inherited from the operator's harness

Porch's native review needs model access, and requiring its own provider account would
put a second credential and a separately billed token path between an operator and their
first gated push. Porch therefore inherits the harness's engine selection, credentials,
and runtime availability — never its session, conversation, memory, or session id — and
spends the harness's inference budget rather than its own. The cost is that porch cannot
enforce that the reviewing model differs from the writing one, which is accepted:
independence here is context and process isolation against anchoring, not vendor
diversity (see **ARCH-3**, **ARCH-11**).
