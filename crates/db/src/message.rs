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
        ORDER BY created_at DESC
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
            save_message(&pool, conv.id, "user", &format!("msg {i}"), Some(i))
                .await
                .unwrap();
        }

        let recent = get_recent_messages(&pool, conv.id, 3).await.unwrap();
        let contents: Vec<_> = recent.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["msg 3", "msg 4", "msg 5"]);

        assert!(
            save_message(&pool, conv.id, "hacker", "x", None)
                .await
                .is_err()
        );
    }
}
