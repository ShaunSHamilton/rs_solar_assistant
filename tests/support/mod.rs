//! Shared test helpers.

#![allow(dead_code)]

use std::{
    future::Future,
    io,
    sync::{Arc, Mutex},
};

use tracing_subscriber::{EnvFilter, fmt::MakeWriter};

/// Collects everything the crate logs while `work` runs.
///
/// The subscriber is installed for the current thread only, so tests that use
/// it stay independent of each other.
pub async fn capture_logs<F: Future>(work: F) -> String {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("rs_solar_assistant=debug"))
        .with_writer(BufferWriter(Arc::clone(&buffer)))
        .without_time()
        .finish();

    {
        let _guard = tracing::subscriber::set_default(subscriber);
        work.await;
    }

    let captured = buffer.lock().unwrap();
    String::from_utf8_lossy(&captured).into_owned()
}

#[derive(Clone)]
struct BufferWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for BufferWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
