# bilibi-dom-supply-chain

Supply Chain Domain for [Bilibi](https://github.com/BelimbingApp/bilibi):
buying through to delivery, ported with UI parity from Bilimbi's
[b-dom-supply-chain](https://github.com/BelimbingApp/b-dom-supply-chain). It is
one optional, independently versioned repository that an adopter mounts into a
Bilibi checkout; it does not build on its own.

| Module | Descriptor ID | Crate | Owns |
| --- | --- | --- | --- |
| [`purchasing/`](purchasing/README.md) | `supply_chain/purchasing` | `bilibi-supply-chain-purchasing` | each company's suppliers and tax codes, and (in later slices) its purchase requests and purchase orders with lines, approval, amendments and an audited status history |

Receiving (`supply_chain/receiving`, goods receipts against purchase orders)
follows as a sibling module folder. Other Supply Chain modules read and write
purchasing records only through `bilibi_supply_chain_purchasing::Purchasing`,
the module's facade; none reads its tables.

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

## Walkthrough

Proofs never use a seeded database. `fixtures/walkthrough` is an example-only
crate: against a freshly created disposable database it migrates the composed
app, installs the Platform walkthrough fixture (its tenants, accounts and
companies), gives `manager@example.test` the Tenant Owner role, and creates
Company 001's first suppliers and tax codes through the Purchasing facade,
printing the refusals beside the writes.

```sh
createdb -h 127.0.0.1 -p 5438 -U bilibi bilibi_sc_walkthrough
export DATABASE_URL=postgres://bilibi:bilibi@127.0.0.1:5438/bilibi_sc_walkthrough
cargo run -p bilibi-supply-chain-walkthrough
APP_ORIGIN=http://localhost:3000 cargo run -p bilibi-web -- run
```

Sign in at <http://localhost:3000> as `manager@example.test` (password
`bilibi-dev`): the sidebar shows the Supply chain menu with its three items.
`viewer@example.test` holds no Purchasing capability and sees no Supply chain
menu.

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
