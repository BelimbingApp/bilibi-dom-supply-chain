//! `/supply-chain/suppliers` (Bilimbi `Purchasing.Web.SuppliersLive`): the
//! company's suppliers and tax codes, each in a FlexTable, with the add
//! forms and the Deactivate/Activate toggle. Viewing needs
//! `supply-chain.purchasing.view`; adding and toggling need
//! `supply-chain.purchasing.configuration.manage`, read again on every
//! write event and checked by the facade once more.
//!
//! A toggle names its row only; the state it sets is the opposite of what
//! that row showed, as Bilimbi's button carries `active={not active}`.
use super::lists::{columns, sort, table, TwoLineCell};
use super::{can, company_id, id, or_empty, outcome, render, Core, Flash};
use crate::{
    NewSupplier, NewTaxCode, Supplier, SupplierChanges, TaxCode, TaxCodeChanges, CONFIGURE,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::ParamNames,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Badge, BadgeKind, Button, ColumnsOutcome, EmptyState, Field, FlashMap, FlexRow, Html,
    InputKind, ListColumns, ListSpec, ListState, RowMode, SortDir,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const DENIED: &str = "You do not have permission to configure purchasing.";
const SUPPLIER_FIELDS: [&str; 12] = [
    "code",
    "name",
    "contact_name",
    "email",
    "phone",
    "address_line1",
    "address_line2",
    "address_line3",
    "locality",
    "postcode",
    "country_iso",
    "admin1_code",
];
const TAX_CODE_FIELDS: [&str; 3] = ["code", "name", "rate"];

/// Sorting on the columns Bilimbi shows, in memory over the facade's bounded
/// list; the tax codes' names carry a `tax_` prefix so both lists share one
/// address. The facade's order (by code) is the default and leaves the
/// address bare.
pub(super) fn spec(prefix: &str, columns: &[&str]) -> Arc<ListSpec> {
    ListSpec {
        sortable: columns
            .iter()
            .map(|column| ((*column).to_owned(), SortDir::Asc))
            .collect::<BTreeMap<_, _>>(),
        default_sort: Some("code".into()),
        param_prefix: prefix.into(),
        param_names: ParamNames {
            sort_by: "sort".into(),
            sort_dir: "dir".into(),
            ..ParamNames::default()
        },
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("purchasing list spec")
}
static SUPPLIERS: LazyLock<Arc<ListSpec>> =
    LazyLock::new(|| spec("", &["code", "name", "contact", "location", "state"]));
static TAX_CODES: LazyLock<Arc<ListSpec>> =
    LazyLock::new(|| spec("tax_", &["code", "name", "rate", "state"]));

#[derive(Template)]
#[template(path = "suppliers.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    can_configure: bool,
    supplier_fields: Vec<Field>,
    tax_code_fields: Vec<Field>,
    suppliers: Html,
    tax_codes: Html,
}

pub(super) struct Suppliers {
    core: Arc<Core>,
    can_configure: bool,
    supplier_form: HashMap<String, String>,
    tax_code_form: HashMap<String, String>,
    suppliers: Vec<Supplier>,
    tax_codes: Vec<TaxCode>,
    supplier_list: ListState,
    tax_code_list: ListState,
    supplier_columns: ListColumns,
    tax_code_columns: ListColumns,
    /// Whether this update read the rows anew, so the columns forget an
    /// unfinished add box.
    rows_changed: bool,
    flash: Flash,
}

fn parse(query: &HashMap<String, String>, spec: &Arc<ListSpec>) -> Result<ListState> {
    ListState::parse(query, spec.clone()).map_err(|_| Error::Internal)
}

impl Suppliers {
    pub(super) fn new(core: Arc<Core>, query: &HashMap<String, String>) -> Result<Self> {
        let supplier_columns = ListColumns::mount(
            "suppliers-table",
            columns(
                &[
                    ("code", "Code"),
                    ("name", "Name"),
                    ("contact", "Contact"),
                    ("location", "Location"),
                    ("state", "State"),
                ],
                &[],
            ),
        )
        .map_err(|_| Error::Internal)?;
        let tax_code_columns = ListColumns::mount(
            "tax-codes-table",
            columns(
                &[
                    ("code", "Code"),
                    ("name", "Name"),
                    ("rate", "Rate (%)"),
                    ("state", "State"),
                ],
                &["rate"],
            ),
        )
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            core,
            can_configure: false,
            supplier_form: HashMap::new(),
            tax_code_form: HashMap::new(),
            suppliers: Vec::new(),
            tax_codes: Vec::new(),
            supplier_list: parse(query, &SUPPLIERS)?,
            tax_code_list: parse(query, &TAX_CODES)?,
            supplier_columns,
            tax_code_columns,
            rows_changed: true,
            flash: Flash::default(),
        })
    }

    fn query(&self) -> HashMap<String, String> {
        let mut query = self.supplier_list.to_params();
        query.extend(self.tax_code_list.to_params());
        query
    }

    pub(super) async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.can_configure = can(&self.core, scope, CONFIGURE).await?;
        if !self.rows_changed {
            return Ok(());
        }
        let company = company_id(scope);
        let purchasing = &self.core.purchasing;
        self.suppliers = or_empty(purchasing.list_suppliers(scope, company, false).await)?;
        self.tax_codes = or_empty(purchasing.list_tax_codes(scope, company, false).await)?;
        self.supplier_columns
            .load(&self.suppliers, |supplier| supplier.id.to_string());
        self.tax_code_columns
            .load(&self.tax_codes, |tax_code| tax_code.id.to_string());
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        let kind = payload
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Error::Invalid)?;
        match kind {
            "suppliers_grid" => {
                let outcome = self
                    .supplier_columns
                    .handle(&params::string_values(payload));
                self.columns_outcome(outcome, false)?;
            }
            "tax_codes_grid" => {
                let outcome = self
                    .tax_code_columns
                    .handle(&params::string_values(payload));
                self.columns_outcome(outcome, true)?;
            }
            "clear-flash" => {
                self.flash.clear(payload.get("key").and_then(Value::as_str));
                self.rows_changed = false;
            }
            "save_supplier" | "toggle_supplier" | "save_tax_code" | "toggle_tax_code" => {
                self.can_configure = can(&self.core, scope, CONFIGURE).await?;
                if self.can_configure {
                    self.write(scope, kind, payload).await?;
                } else {
                    self.flash.error(DENIED);
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    /// A column operation changes only what is on screen; a sort reorders
    /// the rows already read.
    fn columns_outcome(&mut self, outcome: ColumnsOutcome, tax_codes: bool) -> Result<()> {
        self.rows_changed = false;
        if let ColumnsOutcome::Sort(column) = outcome {
            let list = if tax_codes {
                &mut self.tax_code_list
            } else {
                &mut self.supplier_list
            };
            if !list.next_sort(&column) {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }

    async fn write(&mut self, scope: &Scope, kind: &str, payload: &Value) -> Result<()> {
        let company = company_id(scope);
        let purchasing = &self.core.purchasing;
        match kind {
            "save_supplier" => {
                let form = form_values(payload, "supplier", &SUPPLIER_FIELDS);
                let value = |key: &str| form.get(key).cloned().unwrap_or_default();
                let supplier = NewSupplier {
                    code: value("code"),
                    name: value("name"),
                    contact_name: value("contact_name"),
                    email: value("email"),
                    phone: value("phone"),
                    address_line1: value("address_line1"),
                    address_line2: value("address_line2"),
                    address_line3: value("address_line3"),
                    locality: value("locality"),
                    postcode: value("postcode"),
                    country_iso: value("country_iso"),
                    admin1_code: value("admin1_code"),
                    active: true,
                };
                match outcome(purchasing.create_supplier(scope, company, &supplier).await)? {
                    Ok(supplier) => {
                        self.supplier_form.clear();
                        self.flash
                            .success(format!("Supplier {} added.", supplier.code));
                    }
                    Err(message) => {
                        self.supplier_form = form;
                        self.flash.error(message);
                    }
                }
            }
            "save_tax_code" => {
                let form = form_values(payload, "tax_code", &TAX_CODE_FIELDS);
                let value = |key: &str| form.get(key).cloned().unwrap_or_default();
                let tax_code = NewTaxCode {
                    code: value("code"),
                    name: value("name"),
                    rate: value("rate"),
                    active: true,
                };
                match outcome(purchasing.create_tax_code(scope, company, &tax_code).await)? {
                    Ok(tax_code) => {
                        self.tax_code_form.clear();
                        self.flash
                            .success(format!("Tax code {} added.", tax_code.code));
                    }
                    Err(message) => {
                        self.tax_code_form = form;
                        self.flash.error(message);
                    }
                }
            }
            "toggle_supplier" => {
                let Some(shown) = id(payload, "id")
                    .and_then(|id| self.suppliers.iter().find(|supplier| supplier.id == id))
                else {
                    self.flash.error("That supplier is no longer available.");
                    return Ok(());
                };
                let changes = SupplierChanges {
                    active: Some(!shown.active),
                    ..SupplierChanges::default()
                };
                let result = purchasing
                    .update_supplier(scope, company, shown.id, &changes)
                    .await;
                match outcome(result)? {
                    Ok(supplier) => self.flash.success(format!(
                        "Supplier {} is now {}.",
                        supplier.code,
                        state(supplier.active)
                    )),
                    Err(message) => self.flash.error(message),
                }
            }
            "toggle_tax_code" => {
                let Some(shown) = id(payload, "id")
                    .and_then(|id| self.tax_codes.iter().find(|tax_code| tax_code.id == id))
                else {
                    self.flash.error("That tax code is no longer available.");
                    return Ok(());
                };
                let changes = TaxCodeChanges {
                    active: Some(!shown.active),
                    ..TaxCodeChanges::default()
                };
                let result = purchasing
                    .update_tax_code(scope, company, shown.id, &changes)
                    .await;
                match outcome(result)? {
                    Ok(tax_code) => self.flash.success(format!(
                        "Tax code {} is now {}.",
                        tax_code.code,
                        state(tax_code.active)
                    )),
                    Err(message) => self.flash.error(message),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    pub(super) fn render(&self) -> Result<String> {
        let supplier_fields = supplier_fields(&self.supplier_form);
        let tax_code_fields = tax_code_fields(&self.tax_code_form);
        View {
            flash: self.flash.map(),
            can_configure: self.can_configure,
            supplier_fields,
            tax_code_fields,
            suppliers: self.suppliers_table()?,
            tax_codes: self.tax_codes_table()?,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn suppliers_table(&self) -> Result<Html> {
        let compact = self.supplier_columns.mode() == RowMode::Compact;
        let mut suppliers: Vec<&Supplier> = self.suppliers.iter().collect();
        sort(
            &mut suppliers,
            &self.supplier_list,
            |a: &&Supplier, b: &&Supplier, column| match column {
                "name" => a.name.cmp(&b.name),
                "contact" => a.contact_name.cmp(&b.contact_name),
                "location" => location(a).cmp(&location(b)),
                "state" => b.active.cmp(&a.active),
                _ => a.code.cmp(&b.code),
            },
        );
        let mut rows = Vec::with_capacity(suppliers.len());
        for supplier in suppliers {
            let contact = render(&TwoLineCell {
                first: supplier.contact_name.as_deref().unwrap_or("—"),
                second: supplier.email.as_deref().filter(|_| !compact),
            })?;
            let mut row = FlexRow::new(supplier.id.to_string())
                .slot("code", Html::text(&supplier.code))
                .slot("name", Html::text(&supplier.name))
                .slot("contact", contact)
                .slot("location", Html::text(&location(supplier)))
                .slot("state", badge(supplier.active)?);
            if self.can_configure {
                row.actions = toggle(
                    &format!("supplier-toggle-{}", supplier.id),
                    "toggle_supplier",
                    supplier.id,
                    supplier.active,
                )?;
            }
            rows.push(row);
        }
        let reason = if self.can_configure {
            "Add the first supplier above."
        } else {
            "An operator with the configuration permission adds suppliers."
        };
        table(
            "suppliers-table",
            "Suppliers",
            "suppliers_grid",
            &self.supplier_columns,
            &self.supplier_list,
            rows,
            self.can_configure,
            EmptyState::missing("No suppliers yet", reason),
        )
    }

    fn tax_codes_table(&self) -> Result<Html> {
        let mut tax_codes: Vec<&TaxCode> = self.tax_codes.iter().collect();
        sort(
            &mut tax_codes,
            &self.tax_code_list,
            |a: &&TaxCode, b: &&TaxCode, column| match column {
                "name" => a.name.cmp(&b.name),
                "rate" => a.rate.cmp(&b.rate),
                "state" => b.active.cmp(&a.active),
                _ => a.code.cmp(&b.code),
            },
        );
        let mut rows = Vec::with_capacity(tax_codes.len());
        for tax_code in tax_codes {
            let mut row = FlexRow::new(tax_code.id.to_string())
                .slot("code", Html::text(&tax_code.code))
                .slot("name", Html::text(&tax_code.name))
                .slot("rate", Html::text(&tax_code.rate.to_string()))
                .slot("state", badge(tax_code.active)?);
            if self.can_configure {
                row.actions = toggle(
                    &format!("tax-code-toggle-{}", tax_code.id),
                    "toggle_tax_code",
                    tax_code.id,
                    tax_code.active,
                )?;
            }
            rows.push(row);
        }
        table(
            "tax-codes-table",
            "Tax codes",
            "tax_codes_grid",
            &self.tax_code_columns,
            &self.tax_code_list,
            rows,
            self.can_configure,
            EmptyState::missing(
                "No tax codes yet",
                "Order lines without a tax code carry no tax.",
            ),
        )
    }
}

impl LiveState<Scope, Error> for Suppliers {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.supplier_list = parse(values, &SUPPLIERS)?;
                    self.tax_code_list = parse(values, &TAX_CODES)?;
                }
                LiveInput::Event(payload) => self.event(request.scope, payload).await?,
            }
            self.load(request.scope).await?;
            Ok(LiveBody {
                html: self.render()?,
                query: self.query(),
                rows: self.suppliers.len() + self.tax_codes.len(),
                ..LiveBody::default()
            })
        })
    }
}

fn state(active: bool) -> &'static str {
    if active {
        "active"
    } else {
        "inactive"
    }
}

/// The locality and country, as Bilimbi joins the present ones.
fn location(supplier: &Supplier) -> String {
    [
        supplier.locality.as_deref(),
        supplier.country_iso.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ")
}

fn badge(active: bool) -> Result<Html> {
    render(&if active {
        Badge::status(BadgeKind::Success, "Active")
    } else {
        Badge::status(BadgeKind::Neutral, "Inactive")
    })
}

fn toggle(dom_id: &str, event: &str, id: i64, active: bool) -> Result<Html> {
    let value = id.to_string();
    render(&Button {
        id: dom_id,
        button_type: "button",
        event,
        name: "id",
        value: &value,
        body: Html::text(if active { "Deactivate" } else { "Activate" }),
        ..Button::default()
    })
}

fn supplier_fields(form: &HashMap<String, String>) -> Vec<Field> {
    SUPPLIER_FIELDS
        .iter()
        .map(|key| {
            let field =
                Field::in_form("supplier", key).value(form.get(*key).cloned().unwrap_or_default());
            match *key {
                "code" => field.label("Code").hint("Fixed once saved."),
                "name" => field.label("Name"),
                "contact_name" => field.label("Contact name"),
                "email" => field.label("Email").kind(InputKind::Email),
                "phone" => field.label("Phone"),
                "address_line1" => field.label("Address line 1"),
                "address_line2" => field.label("Address line 2"),
                "address_line3" => field.label("Address line 3"),
                "locality" => field.label("Locality"),
                "postcode" => field.label("Postcode"),
                "country_iso" => field
                    .label("Country (ISO code)")
                    .hint("Two letters, such as the code Core Geonames lists."),
                _ => field.label("Division code"),
            }
        })
        .collect()
}

fn tax_code_fields(form: &HashMap<String, String>) -> Vec<Field> {
    TAX_CODE_FIELDS
        .iter()
        .map(|key| {
            let field =
                Field::in_form("tax_code", key).value(form.get(*key).cloned().unwrap_or_default());
            match *key {
                "code" => field.label("Code").hint("Fixed once saved."),
                "name" => field.label("Name"),
                _ => field
                    .label("Rate (%)")
                    .attr("inputmode", "decimal")
                    .hint("A line keeps the rate it was priced with."),
            }
        })
        .collect()
}

/// Keep each submitted field intact for the facade's codepoint validation.
/// The shared query-form coercion drops values over 1 KiB; optional address
/// fields must instead be refused when too long, never silently discarded.
fn form_values(payload: &Value, namespace: &str, fields: &[&str]) -> HashMap<String, String> {
    fields
        .iter()
        .filter_map(|field| {
            let value = payload.get(format!("{namespace}[{field}]"))?.as_str()?;
            Some(((*field).to_owned(), value.to_owned()))
        })
        .collect()
}
