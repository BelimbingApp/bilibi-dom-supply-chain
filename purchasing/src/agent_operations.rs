//! Headless reach for Bilimbi `Purchasing.amend_order/4`. The agent protocol
//! mirrors Bilimbi `AgentAPI`: review first, then commit through the facade
//! on the plan's audited transaction. No new page or endpoint.
use crate::{
    order_changes, orders, NewOrderLine, Order, OrderAmendment, Outcome, Purchasing, Refusal,
    MANAGE_ORDERS, MODULE,
};
use bilibi_agent_api::{
    Definitions, Error, Guide, Handler, Operation, OperationClient, OperationFuture, WriteHandler,
    DEFINITIONS,
};
use bilibi_company::organisation;
use bilibi_database::MutationTransaction;
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::{OrganisationDirectory, Scope};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn refusal(refusal: Refusal) -> Error {
    match refusal {
        Refusal::Invalid(errors) => {
            let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for error in errors {
                let field = error.line.map_or_else(
                    || error.field.to_owned(),
                    |line| format!("lines.{}.{}", line - 1, error.field),
                );
                fields.entry(field).or_default().push(error.message);
            }
            Error::InvalidInput(fields)
        }
        Refusal::OrderNotFound => Error::NotFound,
        other => Error::InvalidInput(BTreeMap::from([(
            if matches!(other, Refusal::NoLines) {
                "lines"
            } else {
                "order_id"
            }
            .into(),
            vec![other.text()],
        )])),
    }
}
fn archived() -> Error {
    Error::InvalidInput(BTreeMap::from([(
        "company_id".into(),
        vec!["This company is archived; nothing in it can change.".into()],
    )]))
}
fn integer(input: &Value, field: &str) -> bilibi_agent_api::Result<i64> {
    input[field]
        .as_i64()
        .ok_or(bilibi_database::Error::Invalid.into())
}
fn count(input: &Value) -> bilibi_agent_api::Result<i32> {
    integer(input, "expected_amendment_count")?
        .try_into()
        .map_err(|_| bilibi_database::Error::Invalid.into())
}
fn text(input: &Value, field: &str) -> String {
    input[field].as_str().unwrap_or_default().to_owned()
}
fn amendment(input: &Value) -> bilibi_agent_api::Result<OrderAmendment> {
    let lines = input["lines"]
        .as_array()
        .ok_or(bilibi_database::Error::Invalid)?;
    Ok(OrderAmendment {
        reason: text(input, "reason"),
        reference: input["reference"].as_str().map(str::to_owned),
        notes: input["notes"].as_str().map(str::to_owned),
        expected_on: input["expected_on"].as_str().map(str::to_owned),
        lines: lines
            .iter()
            .map(|line| NewOrderLine {
                description: text(line, "description"),
                external_item_ref: text(line, "external_item_ref"),
                quantity: text(line, "quantity"),
                unit: text(line, "unit"),
                unit_price: text(line, "unit_price"),
                tax_code_id: line["tax_code_id"].as_i64(),
            })
            .collect(),
    })
}

fn record(order: &Order) -> Value {
    let mut value = orders::to_json(order);
    value["lines"] = json!(order
        .lines
        .iter()
        .map(|line| orders::line_json(order.id, line))
        .collect::<Vec<_>>());
    value["subtotal"] = json!(order.subtotal.to_string());
    value["tax_total"] = json!(order.tax_total.to_string());
    value["total"] = json!(order.total.to_string());
    value
}

impl Purchasing {
    async fn preview_amendment(
        client: &dyn OperationClient,
        scope: &Scope,
        input: &Value,
    ) -> bilibi_agent_api::Result<Value> {
        let company_id = integer(input, "company_id")?;
        // Company's object-safe equivalent of authorize_target, on this same
        // connection. No pool/global binding and no extra VIEW capability.
        organisation::Directory
            .target(
                client,
                scope,
                company_id
                    .try_into()
                    .map_err(|_| bilibi_database::Error::Invalid)?,
                MANAGE_ORDERS,
            )
            .await?;
        // Platform need: require_writable currently accepts only GenericClient,
        // not OperationClient. Keep this narrow lock on the plan's transaction;
        // commit still uses the normal Company guard in amend_order_in.
        let company = client.query_opt(
            "SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE",
            &[&scope.tenant(), &company_id],
        ).await?.ok_or(Error::NotFound)?;
        if company.get::<_, String>(0) == "archived" {
            return Err(archived());
        }
        let current = orders::amendment_current(
            client,
            scope.tenant(),
            company_id,
            integer(input, "order_id")?,
        )
        .await?
        .ok_or(Error::NotFound)?;
        if count(input)? != current.amendment_count {
            return Err(Error::StalePlan);
        }
        let prepared = order_changes::prepare_amendment(
            client,
            scope.tenant(),
            company_id,
            &current,
            &amendment(input)?,
        )
        .await?
        .map_err(refusal)?;
        let replacement: Vec<_> = prepared.lines.iter().enumerate().map(|(index, line)| json!({
            "line_number":index + 1, "description":line.description,
            "external_item_ref":line.external_item_ref, "quantity":line.quantity.normalize().to_string(),
            "unit":line.unit, "unit_price":line.unit_price.normalize().to_string(),
            "tax_code_id":line.tax_code_id, "tax_rate":line.tax_rate.map(|rate| rate.normalize().to_string()),
            "received_quantity":"0",
        })).collect();
        let before = record(&current);
        Ok(json!({
            "summary":"Amend purchase order", "company_id":company_id,
            "reason":prepared.reason, "replaces_all_lines":true,
            "reapproval_required":prepared.resubmitted, "before":before,
            "after":{
                "id":current.id, "order_number":current.order_number,
                "supplier_id":current.supplier_id, "currency_code":current.currency_code,
                "reference":prepared.header.reference, "notes":prepared.header.notes,
                "expected_on":prepared.header.expected_on,
                "amendment_count":current.amendment_count + 1,
                "status":if prepared.resubmitted { "submitted" } else { current.status.as_str() },
                "submitted_by_user_id":if prepared.resubmitted { Some(scope.user()) } else { current.submitted_by_user_id },
                "approved_by_user_id":if prepared.resubmitted { None } else { current.approved_by_user_id },
                "approval_cleared":prepared.resubmitted, "sending_cleared":prepared.resubmitted,
                "lines":replacement,
            },
        }))
    }
}

struct Amend;
impl WriteHandler for Amend {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            Ok(Purchasing::preview_amendment(client, scope, input)
                .await?
                .into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let outcome = Purchasing::amend_order_in(
                tx,
                scope,
                integer(input, "company_id")?,
                integer(input, "order_id")?,
                &amendment(input)?,
                count(input)?,
            )
            .await
            .map_err(|error| match error {
                bilibi_database::Error::Conflict => Error::StalePlan,
                bilibi_database::Error::NotFound => Error::NotFound,
                other => other.into(),
            })?;
            match outcome {
                Outcome::Saved(order) => Ok(json!({"record":record(&order)}).into()),
                Outcome::Refused(reason) => Err(refusal(reason)),
                Outcome::CompanyArchived => Err(archived()),
            }
        })
    }
}

fn schema() -> Value {
    json!({"type":"object","required":["company_id","order_id","expected_amendment_count","reason","lines"],"properties":{
        "company_id":{"type":"integer","minimum":1},
        "order_id":{"type":"integer","minimum":1},
        "expected_amendment_count":{"type":"integer","minimum":0,"maximum":2147483646,"description":"Current amendment_count; stale counts are refused."},
        "reason":{"type":"string","maxLength":500},
        "reference":{"type":"string","maxLength":100,"description":"Omit to keep; blank clears."},
        "notes":{"type":"string","maxLength":1000,"description":"Omit to keep; blank clears."},
        "expected_on":{"type":"string","maxLength":10,"description":"YYYY-MM-DD; omit to keep, blank clears."},
        "lines":{"type":"array","description":"Complete replacement lines, at least one. Even unchanged lines receive new identities.","items":{
            "type":"object","required":["description","quantity","unit","unit_price"],"properties":{
                "description":{"type":"string","maxLength":500},
                "external_item_ref":{"type":"string","maxLength":100},
                "quantity":{"type":"string","maxLength":100,"description":"Positive exact decimal, at most four decimal places."},
                "unit":{"type":"string","maxLength":20},
                "unit_price":{"type":"string","maxLength":100,"description":"Nonnegative exact decimal, at most four decimal places."},
                "tax_code_id":{"type":"integer","minimum":1,"description":"Active tax code of this company; omit for no tax."}
            }
        }}
    }})
}

inventory::submit! { Contribution::new(DEFINITIONS, "supply_chain/purchasing.agent", Provenance { module: MODULE, source: file!(), line: line!() }, &Definitions {
    operations: &[Operation {
        key: "supply_chain.purchasing.order.amend", title: "Amend a purchase order",
        summary: "Amend an approved or sent purchase order with a reason and complete replacement lines. Changed lines require fresh approval and sending.",
        keywords: &["purchase", "order", "amend", "amendment", "supplier"],
        capability: MANAGE_ORDERS, input: schema,
        output: || json!({"type":"object","description":"Amended record with status, amendment_count, current lines, exact decimal totals and lifecycle actors."}),
        handler: Handler::SafeWrite(&Amend),
    }],
    guides: &[Guide {
        key: "supply_chain.purchasing.order", title: "Purchase order amendments",
        summary: "Review and commit amendments without changing the purchase order UI.",
        keywords: &["purchase", "order", "amendment", "approval"], capability: MANAGE_ORDERS,
        body: "Use supply_chain.purchasing.order.amend only for approved or sent orders. Supply company_id, order_id, the current expected_amendment_count, a reason and every replacement line. Never infer company wording, currency, units or tax codes. Quantities and unit prices are exact decimal strings. Optional reference, notes and expected_on preserve their current values when omitted; blank clears them. Omitting a line's tax_code_id removes tax. Supplier, currency and request link cannot change.\n\nPreview a plan through /api/v1 or /mcp and obtain the person's consent to the full review before committing its single-use plan code. Direct calls do not write. The review shows normalized header facts, current line identities, resolved tax rates, replacement lines and whether fresh approval is required. Every amendment supersedes all current lines, even when their values are unchanged. Changing any line's description, item reference, quantity, unit, price or tax returns the order to submitted, clears approval and sending, and makes the amender the submitter; someone else approves it. Header-only changes preserve approved/sent status.\n\nPreview is not a reservation: order, tax, company or permission changes can require a fresh review. Do not put two amendments to the same order in one plan: the count precondition refuses the second and rolls back the whole plan. Partially received orders cannot be amended; close them short instead. Refusals change nothing.",
    }],
}) }
