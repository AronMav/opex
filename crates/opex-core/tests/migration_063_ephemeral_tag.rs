//! DB-тест: после применения всех миграций (включая 063) ephemeral-таблицы
//! должны иметь тег `@opex:ephemeral`, а тег `@hydeclaw:ephemeral` — отсутствовать.

use sqlx::PgPool;

/// Проверяет, что после миграций:
/// 1. Есть хотя бы одна таблица с тегом `@opex:ephemeral` (migration 063 отработала).
/// 2. Не осталось ни одной таблицы с тегом `@hydeclaw:ephemeral` (старый тег стёрт).
#[sqlx::test(migrations = "../../migrations")]
async fn ephemeral_tag_is_opex(pool: PgPool) {
    let opex_count: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM pg_description d \
         JOIN pg_class c ON d.objoid = c.oid AND d.objsubid = 0 \
         WHERE d.description LIKE '@opex:ephemeral%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        opex_count.0 > 0,
        "должны быть таблицы с тегом @opex:ephemeral, найдено: 0"
    );

    let legacy_count: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM pg_description WHERE description LIKE '@hydeclaw:ephemeral%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        legacy_count.0, 0,
        "не должно остаться тега @hydeclaw:ephemeral, найдено: {}",
        legacy_count.0
    );
}
