# bilibi-dom-supply-chain

Supply Chain Domain for [Bilibi](https://github.com/BelimbingApp/bilibi):
buying through to delivery, ported with UI parity from Bilimbi's
[b-dom-supply-chain](https://github.com/BelimbingApp/b-dom-supply-chain). It is
one optional, independently versioned repository that an adopter mounts into a
Bilibi checkout; it does not build on its own.

| Module | Descriptor ID | Crate | Owns |
| --- | --- | --- | --- |
| [`purchasing/`](purchasing/README.md) | `supply_chain/purchasing` | `bilibi-supply-chain-purchasing` | each company's suppliers and tax codes, its purchase requests and its purchase orders with lines, approval, amendments, goods received and an audited status history; the `supply_chain/purchasing.order_line` port; the Suppliers and Purchase requests pages |
| [`receiving/`](receiving/README.md) | `supply_chain/receiving` | `bilibi-supply-chain-receiving` | goods receipt drafts, posting, cancellation and reversal, tolerance, immutable line snapshots, net summaries and durable line events |

Receiving's schema and facade are available; its pages follow in S6. Other Supply Chain modules read and write
purchasing records only through `bilibi_supply_chain_purchasing::Purchasing`,
the module's facade; none reads its tables. Receiving and Purchasing share one mutation transaction when posting or reversing goods, including audit and Base domain events. Cross-domain receipt consumers subscribe to the versioned events documented in `receiving/README.md`; no Receiving port is registered, following the captain's event rule.

## Mount

Clone this repository into a Bilibi checkout at `apps/domains/supply_chain`
(a real nested checkout, never a symlink) and work from the Bilibi root:

```sh
git clone https://github.com/BelimbingApp/bilibi-dom-supply-chain.git apps/domains/supply_chain
python3 scripts/discover-modules.py
cargo run -p bilibi-web -- migrate
```

Bilibi discovers the mounted container from `bilibi.container.toml`; no list
names it. Removing the folder removes Supply Chain's code from the next build
and keeps its data: the migration ledger keeps the applied versions, and a
remount replays nothing. The mount rules are in Bilibi's
`apps/domains/AGENTS.md`. Commit and push only inside this repository; the
Platform's `apps/domains/Cargo.toml`, `apps/domains/src/lib.rs` and
`Cargo.lock` change while mounted and are never committed there.

After migrating, the Supply chain menu offers Purchase requests, Purchase
orders and Suppliers to accounts holding `supply-chain.purchasing.view`; the
Tenant Owner role holds every Purchasing capability.

Approved or sent orders can be amended through the reviewed agent operation `supply_chain.purchasing.order.amend` on `/api/v1` and `/mcp`. It uses the existing facade, requires `supply-chain.purchasing.orders.manage`, and adds no page. The [operation contract](purchasing/README.md#reviewed-agent-amendment) documents replacement lines, the current amendment-count precondition, preview/commit, reapproval and refusals.

## Pages

| Page | Path | Does |
| --- | --- | --- |
| Suppliers | `/supply-chain/suppliers` | the company's suppliers and tax codes in FlexTables, the add forms, Deactivate/Activate |
| Purchase requests | `/supply-chain/purchase-requests` | status tabs, the five-line draft form, submit, approve or reject, cancel (each confirmed) and Create order |

Purchase orders (`/supply-chain/purchase-orders`) is in the menu and comes
with the order pages. Details are in
[`purchasing/README.md`](purchasing/README.md#pages-and-menu).

## Walkthrough

Proofs never use a seeded database. `fixtures/walkthrough` is an example-only
crate: against a freshly created disposable database it migrates the composed
app, installs the Platform walkthrough fixture (its tenants, accounts and
companies), gives `manager@example.test` the Tenant Owner role, and through
the Purchasing facade creates Company 001's first suppliers and tax codes,
changes and deactivates them, walks three purchase requests through
draft, update, submit, approve, reject and cancel, orders the approved one,
prices, submits, returns, approves and sends the order, records and reverses
goods received until it closes, amends a second order, and reads an order
line through the `supply_chain/purchasing.order_line` port (the grant-all
`admin@example.test` decides what the manager submitted). It prints the
refusals beside the writes (a taken code, a broken rule, a missing grant, a
request or order decided by its own submitter, an inactive supplier, an
over-receipt, a transition that no longer applies) and shows PostgreSQL
refusing a change to an approved request, a closed order, their lines or
their history, and a transition without a history row. It also has the
admin submit two more requests for the manager to decide in the browser.

The same fixture also runs the H7 amendment operation through the real registry and plan services: preview leaves business records unchanged, manage-without-view succeeds, header-only changes keep status, line changes require fresh approval, and sent orders can be amended. It shows replay, stale tax facts, revoked permissions and two same-order amendments refused with atomic rollback. Plan codes and session credentials are never printed.

```sh
createdb -h 127.0.0.1 -p 5438 -U bilibi bilibi_sc_walkthrough
export DATABASE_URL=postgres://bilibi:bilibi@127.0.0.1:5438/bilibi_sc_walkthrough
cargo run -p bilibi-supply-chain-walkthrough
APP_ORIGIN=http://localhost:3000 cargo run -p bilibi-web -- run
```

Sign in at <http://localhost:3000> as `manager@example.test` (password
`bilibi-dev`): the sidebar shows the Supply chain menu with its three items.
On Suppliers, add a supplier or tax code, repeat a code (refused), and
deactivate one. On Purchase requests, save a draft with lines, submit it
(confirmed), try to approve it yourself (refused), approve Forklift service
or reject Pallet wrap with a reason, draft an order from an approved
request with an active supplier and a currency, and cancel one; switch the
status tabs, sort a column and try the compact rows.
`viewer@example.test` holds no Purchasing capability: no Supply chain menu,
and both pages answer 403.

## Checks

There are no unit tests. The gate is the Platform's `scripts/domain-ci.sh`,
run from the Bilibi root with this repository mounted and `DATABASE_URL`
naming an empty PostgreSQL 18 database: discovery, the committed CSS, the
mounted `Cargo.lock` pin, `cargo fmt`, `cargo clippy -D warnings`, the locked
release build, `migrate`, `seed-production`, and a boot that answers `/`.

## CI

`.github/workflows/ci.yml` is Bilibi's reference Domain workflow
(`docs/domain-ci/ci.yml`) with the container ID set. It checks out Bilibi at
the revision in [`.github/bilibi-revision`](.github/bilibi-revision), moves
this repository to `apps/domains/supply_chain`, runs `scripts/domain-ci.sh`
and verifies the gates left this repository unchanged. Bump that revision to
build against a newer Platform.

## Naming

Optional Bilibi repositories are named `bilibi-<role>-<id>`: `bilibi-` for
the Platform, `dom-` for a Domain. The mount folder is the container ID with
hyphens as underscores, so `bilibi-dom-supply-chain` mounts at
`apps/domains/supply_chain`, and its crates are
`bilibi-supply-chain-<module>`.

## License

[MIT](LICENSE).
