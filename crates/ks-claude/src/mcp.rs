//! 10d: `ks mcp`: a stdio MCP server (official `rmcp` SDK) that proxies to the running TUI over
//! the local-socket IPC. The socket name comes from `KESTREL_IPC`. If the TUI is not reachable
//! (or the application answers with an error) the tool returns a normal MCP *tool error*
//! (`isError: true`, text starting with `Error:`) instead of failing the protocol.
//!
//! API notes (rmcp 3.5.1, VERIFIED by building): `#[tool_router]` on an inherent impl,
//! `#[tool_handler]` on `impl ServerHandler`, parameters via `Parameters<T: JsonSchema>`,
//! `handler.serve(rmcp::transport::stdio()).await?.waiting().await?`.

use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::ipc::{self, Request, Response, IPC_ENV};

const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// Arguments of `search_files`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchFilesArgs {
    /// Fuzzy query matched against file paths.
    pub query: String,
    /// Directory to search in (default: the project root of the explorer).
    pub root: Option<String>,
    /// Maximum number of results (default 50).
    pub limit: Option<usize>,
}

/// Arguments of `search_contents`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchContentsArgs {
    /// Text or regular expression to search for.
    pub pattern: String,
    /// Optional glob filter such as `*.rs`.
    pub glob: Option<String>,
    /// Interpret `pattern` as a regular expression (default false).
    pub regex: Option<bool>,
    /// Maximum number of hits (default 200).
    pub limit: Option<usize>,
}

/// Arguments of `list_dir`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListDirArgs {
    /// Directory to list.
    pub path: String,
}

/// Arguments of `reveal`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RevealArgs {
    /// File or directory to move the explorer cursor to.
    pub path: String,
}

/// The MCP server handler.
#[derive(Debug, Clone)]
pub struct KestrelMcp {
    ipc_name: Option<String>,
    tool_router: ToolRouter<Self>,
}

impl KestrelMcp {
    /// Create a handler that talks to the socket `ipc_name` (`None` = not running under Kestrel).
    pub fn new(ipc_name: Option<String>) -> Self {
        Self {
            ipc_name,
            tool_router: Self::tool_router(),
        }
    }

    /// Forward a request. `Err` carries the failure text (Kestrel unreachable, application
    /// error); the tools turn it into an MCP tool error (`isError: true`) so Claude can tell it
    /// from content.
    async fn proxy_result(&self, req: Request) -> Result<String, String> {
        let Some(name) = self.ipc_name.as_deref().filter(|n| !n.is_empty()) else {
            return Err(format!("Kestrel is not reachable ({IPC_ENV} is not set). Start Claude from inside the Kestrel TUI."));
        };
        match ipc::call(name, &req, CALL_TIMEOUT).await {
            Ok(Response::Error { message }) => Err(message),
            Ok(Response::Ok) => Ok("ok".to_string()),
            Ok(other) => serde_json::to_string_pretty(&other).map_err(|e| e.to_string()),
            Err(e) => Err(format!("{e}. Is the Kestrel TUI still running?")),
        }
    }

    async fn proxy(&self, req: Request) -> CallToolResult {
        match self.proxy_result(req).await {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(msg) => CallToolResult::error(vec![ContentBlock::text(format!("Error: {msg}"))]),
        }
    }
}

#[tool_router]
impl KestrelMcp {
    /// Marked files, the file under the cursor and the current directory of the explorer.
    #[tool(
        description = "Get the files currently marked in the Kestrel file explorer, the file under the cursor, and the current directory."
    )]
    async fn get_selection(&self) -> CallToolResult {
        self.proxy(Request::GetSelection).await
    }

    /// Fuzzy file search.
    #[tool(description = "Fuzzy-search file paths using Kestrel's cached index.")]
    async fn search_files(&self, Parameters(a): Parameters<SearchFilesArgs>) -> CallToolResult {
        self.proxy(Request::SearchFiles {
            query: a.query,
            root: a.root,
            limit: a.limit.unwrap_or(50).min(1000),
        })
        .await
    }

    /// Content search.
    #[tool(
        description = "Search file contents (literal text or regex) with an optional glob filter."
    )]
    async fn search_contents(
        &self,
        Parameters(a): Parameters<SearchContentsArgs>,
    ) -> CallToolResult {
        self.proxy(Request::SearchContents {
            pattern: a.pattern,
            glob: a.glob,
            regex: a.regex.unwrap_or(false),
            limit: a.limit.unwrap_or(200).min(5000),
        })
        .await
    }

    /// Directory listing.
    #[tool(description = "List the entries of a directory.")]
    async fn list_dir(&self, Parameters(a): Parameters<ListDirArgs>) -> CallToolResult {
        self.proxy(Request::ListDir { path: a.path }).await
    }

    /// Reveal in the explorer.
    #[tool(description = "Move the Kestrel explorer cursor to a path so the user sees it.")]
    async fn reveal(&self, Parameters(a): Parameters<RevealArgs>) -> CallToolResult {
        self.proxy(Request::Reveal { path: a.path }).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KestrelMcp {}

/// Entry point for `ks mcp`: serve MCP over stdin/stdout until the client disconnects.
/// Reads the TUI socket name from `KESTREL_IPC`. Writes nothing else to stdout.
pub async fn run_mcp_stdio() -> Result<(), String> {
    let handler = KestrelMcp::new(std::env::var(IPC_ENV).ok());
    let service = handler
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| e.to_string())?;
    service.waiting().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn rpc(
        w: &mut (impl AsyncWriteExt + Unpin),
        r: &mut (impl AsyncBufReadExt + Unpin),
        msg: Value,
    ) -> Value {
        w.write_all(format!("{msg}\n").as_bytes()).await.unwrap();
        let mut line = String::new();
        r.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn lists_tools_and_reports_unreachable() {
        let (client_io, server_io) = tokio::io::duplex(1 << 16);
        let (sr, sw) = tokio::io::split(server_io);
        let server = tokio::spawn(async move {
            let svc = KestrelMcp::new(Some("kestrel-test-mcp-missing".into()))
                .serve((sr, sw))
                .await
                .unwrap();
            let _ = svc.waiting().await;
        });
        let (cr, mut cw) = tokio::io::split(client_io);
        let mut cr = BufReader::new(cr);
        let init = rpc(
            &mut cw,
            &mut cr,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":"t","version":"0"}}}),
        )
        .await;
        assert!(init.get("result").is_some(), "{init}");
        cw.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        let tools = rpc(
            &mut cw,
            &mut cr,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        let names: Vec<String> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for n in [
            "get_selection",
            "search_files",
            "search_contents",
            "list_dir",
            "reveal",
        ] {
            assert!(names.iter().any(|x| x == n), "missing {n} in {names:?}");
        }
        let call = rpc(
            &mut cw,
            &mut cr,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_selection","arguments":{}}}),
        )
        .await;
        let text = call["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("Error:"), "{call}");
        assert_eq!(call["result"]["isError"], true, "{call}");
        server.abort();
    }

    #[tokio::test]
    async fn without_env_returns_helpful_error() {
        let m = KestrelMcp::new(None);
        let out = m.proxy_result(Request::GetSelection).await.unwrap_err();
        assert!(out.contains("not reachable"));
    }

    use crate::events::{ClaudeEvent, EventSink};
    use crate::ipc::{ContentHit, DirEntry, IpcServer, IpcServerOptions};
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    type Seen = Arc<Mutex<Vec<Request>>>;

    fn stub_server(tag: &str) -> (IpcServer, Seen) {
        let seen: Seen = Arc::default();
        let s2 = seen.clone();
        let sink: EventSink = Arc::new(move |ev| {
            if let ClaudeEvent::Ipc { request, reply, .. } = ev {
                s2.lock().unwrap().push(request.clone());
                reply.send(match request {
                    Request::GetSelection => Response::Selection {
                        marked: vec!["m.rs".into()],
                        cursor: None,
                        cwd: "C:\\p".into(),
                    },
                    Request::SearchFiles { .. } => Response::Files {
                        paths: vec!["src/a.rs".into()],
                        truncated: false,
                    },
                    Request::SearchContents { .. } => Response::Matches {
                        hits: vec![ContentHit {
                            path: "src/a.rs".into(),
                            line: 4,
                            text: "needle".into(),
                        }],
                        truncated: true,
                    },
                    Request::ListDir { path } if path == "bad" => Response::Error {
                        message: "no such directory".into(),
                    },
                    Request::ListDir { .. } => Response::Dir {
                        entries: vec![DirEntry {
                            name: "x".into(),
                            is_dir: true,
                            size: None,
                        }],
                    },
                    Request::Reveal { .. } | Request::HookEvent { .. } => Response::Ok,
                });
            }
        });
        let name = format!("kestrel-test-mcp-{tag}-{}", std::process::id());
        let server = IpcServer::start(
            &name,
            1,
            sink,
            CancellationToken::new(),
            IpcServerOptions::default(),
        )
        .unwrap();
        (server, seen)
    }

    struct Client {
        w: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        r: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        next: u64,
    }

    impl Client {
        async fn connect(ipc_name: &str) -> (Client, tokio::task::JoinHandle<()>) {
            let (client_io, server_io) = tokio::io::duplex(1 << 16);
            let (sr, sw) = tokio::io::split(server_io);
            let name = ipc_name.to_string();
            let server = tokio::spawn(async move {
                let svc = KestrelMcp::new(Some(name)).serve((sr, sw)).await.unwrap();
                let _ = svc.waiting().await;
            });
            let (cr, cw) = tokio::io::split(client_io);
            let mut c = Client {
                w: cw,
                r: BufReader::new(cr),
                next: 1,
            };
            let init = c
                .rpc(
                    "initialize",
                    json!({"protocolVersion":"2025-06-18","capabilities":{},
                           "clientInfo":{"name":"t","version":"0"}}),
                )
                .await;
            assert!(init.get("result").is_some(), "{init}");
            c.w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                .await
                .unwrap();
            (c, server)
        }

        async fn rpc(&mut self, method: &str, params: Value) -> Value {
            let id = self.next;
            self.next += 1;
            let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
            rpc(&mut self.w, &mut self.r, msg).await
        }

        async fn tool(&mut self, name: &str, args: Value) -> Value {
            self.rpc("tools/call", json!({"name":name,"arguments":args}))
                .await
        }
    }

    fn text_of(v: &Value) -> String {
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn all_tools_map_arguments_and_render_replies() {
        let (server, seen) = stub_server("ok");
        let (mut c, task) = Client::connect(server.name()).await;

        let r = c.tool("get_selection", json!({})).await;
        assert!(text_of(&r).contains("m.rs"), "{r}");
        assert!(r["result"]["isError"].is_null() || r["result"]["isError"] == false);

        let r = c
            .tool(
                "search_files",
                json!({"query":"main","root":"C:\\p","limit":7}),
            )
            .await;
        assert!(text_of(&r).contains("src/a.rs"), "{r}");

        let r = c
            .tool(
                "search_contents",
                json!({"pattern":"needle","glob":"*.rs","regex":true,"limit":3}),
            )
            .await;
        let t = text_of(&r);
        assert!(
            t.contains("needle") && t.contains("\"truncated\": true"),
            "{r}"
        );

        let r = c.tool("list_dir", json!({"path":"C:\\p"})).await;
        assert!(text_of(&r).contains("\"name\": \"x\""), "{r}");

        let r = c.tool("reveal", json!({"path":"C:\\p\\a.rs"})).await;
        assert_eq!(text_of(&r), "ok");

        // application errors become tool errors
        let r = c.tool("list_dir", json!({"path":"bad"})).await;
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert!(text_of(&r).contains("no such directory"));

        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                Request::GetSelection,
                Request::SearchFiles {
                    query: "main".into(),
                    root: Some("C:\\p".into()),
                    limit: 7
                },
                Request::SearchContents {
                    pattern: "needle".into(),
                    glob: Some("*.rs".into()),
                    regex: true,
                    limit: 3
                },
                Request::ListDir {
                    path: "C:\\p".into()
                },
                Request::Reveal {
                    path: "C:\\p\\a.rs".into()
                },
                Request::ListDir { path: "bad".into() },
            ]
        );
        task.abort();
    }

    #[tokio::test]
    async fn defaults_and_limit_clamping() {
        let (server, seen) = stub_server("lim");
        let (mut c, task) = Client::connect(server.name()).await;
        c.tool("search_files", json!({"query":"q","limit":999999}))
            .await;
        c.tool("search_contents", json!({"pattern":"p"})).await;
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                Request::SearchFiles {
                    query: "q".into(),
                    root: None,
                    limit: 1000
                },
                Request::SearchContents {
                    pattern: "p".into(),
                    glob: None,
                    regex: false,
                    limit: 200
                },
            ]
        );
        task.abort();
    }

    #[tokio::test]
    async fn bad_arguments_are_an_error_not_a_crash() {
        let (server, _seen) = stub_server("bad");
        let (mut c, task) = Client::connect(server.name()).await;
        let r = c.tool("search_files", json!({"limit":"many"})).await;
        assert!(
            r.get("error").is_some() || r["result"]["isError"] == true,
            "{r}"
        );
        // the session is still alive
        let r = c.tool("get_selection", json!({})).await;
        assert!(text_of(&r).contains("m.rs"), "{r}");
        task.abort();
    }
}
