//! Purchase orders list and draft form (Bilimbi `Purchasing.Web.OrdersLive`).
use super::lists::{columns, sort, table};
use super::{can, company_id, id, or_empty, outcome, render, text, Core, Flash};
use crate::contributions::ORDERS_PATH;
use crate::{
    NewOrder, NewOrderLine, Order, OrderQuery, OrderStatus, Supplier, TaxCode, MANAGE_ORDERS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::ParamNames,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Badge, BadgeKind, Choice, ColumnsOutcome, EmptyState, Field, Filter, FlashMap, FlexRow,
    Html, InputKind, ListColumns, ListSpec, ListState, RecordLink, SortDir, StatusKind, Tab, Tabs,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const TABS: [&str; 8] = [
    "all",
    "draft",
    "submitted",
    "approved",
    "sent",
    "partially_received",
    "closed",
    "cancelled",
];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("number".into(), SortDir::Desc),
            ("supplier".into(), SortDir::Asc),
            ("lines".into(), SortDir::Desc),
            ("total".into(), SortDir::Desc),
            ("expected".into(), SortDir::Asc),
            ("status".into(), SortDir::Asc),
        ]),
        default_sort: Some("number".into()),
        filters: BTreeMap::from([("status".into(), Filter::one_of(&TABS, "all"))]),
        param_names: ParamNames {
            sort_by: "sort".into(),
            sort_dir: "dir".into(),
            ..ParamNames::default()
        },
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("purchase orders list spec")
});
fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}
pub(super) fn label(status: &str) -> String {
    let words = status.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map_or(String::new(), |first| {
        first.to_uppercase().chain(chars).collect()
    })
}
pub(super) fn badge(status: OrderStatus) -> Result<Html> {
    render(&Badge::status(
        match status {
            OrderStatus::Submitted | OrderStatus::PartiallyReceived => BadgeKind::Warning,
            OrderStatus::Approved | OrderStatus::Sent => BadgeKind::Success,
            OrderStatus::Cancelled => BadgeKind::Danger,
            _ => BadgeKind::Neutral,
        },
        &label(status.as_str()),
    ))
}
pub(super) fn supplier(order: &Order) -> &str {
    order
        .supplier
        .as_ref()
        .map_or("the supplier", |s| s.name.as_str())
}
pub(super) fn address(id: i64) -> String {
    format!("{ORDERS_PATH}/{id}")
}
#[derive(Template)]
#[template(path = "orders.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    can_manage: bool,
    fields: Vec<Field>,
    notes: Field,
    lines: Vec<[Field; 6]>,
    tabs: Html,
    orders: Html,
}
pub(super) struct Orders {
    core: Arc<Core>,
    list: ListState,
    columns: ListColumns,
    rows_changed: bool,
    can_manage: bool,
    framed: bool,
    form: HashMap<String, String>,
    orders: Vec<Order>,
    suppliers: Vec<Supplier>,
    taxes: Vec<TaxCode>,
    flash: Flash,
    redirect: Option<(String, String)>,
}
impl Orders {
    pub(super) fn new(
        core: Arc<Core>,
        query: &HashMap<String, String>,
        framed: bool,
    ) -> Result<Self> {
        Ok(Self {
            core,
            list: parse(query)?,
            columns: ListColumns::mount(
                "orders-table",
                columns(
                    &[
                        ("number", "Number"),
                        ("supplier", "Supplier"),
                        ("lines", "Lines"),
                        ("total", "Total"),
                        ("expected", "Expected"),
                        ("status", "Status"),
                    ],
                    &["total"],
                ),
            )
            .map_err(|_| Error::Internal)?,
            rows_changed: true,
            can_manage: false,
            framed,
            form: HashMap::new(),
            orders: vec![],
            suppliers: vec![],
            taxes: vec![],
            flash: Flash::default(),
            redirect: None,
        })
    }
    fn tab(&self) -> &str {
        self.list
            .filters
            .get("status")
            .map_or("all", String::as_str)
    }
    pub(super) async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.can_manage = can(&self.core, scope, MANAGE_ORDERS).await?;
        if !self.rows_changed {
            return Ok(());
        }
        let p = &self.core.purchasing;
        let company = company_id(scope);
        self.orders = or_empty(
            p.list_orders(
                scope,
                company,
                OrderQuery {
                    status: OrderStatus::parse(self.tab()),
                    ..OrderQuery::default()
                },
            )
            .await,
        )?;
        self.suppliers = if self.can_manage {
            or_empty(p.list_suppliers(scope, company, true).await)?
        } else {
            vec![]
        };
        self.taxes = if self.can_manage {
            or_empty(p.list_tax_codes(scope, company, true).await)?
        } else {
            vec![]
        };
        self.columns.load(&self.orders, |o| o.id.to_string());
        Ok(())
    }
    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        match text(payload, "type") {
            "orders_grid" => {
                self.rows_changed = false;
                if let ColumnsOutcome::Sort(column) =
                    self.columns.handle(&params::string_values(payload))
                {
                    self.list.next_sort(&column);
                }
            }
            "clear-flash" => {
                self.rows_changed = false;
                self.flash.clear(payload.get("key").and_then(Value::as_str));
            }
            "save_order" => {
                self.can_manage = can(&self.core, scope, MANAGE_ORDERS).await?;
                if !self.can_manage {
                    self.flash
                        .error("You do not have permission to draft purchase orders.");
                    return Ok(());
                }
                self.form = payload
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(k, _)| k.starts_with("order["))
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                    .take(64)
                    .collect();
                let value = |key: &str| self.form.get(key).cloned().unwrap_or_default();
                let lines = (0..5)
                    .map(|row| {
                        let v = |field: &str| value(&line_name(row, field));
                        NewOrderLine {
                            description: v("description"),
                            external_item_ref: v("external_item_ref"),
                            quantity: v("quantity"),
                            unit: v("unit"),
                            unit_price: v("unit_price"),
                            tax_code_id: params::positive_id(Some(&v("tax_code_id"))),
                        }
                    })
                    .filter(|l| !l.description.trim().is_empty())
                    .collect();
                let input = NewOrder {
                    supplier_id: id(payload, "order[supplier_id]"),
                    currency_code: value("order[currency_code]"),
                    reference: value("order[reference]"),
                    notes: value("order[notes]"),
                    expected_on: value("order[expected_on]"),
                    lines,
                };
                match outcome(
                    self.core
                        .purchasing
                        .create_order(scope, company_id(scope), &input)
                        .await,
                )? {
                    Ok(o) => {
                        self.redirect = Some((
                            address(o.id),
                            format!("Purchase order {} saved as a draft.", o.order_number),
                        ))
                    }
                    Err(message) => self.flash.error(message),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    pub(super) fn render(&self) -> Result<String> {
        let value = |key: &str| self.form.get(key).cloned().unwrap_or_default();
        let field = |name: &str, title: &str| {
            Field::in_form("order", name)
                .label(title)
                .value(value(&format!("order[{name}]")))
        };
        let mut supplier_field = field("supplier_id", "Supplier")
            .kind(InputKind::Select)
            .prompt("Choose a supplier")
            .options(
                self.suppliers
                    .iter()
                    .map(|s| Choice::new(&s.name, s.id.to_string()))
                    .collect(),
            );
        if self.suppliers.is_empty() {
            supplier_field =
                supplier_field.hint("No active supplier yet; add one under Suppliers.");
        }
        let fields = vec![
            supplier_field,
            field("currency_code", "Currency")
                .attr("maxlength", "3")
                .hint("Three letters, fixed once the order leaves draft."),
            field("reference", "Supplier reference"),
            field("expected_on", "Expected delivery").kind(InputKind::Date),
        ];
        let lines = (0..5)
            .map(|row| {
                [
                    "description",
                    "quantity",
                    "unit",
                    "unit_price",
                    "tax_code_id",
                    "external_item_ref",
                ]
                .map(|name| {
                    let control = Field::named(line_name(row, name))
                        .id(format!("order_lines_{row}_{name}"))
                        .aria_label(format!(
                            "Line {} {}",
                            row + 1,
                            match name {
                                "unit_price" => "unit price",
                                "tax_code_id" => "tax code",
                                "external_item_ref" => "item reference",
                                other => other,
                            }
                        ))
                        .value(value(&line_name(row, name)));
                    match name {
                        "quantity" | "unit_price" => control
                            .attr("inputmode", "decimal")
                            .wrapper_class(if name == "quantity" { "w-24" } else { "w-28" }),
                        "unit" => control.wrapper_class("w-24"),
                        // A grid cell keeps no field margin (`mb-4` by default),
                        // or the select rides above its row's inputs.
                        "tax_code_id" => control
                            .kind(InputKind::Select)
                            .prompt("None")
                            .options(
                                self.taxes
                                    .iter()
                                    .map(|t| Choice::new(&t.code, t.id.to_string()))
                                    .collect(),
                            )
                            .wrapper_class("w-auto"),
                        _ => control.wrapper_class("w-full"),
                    }
                })
            })
            .collect();
        let hrefs: Vec<_> = TABS
            .iter()
            .map(|t| {
                params::address(
                    ORDERS_PATH,
                    &HashMap::from([("status".into(), (*t).into())]),
                )
            })
            .collect();
        let ids: Vec<_> = TABS.iter().map(|t| format!("order-tab-{t}")).collect();
        let tabs = render(&Tabs {
            id: "order-tabs",
            label: "Order statuses",
            class: "mt-5",
            tabs: TABS
                .iter()
                .zip(&hrefs)
                .zip(&ids)
                .map(|((t, h), id)| Tab {
                    id,
                    ..Tab::patch(
                        h,
                        &if *t == "all" { "All".into() } else { label(t) },
                        self.tab() == *t,
                    )
                })
                .collect(),
        })?;
        let mut orders: Vec<_> = self.orders.iter().collect();
        sort(&mut orders, &self.list, |a, b, c| match c {
            "supplier" => supplier(a).cmp(supplier(b)),
            "lines" => a.lines.len().cmp(&b.lines.len()),
            "total" => a.total.cmp(&b.total),
            "expected" => a.expected_on.cmp(&b.expected_on),
            "status" => a.status.as_str().cmp(b.status.as_str()),
            _ => a.order_number.cmp(&b.order_number),
        });
        let rows = orders
            .into_iter()
            .map(|o| {
                Ok(FlexRow::new(o.id.to_string())
                    .slot(
                        "number",
                        render(&RecordLink {
                            workspace: self.framed,
                            kind: crate::MODULE,
                            record_id: &o.id.to_string(),
                            href: &address(o.id),
                            class: "text-link hover:underline",
                            body: Html::text(&format!("PO {}", o.order_number)),
                        })?,
                    )
                    .slot(
                        "supplier",
                        Html::text(o.supplier.as_ref().map_or("—", |s| &s.name)),
                    )
                    .slot("lines", Html::text(&o.lines.len().to_string()))
                    .slot(
                        "total",
                        Html::text(&format!("{} {}", o.total, o.currency_code)),
                    )
                    .slot(
                        "expected",
                        Html::text(o.expected_on.as_deref().unwrap_or("—")),
                    )
                    .slot("status", badge(o.status)?))
            })
            .collect::<Result<Vec<_>>>()?;
        let orders = table(
            "orders-table",
            "Purchase orders",
            "orders_grid",
            &self.columns,
            &self.list,
            rows,
            false,
            EmptyState::missing(
                "No purchase orders here",
                if self.tab() == "all" {
                    "Orders you draft appear here.".into()
                } else {
                    format!("No order is {}.", label(self.tab()).to_lowercase())
                },
            ),
        )?;
        View {
            flash: self.flash.map(),
            can_manage: self.can_manage,
            fields,
            notes: field("notes", "Notes").kind(InputKind::Textarea),
            lines,
            tabs,
            orders,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}
impl LiveState<Scope, Error> for Orders {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(q) => self.list = parse(q)?,
                LiveInput::Event(p) => self.event(request.scope, p).await?,
            }
            self.load(request.scope).await?;
            let (redirect, flash) = self
                .redirect
                .take()
                .map_or((None, None), |(path, message)| {
                    (Some(path), Some((StatusKind::Success, message)))
                });
            Ok(LiveBody {
                html: self.render()?,
                query: self.list.to_params(),
                rows: self.orders.len(),
                redirect,
                flash,
                ..LiveBody::default()
            })
        })
    }
}
fn line_name(row: usize, field: &str) -> String {
    format!("order[lines][{row}][{field}]")
}
