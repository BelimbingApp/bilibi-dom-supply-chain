# Receiving (`supply_chain/receiving`)

A company's goods receipts against purchase orders, ported from Bilimbi `Bilimbi.SupplyChain.Receiving` at `4b4f95c8effbd719077b35b9f792f3de413d8dba`. S5 owns the schema and facade; S6 adds the three pages and their menu item. There are no Receiving routes or agent operations in S5.

## Facade

Resolve `Receiving` at `supply_chain/receiving.receiving`. Every call takes a sealed `Scope` and explicit company ID. Reads require `supply-chain.receiving.view`. Writes return `Outcome::Saved`, `Outcome::Refused` (with Bilimbi's sentence in `Refusal::text()`) or `Outcome::CompanyArchived`; missing capability/company remains `Error::Forbidden`/`NotFound`, as in Purchasing. Schema, SQL, validation and lifecycle helpers are private.

| Operations | Capability suffix under `supply-chain.receiving` |
| --- | --- |
| `get_settings`, `list_receipts`, `get_receipt`, `find_receipt_by_number`, `order_receipts`, `order_line_receipts`, `list_postings` | `view` |
| `configure` | `configuration.manage` |
| `create_receipt`, `update_receipt`, `cancel_receipt` | `receipts.manage` |
| `post_receipt`, `reverse_receipt` | `receipts.post-receipt` |

Tenant Owner holds all four. Creation and line replacement also require Purchasing `view`. Posting always reads Purchasing and requires its `view`; accepted quantities additionally require `supply-chain.purchasing.orders.receive`. Reversal requires Purchasing `receive` only when accepted quantities are nonzero. Header-only updates, cancellation and rejected-only reversals do not read Purchasing. Actor and impersonator come from the scope, never input.

`NewReceipt` takes order ID, optional ISO `received_on` (omitted = UTC today), supplier reference, notes and at least one `NewReceiptLine`. Each line names one current order line, once, and carries accepted (`received_quantity`) and rejected quantities as decimal text. Quantities default to zero, are nonnegative, below 10^16 and have at most four decimal places; their sum is positive. Rejected quantities require a rejection reason and do not consume the order's remaining quantity. Header/line texts are trimmed, blanks become absent, and codepoint limits match Bilimbi. `ReceiptChanges` changes only supplied header fields and optionally replaces all lines. `Reversal` takes a required reason, optional date and notes, but no supplier reference override.

Statuses are `draft`, `posted`, `cancelled`; kinds are `receipt`, `reversal`. A draft posts or cancels. Posting refreshes snapshots against the current order and requires `sent`/`partially_received`. A reversal is a separate numbered posted receipt with positive-direction quantities copied from the original, not a negative edit; the original stays posted. An original is reversed once; a reversal cannot be reversed. Correct it with a new receipt. Cancelling and reversing require a reason of at most 500 codepoints.

`configure` appends a tolerance version (0–100 percent, four places); unset means zero with absent setter/time. Draft accepted quantities plus quantities already received must fit ordered quantity × (100 + tolerance) / 100. Posting rechecks the current tolerance through Purchasing. Rejected-only postings leave the order unchanged. Purchasing closes an order received in full and reopens a closed order to `partially_received` when reversed, even if no received quantity remains.

## Transactions and retention

Receiving holds Core Company's writable lock before reading settings/receipt/order state. It owns one `MutationTransaction`; Purchasing's `get_order_on`, `record_receipt_in` and `reverse_receipt_in` participate on that transaction, reauthorize and verify attribution. Receipt rows, order quantities/status/history, audit and outbox events commit together. A refusal or error rolls everything back, including a provisional reversal number. No sibling's private table is read or written.

Migration `supply-chain-receiving-001-receipts` creates receipts, receipt lines, receipt events and append-only settings. Receiving owns all six trigger functions. Company receipt numbers are gapless under the company lock. PostgreSQL freezes posted/cancelled receipts and their lines, keeps all receipt headers, prevents history/settings rewrites and truncation, allows only draft→posted/cancelled, and defers creation/transition-history enforcement until commit. Composite foreign keys reference only Receiving tables; order and supplier references are data, not schema dependencies. Tolerance and current Purchasing line membership are facade rules, not SQL constraints.

## Reads

- Receipt lists sort by number descending, with optional status/order filter and 1–500 limit (default 100). They include lines in receipt-line order and empty history. Single reads/writes also include history in event-ID order and derived original/reversal links.
- `list_postings(PostingQuery)` returns `{posting_id, posted_at, receipt}` ascending by posting-history ID, strictly after the cursor. The timestamp is that history row's database timestamp; nested history is empty. Omitted cursor starts at zero; explicitly supplied cursors must be positive. Receipt/outbox IDs are not cursors. Reversal links in old posting views can become populated later.
- `order_receipts`/`order_line_receipts` aggregate posted lines, including superseded order lines, adding normal and subtracting reversal accepted **and rejected** quantities. Contributions keep positive-direction quantities and sort by header posting timestamp then receipt ID. Latest contributing snapshots supply order facts. No matching line returns its supplied ID, null facts, zero quantities and no receipts; no matching order returns an empty list. These historical summaries intentionally return all contributions for the requested order/line, matching Bilimbi.

## Durable event contract, version 1

The captain's cross-domain event rule supersedes the original `supply_chain/receiving.line_receipts` port deliverable. No Receiving port is registered. Other containers subscribe through Base events and keep their own projection; they never call Receiving or read its tables.

| Name | Meaning |
| --- | --- |
| `supply_chain/receiving.goods_received` | One line of a normal receipt was posted; add accepted and rejected quantities. |
| `supply_chain/receiving.receipt_reversed` | One line of a reversal was posted; subtract both quantities. |

Each posting records one event **per line**, in line-number order, inside the business transaction, including rejected-only lines. This avoids applying Base's 100-member/64-KiB event bounds to a receipt's line count. Each event is independently applicable to an order-line projection. Subscribers requiring receipt-wide atomic visibility stage parts until `part_count` distinct parts exist; Base delivers individual events transactionally, not an entire posting at once.

- Subject: `order:<order_id>`. Both event types order together per tenant/publisher/order, across receipts and reversals. Ordering does not extend to another publisher.
- Key: `posting:<posting_id>:line:<line_number>`, unique per tenant/publisher, not the company-local receipt number. Use Base's event key for subscriber idempotency.
- Root payload: `company_id`, `posting_id`, `posted_at` (UTC ISO instant from history), `receipt_id`, `receipt_number`, `kind`, `order_id`, `order_number`, `supplier_id`, `reverses_receipt_id`, `reason`, `received_on` (ISO date), `supplier_reference`, `notes`, `part_index` (1-based line number), `part_count`, `line`.
- `line`: `id` (receipt-line ID), `line_number`, `order_line_id`, `order_line_number`, `description`, `unit`, `ordered_quantity`, `received_quantity`, `rejected_quantity`, `rejection_reason`. Decimals are strings; absent values are JSON null.
- Facts are immutable posting snapshots. They exclude actors and subsequently derived reversal links. A reversal carries its own line IDs and points to the original receipt; it does not retract the original outbox row.

## Proof

`fixtures/walkthrough` exercises the facade against a disposable migrated PostgreSQL 18 database, including refusal/rollback, tolerance, reversal, cursor, summary, authorization and SQL retention cases. There are no unit tests. The release gate is the pinned Platform's `scripts/domain-ci.sh`.
