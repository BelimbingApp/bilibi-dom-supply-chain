//! One goods receipt with post, cancel and reverse (Bilimbi
//! `Receiving.Web.ReceiptLive`). Every action waits for confirmation; the
//! page keeps the action and its reason on the server until the dialog
//! answers, and checks the action's capability again on both events.
use super::lists::{sort, Grid};
use super::{address, badge, capabilities, company_id, label, outcome, render, text, Core, Flash};
use crate::contributions::RECEIPTS_PATH;
use crate::{Receipt, ReceiptKind, ReceiptStatus, Reversal, MANAGE_RECEIPTS, POST_RECEIPTS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision, Rendered};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BackLink, Button, ButtonVariant, ConfirmDialog, EmptyState, Fact, Facts, Field,
    FlashMap, FlexRow, Html, InputKind, StatusKind,
};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, time::SystemTime};

const DENIED: &str = "You do not have permission to change goods receipts.";

#[derive(Clone, Copy)]
enum Action {
    Post,
    Cancel,
    Reverse,
}
impl Action {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "post" => Self::Post,
            "cancel" => Self::Cancel,
            "reverse" => Self::Reverse,
            _ => return None,
        })
    }
    fn key(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::Cancel => "cancel",
            Self::Reverse => "reverse",
        }
    }
    /// Cancelling a draft is managing receipts; posting and reversing are
    /// posting them.
    fn permitted(self, manage: bool, post: bool) -> bool {
        match self {
            Self::Cancel => manage,
            Self::Post | Self::Reverse => post,
        }
    }
}
struct Pending {
    action: Action,
    reason: String,
}

#[derive(Template)]
#[template(path = "receipt.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    receipt: Option<&'a Receipt>,
    back: Html,
    status_line: Html,
    facts: Html,
    lines: Html,
    actions: Html,
    history: Html,
    dialog: Html,
    missing: Html,
}
#[derive(Template)]
#[template(path = "receipt_actions.html")]
struct Actions {
    post: Html,
    forms: Vec<ActionForm>,
    final_status: Option<String>,
    forbidden: bool,
}
struct ActionForm {
    id: &'static str,
    action: Field,
    reason: Field,
    button: Html,
}
#[derive(Template)]
#[template(path = "instant.html")]
struct Instant {
    id: String,
    value: Rendered,
}
#[derive(Template)]
#[template(path = "receipt_subtitle.html")]
struct Subtitle {
    badge: Html,
    kind: String,
    order_number: i64,
}
#[derive(Template)]
#[template(path = "receipt_link.html")]
struct Link<'a> {
    id: &'a str,
    href: &'a str,
    body: String,
    after: Html,
}

pub(super) struct ReceiptPage {
    core: Arc<Core>,
    id: i64,
    receipt: Option<Receipt>,
    can_manage: bool,
    can_post: bool,
    pending: Option<Pending>,
    flash: Flash,
    redirect: Option<(String, String)>,
    lines: Grid,
    history: Grid,
    display: DisplayContext,
}
impl ReceiptPage {
    pub(super) fn new(
        core: Arc<Core>,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        Ok(Self {
            core,
            id: params::positive_id(path.get("id").map(String::as_str)).unwrap_or(0),
            receipt: None,
            can_manage: false,
            can_post: false,
            pending: None,
            flash: Flash::default(),
            redirect: None,
            lines: Grid::new(
                "receipt-lines-table",
                "lines_grid",
                "lines",
                &[
                    ("number", "#"),
                    ("order_line", "Order line"),
                    ("description", "Description"),
                    ("ordered", "Ordered"),
                    ("accepted", "Accepted"),
                    ("rejected", "Rejected"),
                    ("reason", "Rejection reason"),
                ],
                &["ordered", "accepted", "rejected"],
                query,
            )?,
            history: Grid::new(
                "receipt-events",
                "history_grid",
                "history",
                &[
                    ("at", "When"),
                    ("status", "Status"),
                    ("from", "From"),
                    ("actor", "By"),
                    ("reason", "Reason"),
                ],
                &[],
                query,
            )?,
            display: DisplayContext::anonymous(),
        })
    }
    pub(super) fn title(&self) -> String {
        self.receipt.as_ref().map_or("Goods receipt".into(), |r| {
            format!("Goods receipt {}", r.receipt_number)
        })
    }
    async fn grants(&mut self, scope: &Scope) -> Result<()> {
        let grants = capabilities(&self.core, scope).await?;
        self.can_manage = grants.contains(MANAGE_RECEIPTS);
        self.can_post = grants.contains(POST_RECEIPTS);
        Ok(())
    }
    pub(super) async fn load(&mut self, scope: &Scope, rows_changed: bool) -> Result<()> {
        self.grants(scope).await?;
        let client = self.core.db.get().await?;
        let locale = self
            .core
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        self.display = self
            .core
            .display
            .display(&**client, scope, None, locale)
            .await?;
        if !rows_changed {
            return Ok(());
        }
        self.receipt = match self
            .core
            .receiving
            .get_receipt(scope, company_id(scope), self.id)
            .await
        {
            Ok(r) => r,
            Err(Error::NotFound | Error::Forbidden) => None,
            Err(e) => return Err(e),
        };
        if let Some(r) = &self.receipt {
            self.lines.columns.load(&r.lines, |l| l.id.to_string());
            self.history.columns.load(&r.events, |e| e.id.to_string());
        } else {
            self.pending = None;
        }
        Ok(())
    }
    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<bool> {
        match text(payload, "type") {
            "lines_grid" => self.lines.event(payload),
            "history_grid" => self.history.event(payload),
            "clear-flash" => self.flash.clear(payload.get("key").and_then(Value::as_str)),
            "dismiss" => self.pending = None,
            kind @ ("transition" | "confirm") => {
                self.grants(scope).await?;
                let action = if kind == "transition" {
                    Action::parse(text(payload, "action"))
                } else {
                    self.pending.as_ref().map(|p| p.action)
                };
                // A confirm with nothing pending does nothing; an unknown
                // action is refused like one the scope may not take. A
                // refusal keeps the held action, so the dialog stays open
                // beside the flash, as Bilimbi's.
                let permitted = action.is_some_and(|a| a.permitted(self.can_manage, self.can_post));
                if !permitted && (kind == "transition" || action.is_some()) {
                    self.flash.error(DENIED);
                    return Ok(true);
                }
                if let Some(action) = action {
                    if kind == "transition" {
                        self.pending = Some(Pending {
                            action,
                            reason: text(payload, "reason").chars().take(4096).collect(),
                        });
                    } else if let Some(pending) = self.pending.take() {
                        self.run(scope, pending).await?;
                    }
                }
                return Ok(true);
            }
            _ => return Err(Error::Invalid),
        }
        Ok(false)
    }
    /// The facade authorizes each action for the scope's actor when it runs
    /// and records who performed it; the page passes no actor.
    async fn run(&mut self, scope: &Scope, pending: Pending) -> Result<()> {
        let r = &self.core.receiving;
        let company = company_id(scope);
        let id = self.id;
        match pending.action {
            Action::Post => match outcome(r.post_receipt(scope, company, id).await)? {
                Ok(_) => self
                    .flash
                    .success("Receipt posted; the order's received quantities are updated."),
                Err(message) => self.flash.error(message),
            },
            Action::Cancel => {
                match outcome(r.cancel_receipt(scope, company, id, &pending.reason).await)? {
                    Ok(_) => self.flash.success("Draft receipt cancelled."),
                    Err(message) => self.flash.error(message),
                }
            }
            Action::Reverse => {
                let reversal = Reversal {
                    reason: pending.reason,
                    ..Reversal::default()
                };
                match outcome(r.reverse_receipt(scope, company, id, &reversal).await)? {
                    Ok(reversal) => {
                        self.redirect = Some((
                            address(reversal.id),
                            format!(
                                "Receipt reversed by goods receipt {}.",
                                reversal.receipt_number
                            ),
                        ))
                    }
                    Err(message) => self.flash.error(message),
                }
            }
        }
        Ok(())
    }
    fn instant(&self, id: String, at: SystemTime) -> Result<Html> {
        render(&Instant {
            id,
            value: self.display.render(
                jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?,
                Format::DateTime,
                Precision::Second,
            ),
        })
    }
    fn facts(&self, r: &Receipt) -> Result<Html> {
        let actor = |user: Option<i64>, impersonator: Option<i64>| {
            format!(
                "user {}{}",
                user.map_or("—".into(), |u| u.to_string()),
                impersonator.map_or(String::new(), |id| format!(" (as user {id})"))
            )
        };
        let link = |id: &str, href: &str, body: String, after: Html| {
            render(&Link {
                id,
                href,
                body,
                after,
            })
        };
        let mut items = vec![
            Fact {
                title: "Purchase order",
                value: link(
                    "receipt-order-link",
                    &format!("/supply-chain/purchase-orders/{}", r.order_id),
                    format!("PO {}", r.order_number),
                    Html::EMPTY,
                )?,
                ..Fact::default()
            },
            Fact {
                title: "Received on",
                value: Html::text(&r.received_on),
                ..Fact::default()
            },
            Fact {
                title: "Delivery note",
                value: Html::text(r.supplier_reference.as_deref().unwrap_or("—")),
                ..Fact::default()
            },
            Fact {
                title: "Recorded by",
                value: Html::text(&actor(
                    Some(r.created_by_user_id),
                    r.created_by_impersonator_id,
                )),
                ..Fact::default()
            },
        ];
        if let Some(at) = r.posted_at {
            let mut value = Html::text(&format!(
                "by {} ",
                actor(r.posted_by_user_id, r.posted_by_impersonator_id)
            ));
            value.push(&self.instant("receipt-posted-at".into(), at)?);
            items.push(Fact {
                title: "Posted",
                value,
                ..Fact::default()
            });
        }
        if let Some(original) = r.reverses_receipt_id {
            items.push(Fact {
                title: "Reverses",
                value: link(
                    "receipt-reverses-link",
                    &address(original),
                    format!("GR {}", r.reverses_receipt_number.unwrap_or_default()),
                    Html::text(r.reason.as_deref().unwrap_or_default()),
                )?,
                ..Fact::default()
            });
        }
        if let Some(reversal) = r.reversed_by_receipt_id {
            items.push(Fact {
                title: "Reversed by",
                value: link(
                    "receipt-reversed-by-link",
                    &address(reversal),
                    format!("GR {}", r.reversed_by_receipt_number.unwrap_or_default()),
                    Html::EMPTY,
                )?,
                ..Fact::default()
            });
        }
        items.push(Fact {
            title: "Notes",
            value: Html::text(r.notes.as_deref().unwrap_or("—")),
            ..Fact::default()
        });
        render(&Facts {
            id: "receipt-facts",
            items,
        })
    }
    fn actions(&self, r: &Receipt) -> Result<Html> {
        let reversible = reversible(r);
        let draft = r.status == ReceiptStatus::Draft;
        let mut forms = vec![];
        let mut add = |id: &'static str, label: &str, action: Action, reason_label: &str| {
            forms.push(ActionForm {
                id,
                action: Field::named("action")
                    .id(format!("{id}-action"))
                    .kind(InputKind::Hidden)
                    .value(action.key()),
                reason: Field::named("reason")
                    .id(format!("{id}-reason"))
                    .aria_label(reason_label)
                    .placeholder("Reason")
                    .attr("maxlength", "500")
                    .wrapper_class("w-48"),
                button: render(&Button {
                    button_type: "submit",
                    variant: ButtonVariant::Danger,
                    body: Html::text(label),
                    ..Button::default()
                })?,
            });
            Ok::<_, Error>(())
        };
        if self.can_manage && draft {
            add(
                "receipt-cancel",
                "Cancel receipt",
                Action::Cancel,
                "Cancellation reason",
            )?;
        }
        if self.can_post && reversible {
            add(
                "receipt-reverse",
                "Reverse receipt",
                Action::Reverse,
                "Reversal reason",
            )?;
        }
        let post = if self.can_post && draft {
            render(&Button {
                id: "receipt-post",
                button_type: "button",
                variant: ButtonVariant::Primary,
                event: "transition",
                name: "action",
                value: Action::Post.key(),
                body: Html::text("Post receipt"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let finished = r.status == ReceiptStatus::Cancelled
            || (r.status == ReceiptStatus::Posted && !reversible);
        render(&Actions {
            post,
            forms,
            final_status: finished.then(|| {
                if r.reversed_by_receipt_id.is_some() {
                    "reversed".into()
                } else {
                    label(r.status.as_str()).to_lowercase()
                }
            }),
            forbidden: !self.can_manage && !self.can_post && (draft || reversible),
        })
    }
    fn dialog(&self, r: &Receipt) -> Result<Html> {
        let Some(pending) = &self.pending else {
            return Ok(Html::EMPTY);
        };
        let n = r.receipt_number;
        let po = r.order_number;
        let (consequence, detail, confirm, working) = match pending.action {
            Action::Post => (
                format!("Goods receipt {n} will be posted against PO {po}."),
                "Its accepted quantities count as received on the order, and the receipt is fixed from then on; a mistake is corrected by a reversal. This cannot be undone.".to_owned(),
                "Post",
                "Posting…",
            ),
            Action::Cancel => (
                format!("Draft goods receipt {n} will be cancelled."),
                "It keeps its number and history and is never posted. This cannot be undone.".to_owned(),
                "Cancel receipt",
                "Cancelling…",
            ),
            Action::Reverse => (
                format!("Goods receipt {n} will be reversed."),
                format!("A reversal receipt takes its accepted quantities back off PO {po}; the goods are received again with a new receipt. This cannot be undone."),
                "Reverse",
                "Reversing…",
            ),
        };
        render(&ConfirmDialog {
            id: "receipt-confirm",
            consequence: &consequence,
            detail: &detail,
            confirm,
            working,
            confirm_event: "confirm",
            cancel_event: "dismiss",
        })
    }
    pub(super) fn render(&self) -> Result<String> {
        let mut view = View {
            flash: self.flash.map(),
            receipt: self.receipt.as_ref(),
            back: render(&BackLink {
                href: RECEIPTS_PATH,
                title: "Back to goods receipts",
                ..BackLink::default()
            })?,
            status_line: Html::EMPTY,
            facts: Html::EMPTY,
            lines: Html::EMPTY,
            actions: Html::EMPTY,
            history: Html::EMPTY,
            dialog: Html::EMPTY,
            missing: render(
                &EmptyState::missing(
                    "This goods receipt is not available.",
                    "It may belong to another company, or the link is out of date.",
                )
                .id("receipt-missing")
                .action(render(&Button {
                    href: RECEIPTS_PATH,
                    body: Html::text("Back to goods receipts"),
                    ..Button::default()
                })?),
            )?,
        };
        let Some(r) = &self.receipt else {
            return view.render().map_err(|_| Error::Internal);
        };
        view.status_line = render(&Subtitle {
            badge: badge(r.status)?,
            kind: label(r.kind.as_str()),
            order_number: r.order_number,
        })?;
        view.facts = self.facts(r)?;
        let mut lines: Vec<_> = r.lines.iter().collect();
        sort(&mut lines, &self.lines.list, |a, b, c| match c {
            "order_line" => a.order_line_number.cmp(&b.order_line_number),
            "description" => a.description.cmp(&b.description),
            "ordered" => a.ordered_quantity.cmp(&b.ordered_quantity),
            "accepted" => a.received_quantity.cmp(&b.received_quantity),
            "rejected" => a.rejected_quantity.cmp(&b.rejected_quantity),
            "reason" => a.rejection_reason.cmp(&b.rejection_reason),
            _ => a.line_number.cmp(&b.line_number),
        });
        let rows = lines
            .into_iter()
            .map(|l| {
                FlexRow::new(l.id.to_string())
                    .slot("number", Html::text(&l.line_number.to_string()))
                    .slot("order_line", Html::text(&l.order_line_number.to_string()))
                    .slot("description", Html::text(&l.description))
                    .slot(
                        "ordered",
                        Html::text(&format!("{} {}", l.ordered_quantity, l.unit)),
                    )
                    .slot("accepted", Html::text(&l.received_quantity.to_string()))
                    .slot("rejected", Html::text(&l.rejected_quantity.to_string()))
                    .slot(
                        "reason",
                        Html::text(l.rejection_reason.as_deref().unwrap_or("—")),
                    )
            })
            .collect();
        view.lines = self.lines.table(
            "Receipt lines",
            rows,
            EmptyState::missing("No lines", "A receipt records at least one line."),
        )?;
        view.actions = self.actions(r)?;
        let mut events: Vec<_> = r.events.iter().collect();
        sort(&mut events, &self.history.list, |a, b, c| match c {
            "status" => a.to_status.as_str().cmp(b.to_status.as_str()),
            "from" => a
                .from_status
                .map(ReceiptStatus::as_str)
                .cmp(&b.from_status.map(ReceiptStatus::as_str)),
            "actor" => a.actor_user_id.cmp(&b.actor_user_id),
            "reason" => a.reason.cmp(&b.reason),
            _ => a.occurred_at.cmp(&b.occurred_at),
        });
        let rows = events
            .into_iter()
            .map(|e| {
                Ok(FlexRow::new(format!("receipt-event-{}", e.id))
                    .slot(
                        "at",
                        self.instant(format!("receipt-event-at-{}", e.id), e.occurred_at)?,
                    )
                    .slot("status", badge(e.to_status)?)
                    .slot(
                        "from",
                        Html::text(
                            &e.from_status
                                .map_or("—".into(), |s| format!("from {}", label(s.as_str()))),
                        ),
                    )
                    .slot(
                        "actor",
                        Html::text(&format!(
                            "by user {}{}",
                            e.actor_user_id,
                            e.actor_impersonator_id
                                .map_or(String::new(), |id| format!(" (as user {id})"))
                        )),
                    )
                    .slot("reason", Html::text(e.reason.as_deref().unwrap_or("—"))))
            })
            .collect::<Result<Vec<_>>>()?;
        view.history = self.history.table(
            "History",
            rows,
            EmptyState::missing("No history", "Status changes appear here."),
        )?;
        view.dialog = self.dialog(r)?;
        view.render().map_err(|_| Error::Internal)
    }
}
impl LiveState<Scope, Error> for ReceiptPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let changed = match request.input {
                LiveInput::Refresh => true,
                LiveInput::Params(q) => {
                    self.lines.params(q)?;
                    self.history.params(q)?;
                    self.pending = None;
                    true
                }
                LiveInput::Event(p) => self.event(request.scope, p).await?,
            };
            self.load(request.scope, changed).await?;
            let mut query = self.lines.list.to_params();
            query.extend(self.history.list.to_params());
            let (redirect, flash) = self
                .redirect
                .take()
                .map_or((None, None), |(path, message)| {
                    (Some(path), Some((StatusKind::Success, message)))
                });
            Ok(LiveBody {
                html: self.render()?,
                query,
                redirect,
                flash,
                ..LiveBody::default()
            })
        })
    }
}

/// A posted original that no reversal has taken back yet.
fn reversible(r: &Receipt) -> bool {
    r.status == ReceiptStatus::Posted
        && r.kind == ReceiptKind::Receipt
        && r.reversed_by_receipt_id.is_none()
}
