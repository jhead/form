//! Port of `.upstream/packages/server/test/{unix,unix-connection}.test.ts`.
//!
//! These cover the socket-file discipline: never unlink something this listener
//! does not own, refuse to steal a live socket, and clear a genuinely stale one.

#![cfg(unix)]

mod common;

use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::sync::Arc;
use std::time::Duration;

use common::socket_dir;
use pi_protocol::{
    encode_server_message, FrameDecoderOptions, ProtocolError, ProtocolErrorCode, ServerHelloError,
    ServerMessage, ServerMessageDecoder,
};
use pi_server::connection::ByteConnection;
use pi_server::testing::{connect_unix_test_client, TestServerService};
use pi_server::unix::{create_unix_server, UnixByteConnection, UnixServerOptions};
use pi_server::SessionService;

fn service() -> Arc<dyn SessionService> {
    TestServerService::new() as Arc<dyn SessionService>
}

#[tokio::test]
async fn rejects_a_live_listener_without_unlinking_it() {
    let directory = socket_dir();
    let path = directory.path().join("s.sock");
    let first = create_unix_server(service(), UnixServerOptions::new(&path)).expect("server");
    first.start().await.expect("start");
    let identity = std::fs::symlink_metadata(&path).expect("stat");

    let second = create_unix_server(service(), UnixServerOptions::new(&path)).expect("server");
    let error = second.start().await.expect_err("the path is taken");
    assert!(
        error.to_string().contains("already running"),
        "unexpected error: {error}"
    );

    let current = std::fs::symlink_metadata(&path).expect("stat");
    assert!(current.file_type().is_socket());
    assert_eq!(
        (current.dev(), current.ino()),
        (identity.dev(), identity.ino())
    );

    let client = connect_unix_test_client(&path).await.expect("connect");
    assert!(matches!(client.hello().await, ServerMessage::Hello(_)));
    client.close().await;

    second.close().await;
    first.close().await;
}

#[tokio::test]
async fn never_unlinks_a_regular_file_at_the_configured_path() {
    let directory = socket_dir();
    let path = directory.path().join("s.sock");
    std::fs::write(&path, "do not remove").expect("write");

    let server = create_unix_server(service(), UnixServerOptions::new(&path)).expect("server");
    let error = server.start().await.expect_err("not a socket");
    assert!(
        error.to_string().contains("non-socket"),
        "unexpected error: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "do not remove"
    );
    server.close().await;
}

#[tokio::test]
async fn creates_nested_parents_restricts_permissions_and_removes_its_own_socket() {
    let directory = socket_dir();
    let path = directory.path().join("p").join("n").join("s.sock");
    let server = create_unix_server(service(), UnixServerOptions::new(&path)).expect("server");
    server.start().await.expect("start");

    let stats = std::fs::symlink_metadata(&path).expect("stat");
    assert!(stats.file_type().is_socket());
    assert_eq!(stats.permissions().mode() & 0o777, 0o600);

    server.close().await;
    assert!(!path.exists());
}

#[tokio::test]
async fn does_not_remove_a_replacement_inode_during_shutdown() {
    let directory = socket_dir();
    let path = directory.path().join("s.sock");
    let server = create_unix_server(service(), UnixServerOptions::new(&path)).expect("server");
    server.start().await.expect("start");

    std::fs::remove_file(&path).expect("unlink");
    std::fs::write(&path, "replacement").expect("write");

    server.close().await;
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "replacement");
}

#[tokio::test]
async fn removes_a_genuinely_stale_socket_before_binding() {
    let directory = socket_dir();
    let path = directory.path().join("s.sock");
    // Binding then dropping the listener leaves the socket file behind with
    // nothing listening on it, which is exactly a stale socket.
    {
        let stale = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        drop(stale);
    }
    assert!(std::fs::symlink_metadata(&path)
        .expect("stat")
        .file_type()
        .is_socket());

    let server = create_unix_server(service(), UnixServerOptions::new(&path)).expect("server");
    server.start().await.expect("the stale socket is replaced");
    assert!(std::fs::symlink_metadata(&path)
        .expect("stat")
        .file_type()
        .is_socket());

    let client = connect_unix_test_client(&path).await.expect("connect");
    assert!(matches!(client.hello().await, ServerMessage::Hello(_)));
    client.close().await;
    server.close().await;
}

#[tokio::test]
async fn queues_a_final_protocol_error_behind_pending_output_before_closing() {
    let directory = socket_dir();
    let path = directory.path().join("s.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");
    let accepting = tokio::spawn(async move { listener.accept().await });
    let client = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    let (server_socket, _) = accepting.await.expect("join").expect("accept");

    let connection =
        UnixByteConnection::new(server_socket, Duration::from_millis(1_000), 64 * 1024);
    let pending = connection.send(vec![1, 2, 3]);

    let final_message = ServerMessage::HelloError(ServerHelloError {
        error: ProtocolError::new(ProtocolErrorCode::InvalidRequest, "Protocol violation"),
    });
    let final_frame =
        encode_server_message(&final_message, FrameDecoderOptions::default()).expect("encode");
    let closing = connection.close(Some(final_frame));

    pending.await.expect("the queued write lands first");
    closing.await.expect("close");
    assert!(connection.closed());

    // The peer sees the pending bytes and then the final frame, in that order.
    let mut received = Vec::new();
    let mut reader = client;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let mut buffer = [0u8; 4096];
        let read = tokio::time::timeout_at(
            deadline,
            tokio::io::AsyncReadExt::read(&mut reader, &mut buffer),
        )
        .await
        .expect("read within the deadline")
        .expect("read");
        if read == 0 {
            break;
        }
        received.extend_from_slice(&buffer[..read]);
    }
    assert_eq!(&received[..3], &[1, 2, 3]);
    let messages = ServerMessageDecoder::default()
        .push(&received[3..])
        .expect("the final frame decodes");
    assert_eq!(messages, vec![final_message]);
}
