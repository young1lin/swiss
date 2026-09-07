//! A throwaway in-memory MCP session against a freshly built server — port of `introspect.ts`.
//!
//! The caller drops the returned client when done; EOF then ends the server task. Used by the
//! paging endpoints and the panel's tool runner. The wire is `tokio::io::duplex`, and each side
//! speaks JSON-RPC over it through rmcp's async-rw transport — the same line protocol a stdio
//! child uses, minus the child.

use rmcp::service::{serve_client, serve_server, RunningService};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{ClientHandler, RoleClient, RoleServer, ServerHandler};

/// A no-op client handler: the callers drive the session, the client side needs no behaviour.
pub struct GatewayIntrospectClient;

impl ClientHandler for GatewayIntrospectClient {}

/// Open an in-memory session against `server` and return the client. The server task is spawned
/// and ends when the client side closes.
pub async fn open_session<S>(
    server: S,
) -> Result<RunningService<RoleClient, GatewayIntrospectClient>, String>
where
    S: ServerHandler + Send + 'static,
{
    let (server_side, client_side) = tokio::io::duplex(64 * 1024);
    let (sr, sw) = tokio::io::split(server_side);
    let (cr, cw) = tokio::io::split(client_side);

    let server_transport = AsyncRwTransport::<RoleServer, _, _>::new(sr, sw);
    tokio::spawn(async move {
        // Errors here mean the client went away — the session is disposable by contract. (The
        // call context does NOT ride into this task: task-locals do not cross a spawn, so the
        // CALLER captures it when building the server — see calls::CallSource.)
        if let Ok(service) = serve_server(server, server_transport).await {
            let _ = service.waiting().await;
        }
    });

    let client_transport = AsyncRwTransport::<RoleClient, _, _>::new(cr, cw);
    serve_client(GatewayIntrospectClient, client_transport)
        .await
        .map_err(|e| e.to_string())
}
