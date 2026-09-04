//! Shared test helpers.

#![allow(dead_code)]

#[cfg(feature = "websocket")]
pub mod phoenix;

use std::{
    future::Future,
    io,
    sync::{Arc, Mutex, Once},
};

use tracing_subscriber::{EnvFilter, fmt::MakeWriter};

tokio::task_local! {
    /// Where the task currently under [`capture_logs`] sends its log lines.
    static SINK: Arc<Mutex<Vec<u8>>>;
}

static SUBSCRIBER: Once = Once::new();

/// Collects everything the crate logs while `work` runs.
///
/// One subscriber is installed globally for the whole test binary and every
/// line it writes is routed by the task-local `SINK`. Both halves matter: a
/// per-test subscriber leaves `tracing`'s per-callsite interest cache set to
/// "never" whenever a concurrent test reaches that callsite first, which
/// silently drops events, and a task-local follows `work` when a multi-threaded
/// runtime resumes it on another thread. A test that does not capture has no
/// sink, so its lines are discarded rather than landing in someone else's
/// buffer.
pub async fn capture_logs<F: Future>(work: F) -> String {
    SUBSCRIBER.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("rs_solar_assistant=debug"))
            .with_writer(TaskWriter)
            .without_time()
            .with_ansi(false)
            .init();
    });

    let buffer = Arc::new(Mutex::new(Vec::new()));
    SINK.scope(Arc::clone(&buffer), work).await;

    let captured = buffer.lock().unwrap();
    String::from_utf8_lossy(&captured).into_owned()
}

/// Hands every log line to the sink of the task that produced it.
struct TaskWriter;

impl<'a> MakeWriter<'a> for TaskWriter {
    type Writer = TaskSink;

    fn make_writer(&'a self) -> Self::Writer {
        TaskSink(SINK.try_with(Arc::clone).ok())
    }
}

/// The sink of one task, or nothing when the task is not capturing.
struct TaskSink(Option<Arc<Mutex<Vec<u8>>>>);

impl io::Write for TaskSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(sink) = &self.0 {
            sink.lock().unwrap().extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
