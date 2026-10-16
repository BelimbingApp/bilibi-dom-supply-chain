//! `DATABASE_URL=… cargo run -p bilibi-supply-chain-walkthrough`; see `lib.rs`.
#[tokio::main]
async fn main() -> bilibi_database::Result<()> {
    let url = std::env::var("DATABASE_URL").map_err(|_| bilibi_database::Error::Invalid)?;
    bilibi_supply_chain_walkthrough::run(&url).await
}
