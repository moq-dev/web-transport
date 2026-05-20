uniffi::setup_scaffolding!("web_transport");

#[uniffi::export]
fn hello() -> String {
	"hello from web-transport-ffi".to_string()
}
