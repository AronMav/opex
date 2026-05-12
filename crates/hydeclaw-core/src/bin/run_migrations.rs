//! Standalone migration runner for CI/testing environments.
//!
//! Connects to DATABASE_URL and runs all pending migrations.
//! Use this before `cargo test` when the test database is fresh (e.g., CI).
//!
//! Run via: cargo run --bin run_migrations

use std::path::Path;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");

    println!("Connecting to database...");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await?;

    println!("Running migrations from ./migrations ...");
    let result = sqlx::migrate::Migrator::new(Path::new("migrations"))
        .await?
        .run(&pool)
        .await;

    match result {
        Ok(_count) => println!("Migrations applied successfully."),
        Err(e) => {
            eprintln!("Migration failed: {e}");
            std::process::exit(1);
        }
    }

    pool.close().await;
    Ok(())
}