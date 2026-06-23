//! Standalone migration runner for CI/testing environments.
//!
//! Connects to DATABASE_URL and runs all pending migrations.
//! Use this before `cargo test` when the test database is fresh (e.g., CI).
//!
//! Run via: cargo run --bin run_migrations -- MIGRATIONS_DIR

use std::path::{Path, PathBuf};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");

    let migrations_dir: PathBuf = std::env::var("MIGRATIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Default to workspace/migrations relative to this binary's location
            let manifest_dir = std::env!("CARGO_MANIFEST_DIR");
            Path::new(&manifest_dir).join("../../migrations")
        });

    println!("Connecting to database...");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await?;

    println!("Running migrations from {} ...", migrations_dir.display());
    let result = sqlx::migrate::Migrator::new(migrations_dir)
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