use sqlx::SqlitePool;
use tokio::sync::mpsc;

use crate::storage::records::{self, FinishAttempt, NewAttemptRecord, NewRequestRecord};

const RECORD_QUEUE_CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct RecordWriter {
    sender: mpsc::Sender<RecordEvent>,
}

#[derive(Clone, Debug)]
pub struct CompleteRequest {
    pub request_id: String,
    pub status: String,
    pub upstream_name: Option<String>,
    pub attempt_count: i64,
    pub final_http_status: Option<i64>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FinishAttemptRecord {
    pub attempt_id: String,
    pub update: FinishAttempt,
}

#[derive(Clone, Debug)]
pub struct RecordBodyChunk {
    pub request_id: String,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct FinishBody {
    pub request_id: String,
    pub complete: bool,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug)]
enum RecordEvent {
    CreateRequest(NewRequestRecord),
    CompleteRequest(CompleteRequest),
    CreateAttempt(NewAttemptRecord),
    FinishAttempt(FinishAttemptRecord),
    SaveRequestBody(RecordBodyChunk),
    AppendRequestBody(RecordBodyChunk),
    FinishRequestBody(FinishBody),
    SaveResponseBody(RecordBodyChunk),
    AppendResponseBody(RecordBodyChunk),
    FinishResponseBody(FinishBody),
}

impl RecordWriter {
    pub fn spawn(pool: SqlitePool) -> Self {
        let (sender, receiver) = mpsc::channel(RECORD_QUEUE_CAPACITY);
        tokio::spawn(run_writer(pool, receiver));
        Self { sender }
    }

    pub fn create_request(&self, record: NewRequestRecord) -> bool {
        self.try_send("create_request", RecordEvent::CreateRequest(record))
    }

    pub fn complete_request(&self, record: CompleteRequest) -> bool {
        self.try_send("complete_request", RecordEvent::CompleteRequest(record))
    }

    pub fn create_attempt(&self, record: NewAttemptRecord) -> bool {
        self.try_send("create_attempt", RecordEvent::CreateAttempt(record))
    }

    pub fn finish_attempt(&self, record: FinishAttemptRecord) -> bool {
        self.try_send("finish_attempt", RecordEvent::FinishAttempt(record))
    }

    pub fn save_request_body(&self, record: RecordBodyChunk) -> bool {
        self.try_send("save_request_body", RecordEvent::SaveRequestBody(record))
    }

    pub fn append_request_body(&self, record: RecordBodyChunk) -> bool {
        self.try_send(
            "append_request_body",
            RecordEvent::AppendRequestBody(record),
        )
    }

    pub fn finish_request_body(&self, record: FinishBody) -> bool {
        self.try_send(
            "finish_request_body",
            RecordEvent::FinishRequestBody(record),
        )
    }

    pub fn save_response_body(&self, record: RecordBodyChunk) -> bool {
        self.try_send("save_response_body", RecordEvent::SaveResponseBody(record))
    }

    pub fn append_response_body(&self, record: RecordBodyChunk) -> bool {
        self.try_send(
            "append_response_body",
            RecordEvent::AppendResponseBody(record),
        )
    }

    pub fn finish_response_body(&self, record: FinishBody) -> bool {
        self.try_send(
            "finish_response_body",
            RecordEvent::FinishResponseBody(record),
        )
    }

    fn try_send(&self, event_name: &'static str, event: RecordEvent) -> bool {
        match self.sender.try_send(event) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    event = event_name,
                    "request record queue is full; dropping record event"
                );
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!(
                    event = event_name,
                    "request record writer is closed; dropping record event"
                );
                false
            }
        }
    }
}

async fn run_writer(pool: SqlitePool, mut receiver: mpsc::Receiver<RecordEvent>) {
    while let Some(event) = receiver.recv().await {
        if let Err(error) = write_event(&pool, event).await {
            tracing::warn!(error = %error, "failed to write request record event");
        }
    }
}

async fn write_event(pool: &SqlitePool, event: RecordEvent) -> Result<(), sqlx::Error> {
    match event {
        RecordEvent::CreateRequest(record) => records::create_request(pool, &record).await,
        RecordEvent::CompleteRequest(record) => {
            records::complete_request(
                pool,
                &record.request_id,
                &record.status,
                record.upstream_name.as_deref(),
                record.attempt_count,
                record.final_http_status,
                record.error_message.as_deref(),
            )
            .await
        }
        RecordEvent::CreateAttempt(record) => records::create_attempt(pool, &record).await,
        RecordEvent::FinishAttempt(record) => {
            records::finish_attempt(pool, &record.attempt_id, &record.update).await
        }
        RecordEvent::SaveRequestBody(record) => {
            records::save_request_body(pool, &record.request_id, &record.body).await
        }
        RecordEvent::AppendRequestBody(record) => {
            records::append_request_body(pool, &record.request_id, &record.body).await
        }
        RecordEvent::FinishRequestBody(record) => {
            records::finish_request_body(
                pool,
                &record.request_id,
                record.complete,
                record.error_message.as_deref(),
            )
            .await
        }
        RecordEvent::SaveResponseBody(record) => {
            records::save_response_body(pool, &record.request_id, &record.body).await
        }
        RecordEvent::AppendResponseBody(record) => {
            records::append_response_body(pool, &record.request_id, &record.body).await
        }
        RecordEvent::FinishResponseBody(record) => {
            records::finish_response_body(
                pool,
                &record.request_id,
                record.complete,
                record.error_message.as_deref(),
            )
            .await
        }
    }
}
