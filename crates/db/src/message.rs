use crate::models::Message;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn save_message(
    pool: &PgPool,
    conversation_id: Uuid,
    role: &str,
    content: &str,
    token_count: Option<i32>,
) -> Result<Message, sqlx::Error> {
    sqlx::query_as!(
        Message,
        r#"
        INSERT INTO messages (conversation_id, role, content, token_count)
        VALUES ($1, $2, $3, $4)
        RETURNING id, conversation_id, role, content, token_count, created_at
        "#,
        conversation_id,
        role,
        content,
        token_count
    )
    .fetch_one(pool)
    .await
}

/// Returns the latest messages in ascending (created_at, id) order.
/// UUID breaks timestamp ties deterministically, not by insertion order.
pub async fn get_recent_messages(
    pool: &PgPool,
    conversation_id: Uuid,
    limit: i64,
) -> Result<Vec<Message>, sqlx::Error> {
    let mut message = sqlx::query_as!(
        Message,
        r#"
        SELECT id, conversation_id, role, content, token_count, created_at
        FROM messages
        WHERE conversation_id = $1
        ORDER BY created_at DESC, id DESC
        LIMIT $2
        "#,
        conversation_id,
        limit
    )
    .fetch_all(pool)
    .await?;
    message.reverse();
    Ok(message)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::conversations::create_conversation;

    #[sqlx::test(migrations = "../../migrations")]
    async fn save_get_recent(pool: PgPool) {
        let conv = create_conversation(&pool, Some("chat")).await.unwrap();

        for i in 1..=5 {
            let saved = save_message(&pool, conv.id, "user", &format!("msg {i}"), Some(i))
                .await
                .unwrap();
            sqlx::query(
                "UPDATE messages SET created_at = TIMESTAMPTZ '2026-01-01 00:00:00+00' \
                 + $2::int * INTERVAL '1 second' WHERE id = $1",
            )
            .bind(saved.id)
            .bind(i)
            .execute(&pool)
            .await
            .unwrap();
        }
        let other = create_conversation(&pool, Some("other")).await.unwrap();
        save_message(&pool, other.id, "user", "not this conversation", None)
            .await
            .unwrap();

        let recent = get_recent_messages(&pool, conv.id, 3).await.unwrap();
        let contents: Vec<_> = recent.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["msg 3", "msg 4", "msg 5"]);

        let error = save_message(&pool, conv.id, "hacker", "x", None)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23514")
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn tied_timestamps_have_stable_order(pool: PgPool) {
        let conv = create_conversation(&pool, Some("timestamp ties"))
            .await
            .unwrap();
        for i in [3, 1, 5, 2, 4] {
            sqlx::query(
                "INSERT INTO messages (id, conversation_id, role, content, created_at) \
                 VALUES ($1, $2, 'user', $3, TIMESTAMPTZ '2026-01-01 00:00:00+00')",
            )
            .bind(Uuid::from_u128(i))
            .bind(conv.id)
            .bind(format!("msg {i}"))
            .execute(&pool)
            .await
            .unwrap();
        }

        let recent = get_recent_messages(&pool, conv.id, 3).await.unwrap();
        let ids: Vec<_> = recent.iter().map(|m| m.id).collect();
        assert_eq!(ids, [3, 4, 5].map(Uuid::from_u128));
    }
}
