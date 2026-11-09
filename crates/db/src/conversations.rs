use crate::models::Conversation;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create_conversation(
    pool: &PgPool,
    title: Option<&str>,
) -> Result<Conversation, sqlx::Error> {
    sqlx::query_as!(
        Conversation,
        r#"
        INSERT INTO conversations (title)
        VALUES ($1)
        RETURNING id, title, created_at
        "#,
        title
    )
    .fetch_one(pool)
    .await
}
pub async fn list_conversations(pool: &PgPool) -> Result<Vec<Conversation>, sqlx::Error> {
    sqlx::query_as!(
        Conversation,
        r#"
        SELECT id, title, created_at
        FROM conversations
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(pool)
    .await
}
pub async fn delete_conversation(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM conversations WHERE id = $1", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn create_list_delete(pool: PgPool) {
        let created = create_conversation(&pool, Some("hello")).await.unwrap();
        assert_eq!(created.title.as_deref(), Some("hello"));

        let untitled = create_conversation(&pool, None).await.unwrap();
        assert!(untitled.title.is_none());

        let all = list_conversations(&pool).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, untitled.id);

        assert!(delete_conversation(&pool, created.id).await.unwrap());
        assert!(!delete_conversation(&pool, created.id).await.unwrap());
        assert_eq!(list_conversations(&pool).await.unwrap().len(), 1);
    }
}
