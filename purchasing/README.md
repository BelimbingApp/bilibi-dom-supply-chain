# Purchasing

Module ID: `supply_chain/purchasing`, crate `bilibi-supply-chain-purchasing`.
A company's suppliers and tax codes, its purchase requests with lines,
approval and an audited status history, and its purchase orders with
approval, amendments and the goods received against them, and the Suppliers
and Purchase requests pages. Other Supply Chain modules read and write
through this module's facade; none of them reads its tables. This README is the
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
| `list_orders` | `supply-chain.purchasing.view` | Orders with their current lines and totals, newest number first; `OrderQuery` takes a status, a `supplier_id` and a `limit` (1 to 500, default 100) |
| `get_order` | `supply-chain.purchasing.view` | One order with supplier, current lines, totals, history and amendments, or `None` |
| `find_order_by_number` | `supply-chain.purchasing.view` | The order with that number in the company, as `get_order` returns it |
| `get_order_line` | `supply-chain.purchasing.view` | One current or superseded line with its order's ID, number, status, supplier and currency (`OrderLineFacts`), or `None` |
| `create_order` | `supply-chain.purchasing.orders.manage` | A draft: `supplier_id`, `currency_code`, optional `reference`, `notes`, `expected_on`, and lines |
| `create_order_from_request` | `supply-chain.purchasing.orders.manage` | A draft from an approved request; without `lines` its lines are copied at a unit price of zero |
| `update_order` | `supply-chain.purchasing.orders.manage` | Changes a draft's given fields (`OrderChanges`); `lines`, when given, replaces every line |
| `submit_order` | `supply-chain.purchasing.orders.manage` | Draft to submitted; needs a line and an active supplier |
| `approve_order` | `supply-chain.purchasing.orders.approve` | Submitted to approved, optional reason; not by its submitter |
| `return_order` | `supply-chain.purchasing.orders.approve` | Submitted back to draft, reason required; not for an amended order |
| `send_order` | `supply-chain.purchasing.orders.manage` | Approved to sent |
| `close_order` | `supply-chain.purchasing.orders.manage` | Sent or partially received to closed, reason required |
| `cancel_order` | `supply-chain.purchasing.orders.manage` (and `orders.approve` once approved) | Any open order to cancelled, reason required; never with goods received |
| `amend_order` | `supply-chain.purchasing.orders.manage` | An approved or sent order's amendment: reason, replacement lines, header facts |
| `record_receipt` | `supply-chain.purchasing.orders.receive` | Goods received against current lines, within a stated tolerance |
| `reverse_receipt` | `supply-chain.purchasing.orders.receive` | Goods taken back from current lines, reason required |

A write answers `Outcome::Saved(record)`, `Outcome::Refused(refusal)` or
`Outcome::CompanyArchived`. `Refusal` is every reason short of
authorization a page names: `Invalid` (one `FieldError` per field, in
Bilimbi's words: "can't be blank", "has already been taken", "has invalid
format", "has more than 4 decimal places", "is invalid"; a line's field
carries its 1-based `line`), `SupplierNotFound`, `SupplierInactive`,
`TaxCodeNotFound`, `RequestNotFound`, `OrderNotFound`, `GoodsReceived`,
`InvalidTransition { status, target }`, `NoLines`, `OwnRequest` and
`OwnOrder`. `Refusal::text()` is Bilimbi's reader-facing sentence for
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

## Purchase orders

An order buys from one active supplier of the company in one currency (a
`currency_code` of three letters, stored upper-cased; the company's
currencies are its data, never a list here), with an optional `reference`
(100), `notes` (1000) and `expected_on` date. Each line is a `description`
(500), an optional `external_item_ref` (100), a `quantity` above zero, a
`unit` (20) and a `unit_price` from zero, both under 10^16 with up to four
decimal places, and optionally a `tax_code_id` naming an active tax code,
whose rate the line snapshots. A line's total (quantity times unit price)
must stay under 10^20 ("makes the line total too large"), so an order's
totals always fit the decimal type they are computed in. The database
numbers each company's orders 1, 2, 3, ... without gaps, and records the
request an order was made from; a request may be ordered more than once.

| From | To | Function | Who |
| --- | --- | --- | --- |
| (new) | draft | `create_order`, `create_order_from_request` (an approved request) | orders.manage |
| draft | draft | `update_order` | orders.manage |
| draft | submitted | `submit_order` (a line, an active supplier) | orders.manage |
| submitted | approved | `approve_order` (optional reason) | orders.approve, not the submitter |
| submitted | draft | `return_order` (reason; never once amended) | orders.approve |
| approved | sent | `send_order` | orders.manage |
| approved, sent | same, or submitted | `amend_order` (reason) | orders.manage |
| sent | partially received or closed | `record_receipt` | orders.receive |
| partially received | closed | `record_receipt`, `close_order` (reason) | orders.receive, orders.manage |
| partially received, closed | partially received | `reverse_receipt` (reason) | orders.receive |
| draft, submitted, approved, sent, partially received | cancelled | `cancel_order` (reason; nothing received) | orders.manage, plus orders.approve once ever approved |

- **History.** Every creation and transition appends an `OrderEvent` (actor,
  any impersonator, reason, database time) in the same mutation as the
  change. The approver and the submitter are different people
  (`OwnOrder`).
- **Amendments.** An amendment of an approved or sent order is numbered per
  order and kept for good. Its lines replace the current ones, which stay
  on the order marked with the amendment that superseded them. A change to
  any line (description, item reference, quantity, unit, price or tax) or to
  the number of lines returns the order to submitted with the amender as
  submitter, clearing its approval and sending date, so someone else
  approves it again; a change to `reference`, `notes` or `expected_on` alone
  keeps the status. A partially received order is closed short instead.
- **Receipts.** Receiving's seam. `record_receipt` takes `ReceiptLine`s
  (`line_id`, `quantity` above zero), each naming a current line once, and
  the `tolerance_percent` (0 to 100, four places) the caller's company
  configured: a line's received quantity stays at or below its ordered
  quantity raised by that percentage ("exceeds the ordered quantity", or
  "... and the tolerance"). A sent order becomes partially received; one
  whose current lines are all received in full closes ("every line
  received in full"). `reverse_receipt` subtracts, never below zero
  ("exceeds what was received"), and takes a closed order back to partially
  received with the reason in its history. The goods receipt itself is
  Receiving's record.
- **Totals.** A line's `line_total` is quantity times unit price and its
  `tax_amount` that total times the rate over 100, unrounded; an order's
  `subtotal`, `tax_total` and `total` sum its current lines.

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
  under `supply-chain.purchasing.orders.receive` in its own posting
  transaction.
- **Order line port.** Bilimbi's Payables read an order line through the
  runtime seam `OrderSource`. Here the module contributes the optional port
  `supply_chain/purchasing.order_line` (`base/ports`), so a module in
  another container reads one line now, on its own client and sealed scope,
  with no dependency or edge on this crate, and gets `Unavailable` when
  Supply Chain is not mounted. The provider checks
  `supply-chain.purchasing.view` for the caller's actor and company exactly
  as `get_order_line` does.
  - Request: `{"company_id": <integer>, "line_id": <integer>}`, nothing else
    (another shape is `Invalid`).
  - Answer: the line, `{"id", "line_number", "description",
    "external_item_ref", "quantity", "unit", "unit_price", "tax_code_id",
    "tax_rate", "received_quantity", "amendment_id",
    "superseded_by_amendment_id", "line_total", "tax_amount"}`, with its
    order's `"order_id"`, `"order_number"`, `"order_status"`,
    `"supplier_id"` and `"currency_code"`. Decimals are strings; absent
    values are `null`. A line of no order of the company is `NotFound`.
- **Domain events.** Nothing here is published yet: the goods-received fact
  a Finance projection copies is Receiving's
  (`supply_chain/receiving.*`, `apps/base/events`).
- **Factory** inventory is reached only through a line's
  `external_item_ref`, a free reference the company fills from any
  catalogue; Purchasing never resolves it.

## Reviewed agent amendment

`supply_chain.purchasing.order.amend` exposes `amend_order` through the Platform's `/api/v1` and `/mcp` plan protocol, under `supply-chain.purchasing.orders.manage`; neither `view` nor `orders.approve` is required. Describe the operation and read guide `supply_chain.purchasing.order` before constructing a plan. Direct operation calls never write. No amendment page is added.

Required input is `company_id`, `order_id`, `expected_amendment_count` (the order's current count, initially 0), `reason` and the complete replacement `lines`. Each line requires `description`, `quantity`, `unit` and `unit_price`, with optional `external_item_ref` and `tax_code_id`. Decimals are strings, not JSON numbers; the facade validates precision and bounds. Omitted line tax means no tax. Optional `reference`, `notes` and `expected_on` preserve current values when omitted and clear them when blank; dates are `YYYY-MM-DD`. Unknown fields and `null` inputs are refused. Supplier, currency, request link, actors, line identities and received quantities cannot be supplied.

Preview validates the same rules as commit without business mutations and shows normalized replacement facts, resolved tax rates, existing line identities, and whether approval and sending will be cleared. Only approved and sent orders qualify. Every amendment replaces the current line identities, even if their business values are numerically unchanged. Line changes resubmit the order under the amender and require another person to approve it; header-only changes preserve status. Obtain the person's consent to the review, then commit its expiring, single-use plan code with identical ordered inputs. The result contains `record` with the saved header, current lines, exact decimal totals and lifecycle actors. It intentionally has no page URL until the order-detail page lands.

Commit uses the plan's `MutationTransaction`, reauthorizes through Core Company, locks the writable company and order, and captures all changes through the existing facade. Plan revalidation detects changed order or tax facts and revoked grants. `expected_amendment_count` is checked again under lock, so two amendments to the same order in one plan refuse the second and roll back the whole plan, including audit and code consumption. Use separate reviewed plans instead. Invalid values and business refusals retain the facade's text in field errors (line paths are zero-based, such as `lines.0.quantity`); missing orders return `not_found` and stale counts return `stale_plan`.

Platform limitation: `OperationClient` cannot enter the generic Company writable guard. Preview authorizes with Company's equivalent object-safe `organisation::Directory::target`, then takes a tenant-scoped company row lock on the supplied plan connection and checks the locked status. This avoids a second pool connection and keeps reference facts locked through commit revalidation and execution. Commit uses `authorize_target` and `require_writable` normally. An object-safe Company writable guard would remove the narrow preview SQL workaround; no Platform code is changed here.

`fixtures/walkthrough` exercises the real registry and plan services with authenticated sessions: manage without view, direct-write refusal, mutation-free preview, header-only and changed-line commits, sent-order amendments, replay, duplicate-order atomic rollback, tax drift, grant revocation and field errors. It prints no credentials or plan codes and creates records only through the facades in its disposable database.

## Pages and menu

The Supply chain menu (`supply_chain`, order 55) offers Purchase requests
(`/supply-chain/purchase-requests`), Purchase orders
(`/supply-chain/purchase-orders`) and Suppliers (`/supply-chain/suppliers`)
to accounts holding `supply-chain.purchasing.view`; the root stays hidden
from everyone else. Amendments have no page, as in Bilimbi. The module contributes the Suppliers and Purchase
requests pages (service `supply_chain/purchasing.pages`, both behind
`supply-chain.purchasing.view`); the purchase order pages are the next
slice. Both pages work in the signed-in account's own company (its
affiliation), read each write's capability again on every write-shaped
event, and the facade checks it once more; a refusal or a lost grant is a
flash with Bilimbi's sentence, never a cleared page.

- **Suppliers** (Bilimbi `SuppliersLive`): the suppliers and tax codes, each
  a FlexTable (column settings, row height and compact rows, sort on every
  column in memory over the facade's bounded list; the tax codes' sort
  query names carry a `tax_` prefix), the New supplier and New tax code
  forms, and Deactivate/Activate per row, all under
  `configuration.manage`. A toggle names its row and sets the opposite of
  what that row showed.
- **Purchase requests** (Bilimbi `RequestsLive`): the New purchase request
  form (title, needed-by date, justification, five line rows; rows without
  a description are ignored), status tabs (`?status=`, `all` by default),
  the requests FlexTable newest number first, and per row Submit (draft),
  Approve and Reject with a reason (submitted), Create order with a
  supplier and currency (approved) and Cancel with a reason (draft,
  submitted, approved). Submit, Approve, Reject and Cancel wait for the
  shared confirmation dialog, which acts on the action it holds. Approve
  and Reject are two small forms, each with its reason field, because the
  live transport posts a form without the button that submitted it.
  Create order stays on the page with Bilimbi's flash until the order page
  exists (`TODO(S4)`: navigate to `/supply-chain/purchase-orders/:id`).
