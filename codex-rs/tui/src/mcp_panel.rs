//! Hand the terminal to the bundled panel renderer. The renderer owns no app-server credential.
//! This bridge pins every RPC to the backend and thread that opened the view.
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_protocol::ThreadId;
use color_eyre::eyre::{Result, ensure, eyre};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

pub(crate) async fn open(client: AppServerRequestHandle, thread: ThreadId) -> Result<()> {
    let python = std::env::var_os("CODEX_PANEL_PYTHON")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| eyre!("set CODEX_PANEL_PYTHON to an absolute Python executable"))?;
    let mut child = Command::new(python)
        .args(["-I", "-c", include_str!("mcp_panel.py")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| eyre!("missing panel stdin"))?;
    let mut output = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| eyre!("missing panel stdout"))?,
    );
    loop {
        let mut line = Vec::new();
        let count = (&mut output)
            .take(65537)
            .read_until(b'\n', &mut line)
            .await?;
        if count == 0 {
            break;
        }
        ensure!(
            count <= 65536 && line.ends_with(b"\n"),
            "invalid panel request frame"
        );
        let msg: Value = serde_json::from_slice(&line)?;
        let method = msg["method"]
            .as_str()
            .ok_or_else(|| eyre!("missing panel method"))?;
        ensure!(
            matches!(
                method,
                "thread/turns/list"
                    | "mcpServerStatus/list"
                    | "mcpServer/resource/read"
                    | "mcpServer/tool/call"
            ),
            "unsupported panel method"
        );
        let mut params = msg["params"].as_object().cloned().unwrap_or_default();
        params.insert("threadId".into(), json!(thread));
        let request: ClientRequest = serde_json::from_value(json!({
            "method": method, "id": Uuid::new_v4().to_string(), "params": params
        }))?;
        // A tool call may legitimately run long, and timing out does not stop it on its server;
        // reads answer quickly or not at all.
        let calls_tool = method == "mcpServer/tool/call";
        let limit = Duration::from_secs(if calls_tool { 600 } else { 10 });
        let reply = match timeout(limit, client.request_typed::<Value>(request)).await {
            Ok(Ok(value)) => json!({"result":value}),
            Ok(Err(codex_app_server_client::TypedRequestError::Server { source, .. }))
                if method == "thread/turns/list"
                    && source.code == -32600
                    && source.message.contains("not materialized yet") =>
            {
                json!({"result":{"data":[],"nextCursor":null}})
            }
            Ok(Err(error)) => json!({"error":error.to_string()}),
            Err(_) if calls_tool => json!({"error":
                "No result after 10 minutes. The tool may still be running on its server; running \
                 it again may repeat its effects."}),
            Err(_) => json!({"error":"The request timed out."}),
        };
        input.write_all(format!("{reply}\n").as_bytes()).await?;
    }
    ensure!(
        child.wait().await?.success(),
        "panel renderer exited unsuccessfully"
    );
    Ok(())
}
