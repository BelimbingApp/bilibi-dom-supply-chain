//! Amendment editor on Bilimbi's OrderLive record home. The reviewed operation
//! owns every rule; this caller retains the exact input and session-bound plan.
use super::{render, text, Core, Flash};
use crate::{agent_operations::AMEND, contributions::PAGES, Order, TaxCode};
use askama::Template;
use bilibi_agent_api::{page::Call, Error as OperationError};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    BuiltinColumn, Button, ButtonVariant, Choice, Fact, Facts, Field, FlashMap, FlexRow, FlexTable,
    Form, Html, InputKind, InstanceBanner, ListColumns, Modal, ModalWidth, SectionHeading,
    StatusKind,
};
use serde_json::{json, Value};

const LINE_FIELDS: [(&str, &str); 6] = [
    ("description", "Description"),
    ("external_item_ref", "Item reference"),
    ("quantity", "Quantity"),
    ("unit", "Unit"),
    ("unit_price", "Unit price"),
    ("tax_code_id", "Tax code"),
];
struct Review {
    input: Value,
    code: String,
    response: Value,
}
pub(super) struct Editor {
    input: Value,
    taxes: Vec<TaxCode>,
    review: Option<Review>,
    flash: Flash,
}
#[derive(Template)]
#[template(path = "order_amend_fields.html")]
struct Fields {
    headers: Vec<Html>,
    lines: Vec<(String, Vec<Html>, Html)>,
}
#[derive(Template)]
#[template(path = "order_amend_body.html")]
struct Body<'a> {
    flash: FlashMap<'a>,
    parts: Vec<Html>,
}
#[derive(Template)]
#[template(path = "order_amend_buttons.html")]
struct Buttons {
    buttons: Vec<Html>,
}

fn button(label: &str, event: &str, submit: bool, value: &str) -> Result<Html> {
    render(&Button {
        body: Html::text(label),
        event,
        button_type: if submit { "submit" } else { "button" },
        name: if submit { "draft_action" } else { "" },
        value,
        variant: if value == "review" || event == "amend_commit" {
            ButtonVariant::Primary
        } else {
            ButtonVariant::Secondary
        },
        ..Button::default()
    })
}
impl Editor {
    pub(super) fn new(order: &Order, taxes: Vec<TaxCode>) -> Self {
        Self {
            input: json!({"company_id":order.company_id,"order_id":order.id,
                "expected_amendment_count":order.amendment_count,"reason":"",
                "reference":order.reference.as_deref().unwrap_or_default(),
                "notes":order.notes.as_deref().unwrap_or_default(),
                "expected_on":order.expected_on.as_deref().unwrap_or_default(),
                "lines":order.lines.iter().map(|line| json!({
                    "description":line.description,"external_item_ref":line.external_item_ref,
                    "quantity":line.quantity.to_string(),"unit":line.unit,
                    "unit_price":line.unit_price.to_string(),"tax_code_id":line.tax_code_id
                })).collect::<Vec<_>>() }),
            taxes,
            review: None,
            flash: Flash::default(),
        }
    }
    fn capture(&mut self, payload: &Value) {
        for key in ["reason", "reference", "notes", "expected_on"] {
            self.input[key] = json!(text(payload, key));
        }
        for (index, line) in self.input["lines"]
            .as_array_mut()
            .into_iter()
            .flatten()
            .enumerate()
        {
            for (key, _) in LINE_FIELDS {
                let value = text(payload, &format!("line_{index}_{key}"));
                line[key] = if key == "tax_code_id" {
                    params_id(value).map_or(Value::Null, |id| json!(id))
                } else {
                    json!(value)
                };
            }
            if line["tax_code_id"].is_null() {
                line.as_object_mut().unwrap().remove("tax_code_id");
            }
        }
    }
    pub(super) async fn event(
        &mut self,
        core: &Core,
        scope: &Scope,
        credential: &str,
        payload: &Value,
    ) -> Result<bool> {
        let event = text(payload, "type");
        let call = Call {
            scope,
            credential,
            service: PAGES,
            event,
            contract: AMEND,
        };
        match event {
            "amend_edit" => self.review = None,
            "amend_review" if self.review.is_none() => {
                self.capture(payload);
                let action = text(payload, "draft_action");
                if action == "add" {
                    self.input["lines"].as_array_mut().unwrap().push(json!({}));
                    return Ok(false);
                }
                if let Some(index) = action
                    .strip_prefix("remove_")
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    let lines = self.input["lines"].as_array_mut().unwrap();
                    if index < lines.len() {
                        lines.remove(index);
                    }
                    return Ok(false);
                }
                match core.contracts.preview(&call, self.input.clone()).await {
                    Ok(response) if response["valid"] == true => {
                        let code = response["plan_code"]
                            .as_str()
                            .ok_or(Error::Internal)?
                            .to_owned();
                        self.flash = Flash::default();
                        self.review = Some(Review {
                            input: self.input.clone(),
                            code,
                            response,
                        });
                    }
                    Ok(response) => self.flash.error(preview_errors(&response)),
                    Err(error) => self.flash.error(operation_error(error)),
                }
            }
            "amend_commit" => {
                // Taking the review prevents replay and automatic retry after any outcome.
                if let Some(review) = self.review.take() {
                    match core
                        .contracts
                        .commit(&call, &review.code, review.input)
                        .await
                    {
                        Ok(_) => return Ok(true),
                        Err(error) => self.flash.error(operation_error(error)),
                    }
                }
            }
            _ => {}
        }
        Ok(false)
    }
    pub(super) fn render(&self) -> Result<Html> {
        let parts = if let Some(review) = &self.review {
            let response = &review.response;
            let instance = &response["instance"];
            let banner = render(&InstanceBanner {
                id: "amend-instance",
                role: instance["role"].as_str().unwrap_or_default(),
                label: instance["label"].as_str().unwrap_or_default(),
                message: &format!(
                    "{} · {} · {}",
                    display(&instance["role"]),
                    display(&instance["origin"]),
                    display(&instance["notice"])
                ),
                tone: StatusKind::Warning,
                ..InstanceBanner::default()
            })?;
            let preview = &response["results"][0]["preview"];
            let summary = facts(
                "amend-review-summary",
                &json!({
                    "Reason":preview["reason"], "All current lines will be replaced":preview["replaces_all_lines"],
                    "Fresh approval and sending required":preview["reapproval_required"], "Review expires at":response["expires_at"]
                }),
            )?;
            vec![
                banner,
                summary,
                render(&SectionHeading {
                    title: "Current order",
                    ..SectionHeading::default()
                })?,
                facts("amend-before", &preview["before"])?,
                render(&SectionHeading {
                    title: "After amendment",
                    ..SectionHeading::default()
                })?,
                facts("amend-after", &preview["after"])?,
                render(&Buttons {
                    buttons: vec![
                        button("Cancel", "amend_cancel", false, "")?,
                        button("Back to changes", "amend_edit", false, "")?,
                        button("Confirm amendment", "amend_commit", false, "")?,
                    ],
                })?,
            ]
        } else {
            let headers = [
                ("reason", "Reason", 500),
                ("reference", "Supplier reference", 100),
                ("expected_on", "Expected delivery", 10),
                ("notes", "Notes", 1000),
            ]
            .into_iter()
            .map(|(key, label, max)| {
                render(
                    &Field::named(key)
                        .id(format!("amend-{key}"))
                        .label(label)
                        .kind(if key == "expected_on" {
                            InputKind::Date
                        } else {
                            InputKind::Text
                        })
                        .value(self.input[key].as_str().unwrap_or_default())
                        .attr("maxlength", &max.to_string()),
                )
            })
            .collect::<Result<Vec<_>>>()?;
            let lines = self.input["lines"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let fields = LINE_FIELDS
                        .iter()
                        .map(|(key, label)| {
                            let field = Field::named(format!("line_{index}_{key}"))
                                .id(format!("amend-line-{index}-{key}"))
                                .label(*label)
                                .value(if line[*key].is_null() {
                                    String::new()
                                } else {
                                    display(&line[*key])
                                });
                            let field = if *key == "tax_code_id" {
                                field.kind(InputKind::Select).prompt("None").options(
                                    self.taxes
                                        .iter()
                                        .map(|tax| Choice::new(&tax.code, tax.id.to_string()))
                                        .collect(),
                                )
                            } else {
                                field
                            };
                            render(&field)
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok((
                        format!("Line {}", index + 1),
                        fields,
                        button("Remove line", "", true, &format!("remove_{index}"))?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            let form = render(&Form {
                id: "order-amend-form",
                event: "amend_review",
                body: render(&Fields { headers, lines })?,
                actions: render(&Buttons {
                    buttons: vec![
                        button("Cancel", "amend_cancel", false, "")?,
                        button("Add line", "", true, "add")?,
                        button("Review amendment", "", true, "review")?,
                    ],
                })?,
                ..Form::default()
            })?;
            vec![form]
        };
        render(&Modal {
            id: "order-amend-editor",
            title: if self.review.is_some() {
                "Review purchase order amendment"
            } else {
                "Amend purchase order"
            },
            cancel_event: "amend_cancel",
            width: ModalWidth::Wide,
            description:
                "Every current line is replaced. Changed lines require fresh approval and sending.",
            body: render(&Body {
                flash: self.flash.map(),
                parts,
            })?,
            ..Modal::default()
        })
    }
}
fn params_id(value: &str) -> Option<i64> {
    bilibi_ui::params::positive_id(Some(value))
}
fn display(value: &Value) -> String {
    match value {
        Value::Null => "—".into(),
        Value::String(text) => text.clone(),
        Value::Bool(v) => if *v { "Yes" } else { "No" }.into(),
        other => other.to_string(),
    }
}
// Render every owner fact, including line identities and resolved taxes. Do not
// rebuild a preview from the draft: it would omit the owner's resolved facts.
fn facts(id: &str, value: &Value) -> Result<Html> {
    let Some(object) = value.as_object() else {
        return Ok(Html::text(&display(value)));
    };
    let titles: Vec<_> = object.keys().map(|key| key.replace('_', " ")).collect();
    let mut items = Vec::new();
    for ((key, value), title) in object.iter().zip(&titles) {
        let body = if let Some(lines) = value.as_array() {
            let keys: std::collections::BTreeSet<_> = lines
                .iter()
                .filter_map(Value::as_object)
                .flat_map(|line| line.keys().cloned())
                .collect();
            let columns = keys
                .iter()
                .map(|key| BuiltinColumn::new(key, &key.replace('_', " ")))
                .collect();
            let rows = lines
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    keys.iter()
                        .fold(FlexRow::new(format!("{id}-{i}")), |row, key| {
                            row.slot(key, Html::text(&display(&line[key])))
                        })
                })
                .collect();
            let table_id = format!("{id}-{key}");
            let columns =
                ListColumns::projection(&table_id, columns).map_err(|_| Error::Internal)?;
            render(&FlexTable::new(&table_id, columns.flex_columns(), rows))?
        } else {
            Html::text(&display(value))
        };
        items.push(Fact {
            title,
            value: body,
            ..Fact::default()
        });
    }
    render(&Facts { id, items })
}
fn preview_errors(response: &Value) -> String {
    let error = &response["results"][0]["error"];
    if let Some(fields) = error["fields"].as_object() {
        return fields
            .values()
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
    }
    match error["code"].as_str() {
        Some("stale_plan") => "This order changed. Cancel and open a new amendment.".into(),
        Some("missing_capability") => "You no longer have permission to amend this order.".into(),
        _ => "This amendment cannot be reviewed. Cancel and open the order again.".into(),
    }
}
fn operation_error(error: OperationError) -> String {
    match error {
        OperationError::InvalidInput(fields) => fields
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
        OperationError::PlanRejected(response) => preview_errors(&response),
        OperationError::Forbidden | OperationError::Backend(Error::Forbidden) => {
            "You no longer have permission to amend this order.".into()
        }
        OperationError::NotFound | OperationError::Backend(Error::NotFound) => {
            "This order is no longer available.".into()
        }
        OperationError::StalePlan
        | OperationError::PlanExpired
        | OperationError::InvalidPlanCode
        | OperationError::PlanMismatch
        | OperationError::PlanAlreadyUsed => {
            "This review is no longer current. Cancel and open a new amendment.".into()
        }
        _ => "The amendment could not be confirmed. Check the order before trying again.".into(),
    }
}
