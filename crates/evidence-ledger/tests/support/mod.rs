use evidence_ledger::EvidenceLedger;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

const RESET_QUERY: &str =
    "TRUNCATE evidence_inbox, evidence_outbox, accepted_receipt_mirror RESTART IDENTITY CASCADE";

pub fn require_database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        panic!("DATABASE_URL is required for evidence-ledger integration tests")
    })
}

pub async fn require_pool() -> PgPool {
    let url = require_database_url();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap_or_else(|error| panic!("connect to DATABASE_URL: {error}"));

    EvidenceLedger::new(pool.clone())
        .migrate()
        .await
        .unwrap_or_else(|error| panic!("run evidence-ledger migrations: {error}"));
    assert_database_round_trip(&pool).await;
    pool
}

pub async fn require_clean_pool() -> PgPool {
    let pool = require_pool().await;
    reset_database(&pool).await;
    pool
}

pub async fn reset_database(pool: &PgPool) {
    sqlx::query(RESET_QUERY)
        .execute(pool)
        .await
        .unwrap_or_else(|error| panic!("reset evidence-ledger tables: {error}"));
}

async fn assert_database_round_trip(pool: &PgPool) {
    let value: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(pool)
        .await
        .unwrap_or_else(|error| panic!("PostgreSQL readiness round trip: {error}"));
    assert_eq!(value, 1, "PostgreSQL readiness round trip returned {value}");
    println!("AGI_BACKEND_ROUND_TRIP:1");
}
