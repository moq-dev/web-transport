use pyo3::prelude::*;

mod client;
mod errors;
mod recv_stream;
mod runtime;
mod send_stream;
mod server;
mod session;

#[pymodule]
fn _web_transport(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize the tokio runtime eagerly
    runtime::get_runtime();

    // Register exception types
    errors::register(m)?;

    // Register classes
    m.add_class::<server::Server>()?;
    m.add_class::<server::SessionRequest>()?;
    m.add_class::<client::Client>()?;
    m.add_class::<session::Session>()?;
    m.add_class::<send_stream::SendStream>()?;
    m.add_class::<recv_stream::RecvStream>()?;

    Ok(())
}
