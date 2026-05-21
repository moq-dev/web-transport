uniffi::setup_scaffolding!("web_transport");

#[uniffi::export]
fn hello() -> String {
	"hello from web-transport-ffi".to_string()
}

#[uniffi::export(async_runtime = "tokio")]
async fn hello_async() -> String {
	tokio::time::sleep(std::time::Duration::from_millis(1)).await;
	"hello async from web-transport-ffi".to_string()
}
