//! Purchase requests: Bilimbi's `Request` and `RequestLine` changeset rules,
//! the lifecycle (draft, submitted, approved, rejected, cancelled) and the
//! tables' SQL. Every creation and transition writes its history row in the
//! same mutation; the database refuses a commit without one, numbers the
//! requests without gaps, and freezes a request and its lines once it leaves
//! draft (migration 002). Private to the facade.
use crate::validate::{date, optional_text, quantity, reason, required_text, Checks};
use crate::{
    FieldError, NewRequest, NewRequestLine, Outcome, Refusal, Request, RequestChanges,
    RequestEvent, RequestLine, RequestQuery, RequestStatus,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use tokio_postgres::{types::ToSql, GenericClient, Row};

const COLUMNS: &str = "id,request_number,status,title,justification,to_char(needed_on,'YYYY-MM-DD'),created_by_user_id,created_by_impersonator_id,submitted_by_user_id,submitted_at,decided_by_user_id,decided_at,decision_reason,inserted_at,updated_at";
const LINE_COLUMNS: &str = "id,line_number,description,external_item_ref,quantity,unit";
const EVENT_COLUMNS: &str =
    "id,from_status,to_status,actor_user_id,actor_impersonator_id,reason,occurred_at";

fn status(value: &str) -> Result<RequestStatus> {
    RequestStatus::parse(value).ok_or(Error::Internal)
}

fn from_row(row: &Row) -> Result<Request> {
    Ok(Request {
        id: row.try_get(0)?,
        request_number: row.try_get(1)?,
        status: status(row.try_get(2)?)?,
        title: row.try_get(3)?,
        justification: row.try_get(4)?,
        needed_on: row.try_get(5)?,
        created_by_user_id: row.try_get(6)?,
        created_by_impersonator_id: row.try_get(7)?,
        submitted_by_user_id: row.try_get(8)?,
        submitted_at: row.try_get(9)?,
        decided_by_user_id: row.try_get(10)?,
        decided_at: row.try_get(11)?,
        decision_reason: row.try_get(12)?,
        inserted_at: row.try_get(13)?,
        updated_at: row.try_get(14)?,
        lines: Vec::new(),
        events: Vec::new(),
    })
}

fn line_from_row(row: &Row) -> Result<RequestLine> {
    Ok(RequestLine {
        id: row.try_get(0)?,
        line_number: row.try_get(1)?,
        description: row.try_get(2)?,
        external_item_ref: row.try_get(3)?,
        quantity: row.try_get(4)?,
        unit: row.try_get(5)?,
    })
}

fn event_from_row(row: &Row) -> Result<RequestEvent> {
    Ok(RequestEvent {
        id: row.try_get(0)?,
        from_status: row.try_get::<_, Option<&str>>(1)?.map(status).transpose()?,
        to_status: status(row.try_get(2)?)?,
        actor_user_id: row.try_get(3)?,
        actor_impersonator_id: row.try_get(4)?,
        reason: row.try_get(5)?,
        occurred_at: row.try_get(6)?,
    })
}

/// The audit view of a request header: its facts and decision, not its
/// bookkeeping stamps.
fn to_json(request: &Request) -> Value {
    json!({
        "id": request.id, "request_number": request.request_number,
        "status": request.status.as_str(), "title": request.title,
        "justification": request.justification, "needed_on": request.needed_on,
        "created_by_user_id": request.created_by_user_id,
        "created_by_impersonator_id": request.created_by_impersonator_id,
        "submitted_by_user_id": request.submitted_by_user_id,
        "decided_by_user_id": request.decided_by_user_id,
        "decision_reason": request.decision_reason,
    })
}

fn line_json(request_id: i64, line: &RequestLine) -> Value {
    json!({
        "id": line.id, "request_id": request_id, "line_number": line.line_number,
        "description": line.description, "external_item_ref": line.external_item_ref,
        "quantity": line.quantity.to_string(), "unit": line.unit,
    })
}

fn event_json(request_id: i64, event: &RequestEvent) -> Value {
    json!({
        "id": event.id, "request_id": request_id,
        "from_status": event.from_status.map(RequestStatus::as_str),
        "to_status": event.to_status.as_str(), "actor_user_id": event.actor_user_id,
        "actor_impersonator_id": event.actor_impersonator_id, "reason": event.reason,
    })
}

async fn lines(client: &(impl GenericClient + Sync), request_id: i64) -> Result<Vec<RequestLine>> {
    client
        .query(
            &format!("SELECT {LINE_COLUMNS} FROM supply_chain_purchasing_request_lines WHERE request_id=$1 ORDER BY line_number"),
            &[&request_id],
        )
        .await?
        .iter()
        .map(line_from_row)
        .collect()
}

async fn events(client: &(impl GenericClient + Sync), request_id: i64) -> Result<Vec<RequestEvent>> {
    client
        .query(
            &format!("SELECT {EVENT_COLUMNS} FROM supply_chain_purchasing_request_events WHERE request_id=$1 ORDER BY id"),
            &[&request_id],
        )
        .await?
        .iter()
        .map(event_from_row)
        .collect()
}

pub(crate) async fn list(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    query: RequestQuery,
) -> Result<Vec<Request>> {
    let status = query.status.map(RequestStatus::as_str);
    let mut requests = client
        .query(
            &format!(
                "SELECT {COLUMNS} FROM supply_chain_purchasing_requests \
                 WHERE tenant_id=$1 AND company_id=$2 AND ($3::text IS NULL OR status=$3) \
                 ORDER BY request_number DESC LIMIT $4"
            ),
            &[&tenant, &company_id, &status, &query.limit],
        )
        .await?
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>>>()?;
    let ids: Vec<i64> = requests.iter().map(|request| request.id).collect();
    let rows = client
        .query(
            &format!("SELECT request_id,{LINE_COLUMNS} FROM supply_chain_purchasing_request_lines WHERE request_id = ANY($1) ORDER BY request_id,line_number"),
            &[&ids],
        )
        .await?;
    for row in &rows {
        let request_id: i64 = row.try_get(0)?;
        let line = RequestLine {
            id: row.try_get(1)?,
            line_number: row.try_get(2)?,
            description: row.try_get(3)?,
            external_item_ref: row.try_get(4)?,
            quantity: row.try_get(5)?,
            unit: row.try_get(6)?,
        };
        if let Some(request) = requests.iter_mut().find(|request| request.id == request_id) {
            request.lines.push(line);
        }
    }
    Ok(requests)
}

/// The header only, `FOR UPDATE` when `lock`: a write reads its request
/// locked, so a transition is judged on the status it will change.
async fn header(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    request_id: i64,
    lock: bool,
) -> Result<Option<Request>> {
    client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM supply_chain_purchasing_requests WHERE tenant_id=$1 AND company_id=$2 AND id=$3{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            &[&tenant, &company_id, &request_id],
        )
        .await?
        .as_ref()
        .map(from_row)
        .transpose()
}

pub(crate) async fn get(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    company_id: i64,
    request_id: i64,
) -> Result<Option<Request>> {
    let Some(mut request) = header(client, tenant, company_id, request_id, false).await? else {
        return Ok(None);
    };
    request.lines = lines(client, request.id).await?;
    request.events = events(client, request.id).await?;
    Ok(Some(request))
}

/// The locked request with its lines, or the refusal a missing one is.
pub(crate) async fn locked(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    request_id: i64,
) -> Result<std::result::Result<Request, Refusal>> {
    let Some(mut request) = header(tx.sql(), scope.tenant(), company_id, request_id, true).await?
    else {
        return Ok(Err(Refusal::RequestNotFound));
    };
    request.lines = lines(tx.sql(), request.id).await?;
    Ok(Ok(request))
}

fn require_status(
    request: &Request,
    allowed: &[RequestStatus],
    target: &'static str,
) -> std::result::Result<(), Refusal> {
    if allowed.contains(&request.status) {
        Ok(())
    } else {
        Err(Refusal::InvalidTransition {
            status: request.status.as_str(),
            target,
        })
    }
}

/// A request line as `RequestLine.changeset/2` accepts it.
#[derive(Default)]
struct ValidLine {
    description: String,
    external_item_ref: Option<String>,
    quantity: Decimal,
    unit: String,
}

fn validate_lines(checks: &mut Checks, lines: &[NewRequestLine]) -> Vec<ValidLine> {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let number = index + 1;
            ValidLine {
                description: checks
                    .take_line(number, required_text("description", &line.description, 500)),
                external_item_ref: checks.take_line(
                    number,
                    optional_text("external_item_ref", &line.external_item_ref, 100),
                ),
                quantity: checks.take_line(number, quantity("quantity", &line.quantity)),
                unit: checks.take_line(number, required_text("unit", &line.unit, 20)),
            }
        })
        .collect()
}

fn title(value: &str) -> std::result::Result<String, FieldError> {
    required_text("title", value, 200)
}

fn justification(value: &str) -> std::result::Result<Option<String>, FieldError> {
    optional_text("justification", value, 1000)
}

/// Who acts, from the sealed scope: the signed-in account and any
/// impersonating operator.
fn actor(scope: &Scope) -> (i64, Option<i64>) {
    (
        scope.user(),
        scope.impersonator().map(|operator| operator.user()),
    )
}

/// Inserts `lines` as the request's lines 1, 2, 3, ... and returns them
/// with their `Change`s.
async fn insert_lines(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    company_id: i64,
    request_id: i64,
    valid: &[ValidLine],
    changes: &mut Vec<Change>,
) -> Result<Vec<RequestLine>> {
    let mut inserted = Vec::with_capacity(valid.len());
    for (index, line) in valid.iter().enumerate() {
        let number = i32::try_from(index + 1).map_err(|_| Error::Invalid)?;
        let row = sql
            .query_one(
                &format!(
                    "INSERT INTO supply_chain_purchasing_request_lines(tenant_id,company_id,request_id,line_number,description,external_item_ref,quantity,unit) \
                     VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING {LINE_COLUMNS}"
                ),
                &[
                    &tenant, &company_id, &request_id, &number, &line.description,
                    &line.external_item_ref, &line.quantity, &line.unit,
                ],
            )
            .await?;
        let line = line_from_row(&row)?;
        changes.push(Change {
            kind: "supply_chain.purchasing.request_line".into(),
            id: line.id,
            old: json!({}),
            new: line_json(request_id, &line),
        });
        inserted.push(line);
    }
    Ok(inserted)
}

/// Appends the history row for `request` having become its current status
/// from `from`; the database stamps its time with `clock_timestamp()`.
async fn insert_event(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    company_id: i64,
    request: &Request,
    from: Option<RequestStatus>,
    actor: (i64, Option<i64>),
    reason: Option<&str>,
) -> Result<Change> {
    let row = sql
        .query_one(
            &format!(
                "INSERT INTO supply_chain_purchasing_request_events(tenant_id,company_id,request_id,from_status,to_status,actor_user_id,actor_impersonator_id,reason) \
                 VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING {EVENT_COLUMNS}"
            ),
            &[
                &tenant, &company_id, &request.id, &from.map(RequestStatus::as_str),
                &request.status.as_str(), &actor.0, &actor.1, &reason,
            ],
        )
        .await?;
    let event = event_from_row(&row)?;
    Ok(Change {
        kind: "supply_chain.purchasing.request_event".into(),
        id: event.id,
        old: json!({}),
        new: event_json(request.id, &event),
    })
}

/// The saved request as `get` reads it inside the write's transaction, so
/// the caller sees its lines and history at once.
async fn saved(
    tx: &MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    request_id: i64,
) -> Result<Outcome<Request>> {
    get(tx.sql(), tenant, company_id, request_id)
        .await?
        .map(Outcome::Saved)
        .ok_or(Error::Internal)
}

pub(crate) async fn create(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    input: &NewRequest,
) -> Result<Outcome<Request>> {
    let mut checks = Checks::default();
    let title = checks.take(title(&input.title));
    let justification = checks.take(justification(&input.justification));
    let needed_on = checks.take(date("needed_on", &input.needed_on));
    let valid_lines = validate_lines(&mut checks, &input.lines);
    if let Err(errors) = checks.finish() {
        return Ok(Outcome::Refused(Refusal::Invalid(errors)));
    }
    let tenant = scope.tenant();
    let actor = actor(scope);
    let id = tx
        .mutate(move |sql| {
            Box::pin(async move {
                // The trigger numbers the request; the status defaults to draft.
                let row = sql
                    .query_one(
                        &format!(
                            "INSERT INTO supply_chain_purchasing_requests(tenant_id,company_id,title,justification,needed_on,created_by_user_id,created_by_impersonator_id,inserted_at,updated_at) \
                             VALUES($1,$2,$3,$4,$5::text::date,$6,$7,now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC') RETURNING {COLUMNS}"
                        ),
                        &[&tenant, &company_id, &title, &justification, &needed_on, &actor.0, &actor.1],
                    )
                    .await?;
                let mut request = from_row(&row)?;
                let mut changes = Vec::new();
                request.lines =
                    insert_lines(sql, tenant, company_id, request.id, &valid_lines, &mut changes)
                        .await?;
                changes
                    .push(insert_event(sql, tenant, company_id, &request, None, actor, None).await?);
                changes.insert(
                    0,
                    Change {
                        kind: "supply_chain.purchasing.request".into(),
                        id: request.id,
                        old: json!({}),
                        new: to_json(&request),
                    },
                );
                Ok((request.id, changes))
            })
        })
        .await?;
    saved(tx, tenant, company_id, id).await
}

pub(crate) async fn update(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    request_id: i64,
    input: &RequestChanges,
) -> Result<Outcome<Request>> {
    let current = match locked(tx, scope, company_id, request_id).await? {
        Ok(request) => request,
        Err(refusal) => return Ok(Outcome::Refused(refusal)),
    };
    if let Err(refusal) = require_status(&current, &[RequestStatus::Draft], "draft") {
        return Ok(Outcome::Refused(refusal));
    }
    let mut checks = Checks::default();
    let mut next = current.clone();
    if let Some(value) = &input.title {
        next.title = checks.take(title(value));
    }
    if let Some(value) = &input.justification {
        next.justification = checks.take(justification(value));
    }
    if let Some(value) = &input.needed_on {
        next.needed_on = checks.take(date("needed_on", value));
    }
    let valid_lines = input
        .lines
        .as_ref()
        .map(|lines| validate_lines(&mut checks, lines));
    if let Err(errors) = checks.finish() {
        return Ok(Outcome::Refused(Refusal::Invalid(errors)));
    }
    let tenant = scope.tenant();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let row = sql
                .query_one(
                    &format!(
                        "UPDATE supply_chain_purchasing_requests SET title=$3,justification=$4,needed_on=$5::text::date,updated_at=now() AT TIME ZONE 'UTC' \
                         WHERE company_id=$1 AND id=$2 RETURNING {COLUMNS}"
                    ),
                    &[&company_id, &current.id, &next.title, &next.justification, &next.needed_on],
                )
                .await?;
            let updated = from_row(&row)?;
            let mut changes = vec![Change {
                kind: "supply_chain.purchasing.request".into(),
                id: updated.id,
                old: to_json(&current),
                new: to_json(&updated),
            }];
            if let Some(valid_lines) = valid_lines {
                // A draft's lines are replaced whole, as Bilimbi's
                // `replace_lines/4` deletes and inserts them.
                sql.execute(
                    "DELETE FROM supply_chain_purchasing_request_lines WHERE request_id=$1",
                    &[&current.id],
                )
                .await?;
                for line in &current.lines {
                    changes.push(Change {
                        kind: "supply_chain.purchasing.request_line".into(),
                        id: line.id,
                        old: line_json(current.id, line),
                        new: json!({}),
                    });
                }
                insert_lines(sql, tenant, company_id, current.id, &valid_lines, &mut changes)
                    .await?;
            }
            Ok(((), changes))
        })
    })
    .await?;
    saved(tx, tenant, company_id, request_id).await
}

/// One status transition and its history row, in one mutation. `stamps`
/// names the lifecycle columns the target status sets: the submitter, or
/// the decider with the decision reason.
async fn transition(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    current: Request,
    to: RequestStatus,
    reason: Option<String>,
) -> Result<Outcome<Request>> {
    let tenant = scope.tenant();
    let actor = actor(scope);
    let stamps = match to {
        RequestStatus::Submitted => {
            ",submitted_by_user_id=$4,submitted_at=clock_timestamp() AT TIME ZONE 'UTC'"
        }
        RequestStatus::Approved | RequestStatus::Rejected => {
            ",decided_by_user_id=$4,decided_at=clock_timestamp() AT TIME ZONE 'UTC',decision_reason=$5"
        }
        RequestStatus::Draft | RequestStatus::Cancelled => "",
    };
    let request_id = current.id;
    tx.mutate(move |sql| {
        Box::pin(async move {
            let status = to.as_str();
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![&company_id, &current.id, &status];
            if stamps.contains("$4") {
                params.push(&actor.0);
            }
            if stamps.contains("$5") {
                params.push(&reason);
            }
            let row = sql
                .query_one(
                    &format!(
                        "UPDATE supply_chain_purchasing_requests SET status=$3{stamps},updated_at=now() AT TIME ZONE 'UTC' \
                         WHERE company_id=$1 AND id=$2 RETURNING {COLUMNS}"
                    ),
                    &params,
                )
                .await?;
            let updated = from_row(&row)?;
            let mut changes = vec![Change {
                kind: "supply_chain.purchasing.request".into(),
                id: updated.id,
                old: to_json(&current),
                new: to_json(&updated),
            }];
            changes.push(
                insert_event(
                    sql,
                    tenant,
                    company_id,
                    &updated,
                    Some(current.status),
                    actor,
                    reason.as_deref(),
                )
                .await?,
            );
            Ok(((), changes))
        })
    })
    .await?;
    saved(tx, tenant, company_id, request_id).await
}

pub(crate) async fn submit(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    request_id: i64,
) -> Result<Outcome<Request>> {
    let request = match locked(tx, scope, company_id, request_id).await? {
        Ok(request) => request,
        Err(refusal) => return Ok(Outcome::Refused(refusal)),
    };
    if let Err(refusal) = require_status(&request, &[RequestStatus::Draft], "submitted") {
        return Ok(Outcome::Refused(refusal));
    }
    if request.lines.is_empty() {
        return Ok(Outcome::Refused(Refusal::NoLines));
    }
    transition(
        tx,
        scope,
        company_id,
        request,
        RequestStatus::Submitted,
        None,
    )
    .await
}

/// Approves or rejects a submitted request: someone other than its
/// submitter decides, and a rejection needs a reason.
pub(crate) async fn decide(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    request_id: i64,
    to: RequestStatus,
    reason_text: &str,
) -> Result<Outcome<Request>> {
    let request = match locked(tx, scope, company_id, request_id).await? {
        Ok(request) => request,
        Err(refusal) => return Ok(Outcome::Refused(refusal)),
    };
    if let Err(refusal) = require_status(&request, &[RequestStatus::Submitted], to.as_str()) {
        return Ok(Outcome::Refused(refusal));
    }
    if request.submitted_by_user_id == Some(scope.user()) {
        return Ok(Outcome::Refused(Refusal::OwnRequest));
    }
    let reason = match reason(reason_text, to == RequestStatus::Rejected) {
        Ok(reason) => reason,
        Err(error) => return Ok(Outcome::Refused(Refusal::Invalid(vec![error]))),
    };
    transition(tx, scope, company_id, request, to, reason).await
}

pub(crate) async fn cancel(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    request_id: i64,
    reason_text: &str,
) -> Result<Outcome<Request>> {
    let request = match locked(tx, scope, company_id, request_id).await? {
        Ok(request) => request,
        Err(refusal) => return Ok(Outcome::Refused(refusal)),
    };
    let open = [
        RequestStatus::Draft,
        RequestStatus::Submitted,
        RequestStatus::Approved,
    ];
    if let Err(refusal) = require_status(&request, &open, "cancelled") {
        return Ok(Outcome::Refused(refusal));
    }
    let reason = match reason(reason_text, true) {
        Ok(reason) => reason,
        Err(error) => return Ok(Outcome::Refused(Refusal::Invalid(vec![error]))),
    };
    transition(
        tx,
        scope,
        company_id,
        request,
        RequestStatus::Cancelled,
        reason,
    )
    .await
}
