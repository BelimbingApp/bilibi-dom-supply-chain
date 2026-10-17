# Purchasing

Module ID: `supply_chain/purchasing`, crate `bilibi-supply-chain-purchasing`.
A company's suppliers and tax codes and its purchase requests with lines,
approval and an audited status history; a later slice adds the purchase
order lifecycle (its schema is already in place) with amendments and
receipts. Other Supply Chain modules read and write through
this module's facade; none of them reads its tables. This README is the
owner contract; the Bilimbi source it ports is
[`purchasing/docs/README.md`](https://github.com/BelimbingApp/b-dom-supply-chain/blob/main/purchasing/docs/README.md).

## Public API

Resolve `Purchasing` from the registry as `supply_chain/purchasing.purchasing`
(`bilibi_supply_chain_purchasing::SERVICE`) and call it with the sealed
`Scope` you were handed and an explicit company ID. Results are typed
records; the schema is private.

| Function | Capability | Does |
| --- | --- | --- |
| `list_suppliers` | `supply-chain.purchasing.view` | The company's suppliers by code; `active_only` keeps active ones |
| `get_supplier` | `supply-chain.purchasing.view` | One supplier, or `None` |
| `create_supplier` | `supply-chain.purchasing.configuration.manage` | Adds a supplier (`code`, `name`, contact and address facts) |
| `update_supplier` | `supply-chain.purchasing.configuration.manage` | Changes the given details or `active` flag (`SupplierChanges`); the code never changes |
| `list_tax_codes` | `supply-chain.purchasing.view` | The company's tax codes by code: `code`, `name`, `rate` (a percentage), `active` |
| `create_tax_code` | `supply-chain.purchasing.configuration.manage` | Adds a tax code |
| `update_tax_code` | `supply-chain.purchasing.configuration.manage` | Changes the given `name`, `rate` or `active` flag (`TaxCodeChanges`) |
| `list_requests` | `supply-chain.purchasing.view` | Requests with their lines, newest number first; `RequestQuery` takes a status and a `limit` (1 to 500, default 100; outside is `Error::Invalid`) |
| `get_request` | `supply-chain.purchasing.view` | One request with its lines and history (oldest first), or `None` |
| `create_request` | `supply-chain.purchasing.requests.manage` | A draft: `title`, optional `justification` and `needed_on`, and lines |
| `update_request` | `supply-chain.purchasing.requests.manage` | Changes a draft's given fields (`RequestChanges`); `lines`, when given, replaces every line |
| `submit_request` | `supply-chain.purchasing.requests.manage` | Draft to submitted; needs a line |
| `approve_request` | `supply-chain.purchasing.requests.approve` | Submitted to approved, optional reason; not by its submitter |
| `reject_request` | `supply-chain.purchasing.requests.approve` | Submitted to rejected, reason required; not by its submitter |
| `cancel_request` | `supply-chain.purchasing.requests.manage` | Draft, submitted or approved to cancelled, reason required |

The order lifecycle (`create_order`, `create_order_from_request`, submit,
approve, return, send, close, cancel, `amend_order`, `find_order_by_number`,
`get_order_line`) and `record_receipt`/`reverse_receipt` for Receiving come
in the next slice.

A write answers `Outcome::Saved(record)`, `Outcome::Refused(refusal)` or
`Outcome::CompanyArchived`. `Refusal` is every reason short of
authorization a page names: `Invalid` (one `FieldError` per field, in
Bilimbi's words: "can't be blank", "has already been taken", "has invalid
format", "has more than 4 decimal places", "is invalid"; a line's field
carries its 1-based `line`), `SupplierNotFound`, `TaxCodeNotFound`,
`RequestNotFound`, `InvalidTransition { status, target }`, `NoLines` and
`OwnRequest`. `Refusal::text()` is Bilimbi's reader-facing sentence for
each (`Web.Refusals.text/1`), so the pages do not drift apart in wording. A
missing grant is `Error::Forbidden`; a company the scope cannot see is
`Error::NotFound`.

## Authorization and company scope

Every call authorizes its capability for the scope's signed-in actor and the
target company when it runs, through Core Company's
`organisation::authorize_target` (Bilimbi `authorize_company_target/3`): the
actor's own company, or a sibling company with Core Company's tenant-wide
reach. Writes then take `organisation::require_writable` with the row lock
inside their mutation transaction, so an archived company is refused and one
company's purchasing writes run one at a time; that lock is also what makes
a code unique without a race and, later, keeps a supplier from being
deactivated while an order names it and the gapless numbering safe. Every
write records its actor and any impersonator from the scope through
`MutationBoundary`; no function takes an actor or actor ID. The Tenant Owner
role holds every capability:

| Capability | Verb |
| --- | --- |
| `supply-chain.purchasing.view` | view |
| `supply-chain.purchasing.configuration.manage` | manage |
| `supply-chain.purchasing.requests.manage` | manage |
| `supply-chain.purchasing.requests.approve` | approve |
| `supply-chain.purchasing.orders.manage` | manage |
| `supply-chain.purchasing.orders.approve` | approve |
| `supply-chain.purchasing.orders.receive` | receive |

The Authz domain is `supply-chain` (a capability segment may not carry an
underscore); the module declares the verbs `approve` and `receive`.

## Suppliers and tax codes

A supplier is a party the company buys from: a `code` unique in the company
and fixed for good (at most 50 characters), a `name` (at most 200), and
optional contact and address facts (`contact_name`, `email`, `phone`,
`address_line1` to `address_line3`, `locality`, `postcode`, `country_iso`,
`admin1_code`) shaped like Core Address so a later shared party record is a
copy, not a translation. `country_iso` is stored upper-cased and must be two
letters; `email` must have one `@` with no whitespace. A supplier is
deactivated, never deleted.

A tax code is a company's configuration: a `code` (at most 20 characters),
a `name` (at most 120) and a `rate` in percent from 0 to 100 with up to
four decimal places, refused rather than rounded when finer. An order line
that names a tax code snapshots its rate, so changing a rate affects future
lines only. The code ships no supplier, tax code, unit or currency: all are a
company's data. `update_supplier` and `update_tax_code` change only the
fields given, by the same rules; a blank optional text clears it. An
inactive supplier or tax code takes no new orders, and its history stays.

## Purchase requests

A request is a numbered ask to buy: a `title` (at most 200 characters), an
optional `justification` (1000) and `needed_on` date (`YYYY-MM-DD`), and
lines, each a `description` (500), an optional `external_item_ref` (100),
a `quantity` above zero with up to four decimal places, and a `unit` (20).
The database numbers each company's requests 1, 2, 3, ... without gaps.

| From | To | Function | Who |
| --- | --- | --- | --- |
| (new) | draft | `create_request` | requests.manage |
| draft | draft | `update_request` | requests.manage |
| draft | submitted | `submit_request` (at least one line) | requests.manage |
| submitted | approved | `approve_request` (optional reason) | requests.approve, not the submitter |
| submitted | rejected | `reject_request` (reason) | requests.approve, not the submitter |
| draft, submitted, approved | cancelled | `cancel_request` (reason) | requests.manage |

Every creation and transition appends a history row (`RequestEvent`) with
the actor, any impersonator, the reason and the time the database recorded
it, in the same mutation as the change. Only a draft changes; a request in
another status is refused as `InvalidTransition`.

## Database

Migration `supply-chain-purchasing-001-suppliers-tax-codes` creates
`supply_chain_purchasing_suppliers` and `supply_chain_purchasing_tax_codes`
as Bilimbi's schema leaves them, with the `(id, company_id)` unique pairs
the request and order tables reference through composite foreign keys.
Migration `supply-chain-purchasing-002-requests-orders` creates the
`requests`, `request_lines`, `request_events`, `orders`,
`order_amendments`, `order_lines` and `order_events` tables (all prefixed
`supply_chain_purchasing_`) at the end state of Bilimbi's two migrations:
the database numbers requests, orders and amendments without gaps; a
request or order changes only by an allowed status transition once it
leaves draft, and its lines are then frozen (an order's change only by an
amendment or a receipt, and a received quantity may exceed the ordered
one); history rows and amendments are append-only; and a deferred
constraint trigger refuses a commit that created, transitioned or amended a
request or order without its history or amendment row.
PostgreSQL enforces the rules the facade checks, so no other writer can
break them: triggers refuse DELETE and TRUNCATE on both tables and refuse a
change to a row's company or code; rates are checked between 0 and 100 and
country codes against `^[A-Z]{2}$`. The two trigger functions
(`supply_chain_purchasing_refuse_change`, `_keep_identity`) are shared by
every later Purchasing table. Each later schema change is a new numbered
migration; an applied file is never edited.

## Seams

- **Receiving** (`supply_chain/receiving`, the next module of this Domain)
  depends on this module through the descriptor and the Cargo manifest, as
  Bilimbi does, and calls the facade's `record_receipt`/`reverse_receipt`
  under `supply-chain.purchasing.orders.receive` once they exist.
- **Finance payables** matched a bill line to an order line through this
  module in Bilimbi. Bilibi Domains do not reach across containers: no port,
  dependency or SQL reference is added here. `TODO(domain-events)`: the
  order-line facts Payables needs will be published through Base domain
  events when that Platform slice lands.
- **Factory** inventory is reached only through a line's
  `external_item_ref`, a free reference the company fills from any
  catalogue; Purchasing never resolves it.

## Pages and menu

The Supply chain menu (`supply_chain`, order 55) offers Purchase requests
(`/supply-chain/purchase-requests`), Purchase orders
(`/supply-chain/purchase-orders`) and Suppliers (`/supply-chain/suppliers`)
to accounts holding `supply-chain.purchasing.view`; the root stays hidden
from everyone else. The pages themselves are later slices: each will read
the capability again for every write-shaped event, and the facade checks the
capability again when the event runs.
