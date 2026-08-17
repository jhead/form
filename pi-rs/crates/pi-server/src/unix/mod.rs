//! Port of `.upstream/packages/server/src/transports/unix/`.

pub mod listener;
pub mod preset;
pub mod types;

pub use listener::{
    create_unix_listener, validate_unix_socket_path, UnixByteConnection, UnixListener,
    MAX_UNIX_SOCKET_PATH_BYTES,
};
pub use preset::create_unix_server;
pub use types::{UnixListenerOptions, UnixServerOptions};
