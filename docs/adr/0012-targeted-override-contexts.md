# ADR 0012: Reject divergent targeted override contexts

Native resolution applies root override ranges to immediate registry dependency
edges using the selected parent version and incoming local binding. Shared raw
metadata cannot carry these replacements: two aliases of one exact package can
match different parent selectors. Evaluate the policy in the existing traversal
and retain actual rule matches alongside original and effective requirements.

Keep the existing exact package identity and lock/linker contracts. Compare the
complete eligible outgoing requirement maps for every reachable incoming binding;
share equivalent contexts and reject divergent contexts before publication,
even when their current candidates would select the same child version. Bound
and deduplicate contexts to terminate cycles. Optional eligibility uses each
binding's effective range and platform-filtered candidates in that traversal.

Creating synthetic identities or new placement contexts would expand the lock,
linker and replay contracts substantially. Silently applying a targeted rule to
all aliases would broaden user intent. This bounded rejection trades some npm
compatibility for preservation of source, alias, peer and verification identity.
Imported replay continues to use recorded selections and manifest freshness;
only deliberate native resolution applies override selection policy.
