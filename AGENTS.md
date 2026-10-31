# Project agent memory

This repository is the Supply Chain Domain for Bilibi. It builds only when
mounted in a Bilibi checkout at `apps/domains/supply_chain` (see `README.md`).
Read Bilibi's `apps/domains/AGENTS.md`, `apps/base/ui/AGENTS.md` and
`apps/base/ports/README.md` in that checkout first; they govern this code and
are not copied here. Entries below are this repository's own rules: short
"use this, not that" lines with the reason.

- Correct entries that work proves wrong; add new ones only by deliberate maintainer choice, never as routine task output.

## Architecture

- **No production users yet (until 2027-01-01).** Never build data imports, backfills, conversion of existing rows, or compatibility tables, columns or code paths; replace or drop them in a new migration. Development and walkthrough databases are disposable. After 2027-01-01 this rule no longer applies: ask the owner whether to extend it before relying on it.
- Expose one small facade per module (`purchasing/src/lib.rs`: `Purchasing`; `receiving/src/lib.rs`: `Receiving`, each with its record/input types, `Outcome` and capability keys) and keep the schema, SQL, validation and helpers in private modules, not `pub`: a deep module is the one place a rule lives, so a caller cannot bypass the capability check, the company lock or the validation by reaching a lower layer.
- Never add a Cargo dependency, descriptor edge, port call or SQL reference to another Domain's container (Finance, Factory): each couples the two containers' composition or schema. Cross-domain consumers copy Base domain events (`apps/base/events`); Purchasing and Receiving document their versioned shapes in their READMEs. Keep the legacy `supply_chain/purchasing.order_line` port compatible for optional same-domain callers, not as a cross-domain seam, because consumers keep their own serde types.
- Within this Domain, depend on a sibling module only through its facade, declared in both the descriptor's `dependencies` and the Cargo manifest, only where Bilimbi has the same edge (Receiving → Purchasing), and never in a cycle: the registry orders modules by their declared edges. Receiving calls Purchasing's `get_order_on` and `record_receipt_in`/`reverse_receipt_in` on its own mutation transaction, never the pool-owning wrappers, because receipt, order, audit and outbox must commit together; roll back every refusal/error. Receiving's versioned event shape is in `receiving/README.md`.
- Use only meta-terms in Domain code: supplier, tax code, purchase request, purchase order, line, unit, amendment, receipt. A company's suppliers, tax codes and rates, units, currencies and item references are its data, never a code identifier, constant, inclusion list or default; the request and order statuses are the one fixed classification.

## Platform contracts

- Keep Bilimbi's IDs, keys and names verbatim: module IDs `supply_chain/purchasing` and `supply_chain/receiving`, tables under their `supply_chain_<module>_*` prefix, capabilities under the Authz domain `supply-chain` (hyphen: a capability segment may not carry an underscore, while the container ID and table prefix stay `supply_chain`), the menu IDs and labels, flash texts and confirmations. They are persisted data and the parity reference.
- Declare every verb a capability key ends in that Base does not already declare (`approve`, `receive` in `purchasing/src/contributions.rs`), not only the ones Bilimbi listed: Bilibi's vocabulary refuses a key whose last segment no module declared, and merges verbs across modules, so declaring one Base already has is harmless.
- Name a migration `supply-chain-<module>-NNN-<slug>` and add a new file for every schema change, never editing an applied one: the ledger records each file's SHA-256 and refuses a changed applied file. `001` creates the two trigger functions (`supply_chain_purchasing_refuse_change`, `_keep_identity`) every later Purchasing table reuses.
- Authorize facade calls through Core Company's `bilibi_company::authorize_work_target` (Bilimbi `authorize_company_target/3`) and take `bilibi_company::require_writable(.., lock: true)` inside every write's mutation transaction, as `Purchasing::begin` does: the lock keeps codes unique, reference facts stable and numbering gapless. Agent preview uses Company's equivalent object-safe `Directory::target` and the narrow same-connection lock documented in `purchasing/README.md`, because `OperationClient` cannot enter the generic writable guard; never open a second pooled connection while holding the plan's locks. Never take an actor or actor ID as input; derive both actor and impersonator from the sealed `Scope`.
- Make every write through `MutationBoundary` (`tx.mutate` returning each affected row as a `Change`), never a bare transaction: Bilimbi captured Ecto writes implicitly, Bilibi must be told, and a write that reports no `Change` cannot commit. Decide refusals (a taken code, a broken rule, a transition that no longer applies) with `tx.sql()` before `mutate`, and return them as `Outcome::Refused(Refusal)`, never as an `Error`; `Refusal::text()` holds Bilimbi's reader-facing sentence, so pages never word a refusal themselves.
- Give a `Change` for a created or deleted row `json!({})` on the absent side, never `Value::Null`: Base Audit names the event `created`, `updated` or `deleted` from which side is an empty object, so `Null` records every write as `updated`.
- Bind and read a `date` column as text (`$n::text::date`, `to_char(col,'YYYY-MM-DD')`) and check it with `validate::date`: the workspace's `tokio-postgres` has no date feature, and a Domain must not turn one on for every Platform crate.
- Draw every list with the shared `FlexTable` and the table features Bilimbi enables on that page, never the plain `Table`; a page checks its capability again on every write-shaped event and the facade checks once more. Domains own no CSS or JavaScript: a class the Platform stylesheet lacks is a Platform change made with nothing mounted.
- Keep `fixtures/walkthrough` the only fixture: an example-only crate that migrates a disposable database and creates records through the facades, never seed data, demo users or demo pages. It stays a workspace member through `purchasing`'s `[dev-dependencies]` edge (Cargo adds path dependencies of members, and ignores one without a lib target, which is why it has `lib.rs` and a thin `main.rs`); never move it to `[dependencies]`.
- Never commit a lock file, a root `Cargo.toml` or a `[workspace]` here, and never commit the Platform's composition files or `Cargo.lock` from the mounted checkout: discovery refuses the first, and `discover-modules.py --check` the second.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
