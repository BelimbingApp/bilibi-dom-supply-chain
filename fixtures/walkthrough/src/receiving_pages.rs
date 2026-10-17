//! S6: leaves one sent order for the Receive goods page to take a receipt
//! against, created through the Purchasing facade like every other record.
use super::{order_line, saved, show, COMPANY};
use bilibi_database::{Error, Result};
use bilibi_module_registry::Services;
use bilibi_supply_chain_purchasing::{NewOrder, Purchasing, SERVICE};
use bilibi_tenancy::Scope;

pub(super) async fn run(services: &Services, manager: &Scope, admin: &Scope) -> Result<()> {
    let p = services.get::<Purchasing>(SERVICE)?;
    let supplier = p
        .list_suppliers(manager, COMPANY, true)
        .await?
        .first()
        .map(|s| s.id)
        .ok_or(Error::Internal)?;
    let order = saved(show(
        "S6 draft order to receive against",
        p.create_order(
            manager,
            COMPANY,
            &NewOrder {
                supplier_id: Some(supplier),
                currency_code: "MYR".into(),
                reference: "S6 delivery".into(),
                lines: vec![
                    order_line("Receiving item A", "10", "unit", "4.50", None),
                    order_line("Receiving item B", "5", "box", "12", None),
                ],
                ..NewOrder::default()
            },
        )
        .await,
    ))?;
    saved(show(
        "S6 submit order",
        p.submit_order(manager, COMPANY, order.id).await,
    ))?;
    saved(show(
        "S6 approve order",
        p.approve_order(admin, COMPANY, order.id, "").await,
    ))?;
    let order = saved(show(
        "S6 send order",
        p.send_order(manager, COMPANY, order.id).await,
    ))?;
    println!(
        "S6: PO {} is sent; sign in as manager@example.test and open /supply-chain/receive?order={} to receive it, then post, cancel or reverse receipts from /supply-chain/receipts.",
        order.order_number, order.id
    );
    Ok(())
}
