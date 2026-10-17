//! The `supply_chain/purchasing.order_line` port: Bilimbi's `OrderSource`
//! seam, through which a module that may run without Supply Chain (Finance
//! payables matching a bill line) reads one order line now, on its own
//! client and scope. The request and answer are documented in `README.md`;
//! that shape is the whole contract. Purchasing calls no port itself.
use crate::{order_line, OrderLine};
use bilibi_database::Error;
use bilibi_ports::{answer, on_client, request, PortClient, PortFuture, PortProvider, Value};
use bilibi_tenancy::Scope;
use serde_json::json;

/// The port ID, in this module's namespace.
pub(crate) const ORDER_LINE: &str = "supply_chain/purchasing.order_line";

fn decimal(value: rust_decimal::Decimal) -> String {
    value.to_string()
}

fn line_json(line: &OrderLine) -> Value {
    json!({
        "id": line.id, "line_number": line.line_number, "description": line.description,
        "external_item_ref": line.external_item_ref, "quantity": decimal(line.quantity),
        "unit": line.unit, "unit_price": decimal(line.unit_price),
        "tax_code_id": line.tax_code_id, "tax_rate": line.tax_rate.map(decimal),
        "received_quantity": decimal(line.received_quantity),
        "amendment_id": line.amendment_id,
        "superseded_by_amendment_id": line.superseded_by_amendment_id,
        "line_total": decimal(line.line_total), "tax_amount": decimal(line.tax_amount),
    })
}

pub(crate) struct OrderLinePort;
impl PortProvider for OrderLinePort {
    fn call<'a>(&'a self, client: PortClient<'a>, scope: &'a Scope, body: Value) -> PortFuture<'a> {
        Box::pin(async move {
            // `{"company_id": <id>, "line_id": <id>}` and nothing else.
            let body: serde_json::Map<String, Value> = request(body)?;
            if body.len() != 2 {
                return Err(Error::Invalid);
            }
            let id = |key: &str| body.get(key).and_then(Value::as_i64).ok_or(Error::Invalid);
            let (company_id, line_id) = (id("company_id")?, id("line_id")?);
            let facts = on_client!(client, |c| order_line(c, scope, company_id, line_id).await)?
                .ok_or(Error::NotFound)?;
            let mut line = line_json(&facts.line);
            if let Some(fields) = line.as_object_mut() {
                fields.insert("order_id".into(), facts.order_id.into());
                fields.insert("order_number".into(), facts.order_number.into());
                fields.insert("order_status".into(), facts.order_status.as_str().into());
                fields.insert("supplier_id".into(), facts.supplier_id.into());
                fields.insert("currency_code".into(), facts.currency_code.into());
            }
            answer(&line)
        })
    }
}
