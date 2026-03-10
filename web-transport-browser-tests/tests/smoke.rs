use std::time::Duration;

use web_transport_browser_tests::harness;

#[tokio::test]
async fn browser_connects_to_server() {
    let harness = harness::setup(harness::echo_handler()).await.unwrap();

    let result = harness
        .run_js(
            r#"
        const wt = await connectWebTransport();
        wt.close();
        return { success: true, message: "connected and closed" };
    "#,
            Duration::from_secs(10),
        )
        .await;

    // Teardown before asserting so resources are cleaned up even on failure.
    harness.teardown().await;

    let result = result.unwrap();
    assert!(result.success, "{}", result.message);
}
