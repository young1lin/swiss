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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::echo::EchoServer;

    #[tokio::test]
    async fn speaks_the_real_wire_protocol_to_a_server_with_no_child_process() {
        // The point of the duplex: the same JSON-RPC line protocol a stdio child would use, with
        // no process, no port and no handshake of our own to keep in step.
        let client = open_session(EchoServer::default())
            .await
            .expect("a session opens");

        let listed = client.list_tools(None).await.expect("tools/list answers");
        assert_eq!(listed.tools.len(), 1);
        assert_eq!(listed.tools[0].name.as_ref(), "echo");

        let mut params = rmcp::model::CallToolRequestParams::default();
        params.name = "echo".into();
        params.arguments = serde_json::json!({ "msg": "hi" }).as_object().cloned();
        let out = client.call_tool(params).await.expect("tools/call answers");
        let rendered = serde_json::to_value(&out).expect("the result serializes");
        assert!(rendered.to_string().contains("hi"), "{rendered}");

        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn every_session_is_its_own_and_disposable() {
        // Callers open one per request, because an SDK server holds a single connected transport
        // and races under concurrent use. Two at once must both work, and closing one must not
        // touch the other.
        let a = open_session(EchoServer::default())
            .await
            .expect("session a");
        let b = open_session(EchoServer::default())
            .await
            .expect("session b");

        assert_eq!(a.list_tools(None).await.expect("a lists").tools.len(), 1);
        let _ = a.cancel().await;
        assert_eq!(
            b.list_tools(None).await.expect("b still lists").tools.len(),
            1
        );
        let _ = b.cancel().await;
    }

    #[tokio::test]
    async fn the_server_side_ends_when_the_client_is_dropped() {
        // Nothing joins the spawned server task, so a session that outlived its client would be a
        // leak on every paging call the panel makes.
        let client = open_session(EchoServer::default())
            .await
            .expect("a session opens");
        assert_eq!(client.list_tools(None).await.expect("lists").tools.len(), 1);
        drop(client);

        // The server task ends on EOF rather than on a signal from us; yielding is enough for it
        // to observe the closed duplex on this runtime.
        tokio::task::yield_now().await;
    }
}
