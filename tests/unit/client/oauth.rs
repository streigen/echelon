use super::*;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

#[tokio::test]
async fn redirect_timeout_shuts_down_local_server() {
    let (redirect_uri, redirect_handle) = LocalServerBuilder::new()
        .spawn()
        .await
        .expect("local redirect server should start");
    let address = redirect_uri
        .socket_addrs(|| None)
        .expect("redirect URI should resolve to a socket address")[0];

    let probe = TcpStream::connect(address)
        .await
        .expect("redirect listener should accept connections before timeout");
    drop(probe);

    let error = wait_for_oauth_redirect(redirect_handle, Duration::from_millis(10), None)
        .await
        .expect_err("waiting without a redirect should time out");

    assert_eq!(
        error.to_string(),
        "OAuth login timed out. Please try again."
    );

    // Dropping the timed out redirect future signals the SDK server to stop.
    let mut listener_stopped = false;
    for _ in 0..50 {
        if TcpStream::connect(address).await.is_err() {
            listener_stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        listener_stopped,
        "timed out redirect listener should shut down"
    );
}

#[tokio::test]
async fn redirect_query_is_returned_when_listener_receives_redirect() {
    let (redirect_uri, redirect_handle) = LocalServerBuilder::new()
        .spawn()
        .await
        .expect("local redirect server should start");
    let address = redirect_uri
        .socket_addrs(|| None)
        .expect("redirect URI should resolve to a socket address")[0];

    let mut stream = TcpStream::connect(address)
        .await
        .expect("redirect listener should accept connections");
    stream
        .write_all(b"GET /?code=test-code HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("redirect request should be sent");

    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let query = wait_for_oauth_redirect(redirect_handle, Duration::from_secs(1), Some(cancel_rx))
        .await
        .expect("redirect should arrive before timeout");
    assert_eq!(query.as_deref(), Some("code=test-code"));
}

#[tokio::test]
async fn cancelling_redirect_wait_shuts_down_local_server() {
    let (redirect_uri, redirect_handle) = LocalServerBuilder::new()
        .spawn()
        .await
        .expect("local redirect server should start");
    let address = redirect_uri
        .socket_addrs(|| None)
        .expect("redirect URI should resolve to a socket address")[0];
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let wait = tokio::spawn(wait_for_oauth_redirect(
        redirect_handle,
        Duration::from_secs(5),
        Some(cancel_rx),
    ));
    let probe = tokio::time::timeout(Duration::from_secs(1), TcpStream::connect(address))
        .await
        .expect("redirect listener should start accepting requests")
        .expect("redirect listener should accept a connection");
    drop(probe);
    cancel_tx
        .send(())
        .expect("cancel receiver should be waiting");

    let result = wait
        .await
        .expect("redirect wait task should finish")
        .expect("cancellation should end redirect waiting normally");
    assert_eq!(result, None);

    let mut listener_stopped = false;
    for _ in 0..50 {
        if TcpStream::connect(address).await.is_err() {
            listener_stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        listener_stopped,
        "cancelled redirect listener should shut down"
    );
}
