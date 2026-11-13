use axum::response::{
    Sse,
    sse::{Event, KeepAlive},
};
use std::{convert::Infallible, time::Duration};
use tokio::sync::mpsc;
use tokio_stream::{Stream, wrappers::ReceiverStream};

pub async fn demo() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(4);

    tokio::spawn(async move {
        let words = ["Rust", "streams", "tokens", "with", "SSE"];

        for word in words.into_iter().cycle().take(25) {
            tokio::time::sleep(Duration::from_millis(200)).await;

            let event = Event::default().data(word);

            if tx.send(Ok(event)).await.is_err() {
                // 客户端断开，接收端已销毁，停止生成。
                break;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}
