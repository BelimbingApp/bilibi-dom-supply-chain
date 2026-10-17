//! Goods receipts list and over-receipt tolerance form (Bilimbi
//! `Receiving.Web.ReceiptsLive`).
use super::lists::{columns, sort, Table, TwoLineCell};
use super::{badge, capabilities, company_id, label, or_empty, outcome, render, text, Core, Flash};
use crate::contributions::{RECEIPTS_PATH, RECEIVE_PATH};
use crate::{Receipt, ReceiptQuery, ReceiptStatus, Settings, CONFIGURE, MANAGE_RECEIPTS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::ParamNames,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Button, ButtonVariant, ColumnsOutcome, EmptyState, Field, Filter, FlashMap, FlexRow,
    Html, ListColumns, ListSpec, ListState, RecordLink, RowMode, SortDir, Tab, Tabs,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const TABS: [&str; 4] = ["all", "draft", "posted", "cancelled"];
const DENIED: &str = "You do not have permission to configure receiving.";
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("number".into(), SortDir::Desc),
            ("kind".into(), SortDir::Asc),
            ("order".into(), SortDir::Desc),
            ("received".into(), SortDir::Desc),
            ("lines".into(), SortDir::Desc),
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
    .expect("goods receipts list spec")
});
fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(path = "receipts.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    actions: Html,
    can_configure: bool,
    tolerance: String,
    set_by: Option<i64>,
    percent: Field,
    tabs: Html,
    receipts: Html,
}

pub(super) struct Receipts {
    core: Arc<Core>,
    list: ListState,
    columns: ListColumns,
    rows_changed: bool,
    can_manage: bool,
    can_configure: bool,
    framed: bool,
    percent: String,
    settings: Option<Settings>,
    receipts: Vec<Receipt>,
    flash: Flash,
}
impl Receipts {
    pub(super) fn new(
        core: Arc<Core>,
        query: &HashMap<String, String>,
        framed: bool,
    ) -> Result<Self> {
        Ok(Self {
            core,
            list: parse(query)?,
            columns: ListColumns::mount(
                "receipts-table",
                columns(
                    &[
                        ("number", "Number"),
                        ("kind", "Kind"),
                        ("order", "Order"),
                        ("received", "Received on"),
                        ("lines", "Lines"),
                        ("status", "Status"),
                    ],
                    &[],
                ),
            )
            .map_err(|_| Error::Internal)?,
            rows_changed: true,
            can_manage: false,
            can_configure: false,
            framed,
            percent: String::new(),
            settings: None,
            receipts: vec![],
            flash: Flash::default(),
        })
    }
    fn tab(&self) -> &str {
        self.list
            .filters
            .get("status")
            .map_or("all", String::as_str)
    }
    async fn grants(&mut self, scope: &Scope) -> Result<()> {
        let grants = capabilities(&self.core, scope).await?;
        self.can_manage = grants.contains(MANAGE_RECEIPTS);
        self.can_configure = grants.contains(CONFIGURE);
        Ok(())
    }
    pub(super) async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.grants(scope).await?;
        if !self.rows_changed {
            return Ok(());
        }
        let r = &self.core.receiving;
        let company = company_id(scope);
        if self.settings.is_none() {
            self.settings = match r.get_settings(scope, company).await {
                Ok(settings) => Some(settings),
                Err(Error::Forbidden | Error::NotFound) => None,
                Err(e) => return Err(e),
            };
        }
        self.receipts = or_empty(
            r.list_receipts(
                scope,
                company,
                ReceiptQuery {
                    status: ReceiptStatus::parse(self.tab()),
                    ..ReceiptQuery::default()
                },
            )
            .await,
        )?;
        self.columns.load(&self.receipts, |r| r.id.to_string());
        Ok(())
    }
    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        match text(payload, "type") {
            "receipts_grid" => {
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
            "save_settings" => {
                self.rows_changed = false;
                self.grants(scope).await?;
                if !self.can_configure {
                    self.flash.error(DENIED);
                    return Ok(());
                }
                let percent: String = text(payload, "settings[over_receipt_tolerance_percent]")
                    .chars()
                    .take(64)
                    .collect();
                match outcome(
                    self.core
                        .receiving
                        .configure(scope, company_id(scope), &percent)
                        .await,
                )? {
                    Ok(settings) => {
                        self.flash.success(format!(
                            "Over-receipt tolerance set to {} %.",
                            settings.over_receipt_tolerance_percent
                        ));
                        self.settings = Some(settings);
                        self.percent.clear();
                    }
                    Err(message) => {
                        self.percent = percent;
                        self.flash.error(message);
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    pub(super) fn render(&self) -> Result<String> {
        let actions = if self.can_manage {
            render(&Button {
                id: "receive-goods",
                href: RECEIVE_PATH,
                variant: ButtonVariant::Primary,
                body: Html::text("Receive goods"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let hrefs: Vec<_> = TABS
            .iter()
            .map(|t| {
                params::address(
                    RECEIPTS_PATH,
                    &HashMap::from([("status".into(), (*t).into())]),
                )
            })
            .collect();
        let ids: Vec<_> = TABS.iter().map(|t| format!("receipt-tab-{t}")).collect();
        let tabs = render(&Tabs {
            id: "receipt-tabs",
            label: "Receipt statuses",
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
        let compact = self.columns.mode() == RowMode::Compact;
        let mut receipts: Vec<_> = self.receipts.iter().collect();
        sort(&mut receipts, &self.list, |a, b, c| match c {
            "kind" => a.kind.as_str().cmp(b.kind.as_str()),
            "order" => a.order_number.cmp(&b.order_number),
            "received" => a.received_on.cmp(&b.received_on),
            "lines" => a.lines.len().cmp(&b.lines.len()),
            "status" => a.status.as_str().cmp(b.status.as_str()),
            _ => a.receipt_number.cmp(&b.receipt_number),
        });
        let rows = receipts
            .into_iter()
            .map(|r| {
                let of = r
                    .reverses_receipt_number
                    .map(|n| format!("of GR {n}"))
                    .filter(|_| !compact);
                let by = r
                    .reversed_by_receipt_number
                    .map(|n| format!("reversed by GR {n}"))
                    .filter(|_| !compact);
                Ok(FlexRow::new(r.id.to_string())
                    .slot(
                        "number",
                        render(&RecordLink {
                            workspace: self.framed,
                            kind: crate::MODULE,
                            record_id: &r.id.to_string(),
                            href: &super::address(r.id),
                            class: "text-link hover:underline",
                            body: Html::text(&format!("GR {}", r.receipt_number)),
                        })?,
                    )
                    .slot(
                        "kind",
                        render(&TwoLineCell {
                            first: Html::text(&label(r.kind.as_str())),
                            second: of.as_deref(),
                        })?,
                    )
                    .slot("order", Html::text(&format!("PO {}", r.order_number)))
                    .slot("received", Html::text(&r.received_on))
                    .slot("lines", Html::text(&r.lines.len().to_string()))
                    .slot(
                        "status",
                        render(&TwoLineCell {
                            first: badge(r.status)?,
                            second: by.as_deref(),
                        })?,
                    ))
            })
            .collect::<Result<Vec<_>>>()?;
        let receipts = Table {
            id: "receipts-table",
            caption: "Goods receipts",
            event: "receipts_grid",
            columns: &self.columns,
            list: &self.list,
        }
        .render(
            rows,
            EmptyState::missing(
                "No goods receipts here",
                if self.tab() == "all" {
                    "Receipts recorded against purchase orders appear here.".into()
                } else {
                    format!("No receipt is {}.", label(self.tab()).to_lowercase())
                },
            ),
        )?;
        View {
            flash: self.flash.map(),
            actions,
            can_configure: self.can_configure,
            tolerance: self.settings.as_ref().map_or("—".into(), |s| {
                s.over_receipt_tolerance_percent.to_string()
            }),
            set_by: self.settings.as_ref().and_then(|s| s.set_by_user_id),
            percent: Field::in_form("settings", "over_receipt_tolerance_percent")
                .label("Tolerance %")
                .value(&self.percent)
                .attr("inputmode", "decimal")
                .hint("0 to 100, up to four decimal places. Each change is kept as history."),
            tabs,
            receipts,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}
impl LiveState<Scope, Error> for Receipts {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(q) => self.list = parse(q)?,
                LiveInput::Event(p) => self.event(request.scope, p).await?,
            }
            self.load(request.scope).await?;
            Ok(LiveBody {
                html: self.render()?,
                query: self.list.to_params(),
                rows: self.receipts.len(),
                ..LiveBody::default()
            })
        })
    }
}
