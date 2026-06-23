//! Печатает UPDATE _sqlx_migrations с актуальными чексуммами файлов миграций.
//! Используется один раз в PR2 после правки комментариев старых миграций,
//! чтобы живая БД приняла изменённые (только комментарии) файлы.
use sqlx::migrate::Migrator;
use std::path::Path;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "migrations".into());
    let m = Migrator::new(Path::new(&dir)).await?;
    for mig in m.iter() {
        let hex: String = mig.checksum.iter().map(|b| format!("{b:02x}")).collect();
        println!(
            "UPDATE _sqlx_migrations SET checksum = decode('{hex}','hex') WHERE version = {};",
            mig.version
        );
    }
    Ok(())
}
