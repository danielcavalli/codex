# Private turn observations and terminal panels

The owner of a local app-server can install a completed-turn observer. The terminal client can separately host interactive MCP resources. Each feature can be enabled on its own.

## Completed-turn observer

Set `CODEX_TURN_OBSERVER` to an absolute path to a trusted executable in the environment of the app-server. Only then, and only for root threads (CLI, VS Code, exec), do requests record the prefix a later observation reuses. A foreground CLI started with `--no-daemon` owns its app-server. A remote connection requires configuring the executable on the server machine.

After a successful root turn becomes idle, the host starts the executable in the system temporary directory and sends one JSON line on stdin.

```json
{"type":"completed","threadId":"<thread ID>","turnId":"<turn ID>","cwd":"<thread's directory>"}
```

`cwd` is the thread's own directory, so an observer can read its project's settings although it runs in the temporary directory.

The executable has five seconds to write and flush one line on stdout. Return `{"question":null}` when there is no work. Otherwise return `{"question":"..."}` and read the reply from stdin. The complete decision frame can contain up to 16 KiB. Logs belong on stderr. The host discards them.

A successful reply contains the existing thread sample payload.

```json
{"type":"sample","sample":{"sourceTurnId":"<turn ID>","responseId":"<response ID>","text":"...","inputTokens":12000,"cachedInputTokens":11800,"outputTokens":35}}
```

Text and usage fields can be null. A skipped sample or EOF means no result is available. Consumers should validate output against the records they offered, recheck that those records still need work, and commit an idempotent delivery before exiting. The host gives the callback five seconds for delivery. The next foreground turn or thread shutdown cancels a pending observation. Admission is best effort, with one active callback per host, twelve attempted samples per rolling hour, and a ten-minute pause after two completed samples with zero or unknown cached tokens.

The runtime uses a disposable fork of the specific completed turn, inherits its cache routing, reuses the request header and tool declarations, and checks the reconstructed input prefix against the parent request before inference. The private question runs at low reasoning effort whatever the session uses, and its response is limited to 2,048 output tokens, reasoning included, so ask for short answers. Failures are logged at warning level. Observer forks report `turn_observer` as their thread source. Foreground tool execution and panel actions have independent lifecycles. Cache retention, eviction and billing remain provider decisions.

## Open a panel

Set `CODEX_PANEL_PYTHON` on the terminal client to an absolute Python 3.11+ interpreter with curses. To run HTML apps, install Playwright in that interpreter and set `CODEX_PANEL_CHROMIUM` to a matching sandbox-capable Chromium executable. Type `/panel` to open the picker; it is listed only where `CODEX_PANEL_PYTHON` is set, and it waits until a running turn finishes, since the panel holds the terminal until it closes. Esc returns to the previous view. Native entrypoints can open on a fresh thread. HTML views open from completed MCP tool calls on the displayed thread.

HTML apps receive the MCP Apps handshake, the originating input and result, and a confirmed `tools/call` bridge. Before a call runs, the host shows every argument (scrolling), discards keys typed before the question, and runs it only on Enter once the last line has been seen. A call gets ten minutes; one that times out may still be running on its server, so the host says so instead of inviting a retry. Chromium runs with WebRTC limited to proxied UDP and no proxy, which leaves it no path out. Tab moves browser focus and Enter activates the focused control. The cell view uses the browser accessibility tree. Direct Kitty-compatible terminals also offer F2 for a rendered viewport with pointer input. The sandbox supports self-contained resources. External network resources, host chat submission and clipboard access are outside the advertised capabilities. Hosted connector actions require the original call, current tool permissions and matching connector and account binding. [MCP Apps specification](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx) · [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)

## Author a terminal resource

A client supporting the native format advertises `codex/terminal-panel` with `{"version":1}`. Set the tool's `_meta["codex/terminal-panel"].resourceUri` to a `ui://` URI. Serve the following JSON as `application/vnd.codex.terminal-panel+json`. An app-only entrypoint may set `_meta.ui.visibility` to `["app"]`.

```json
{
  "version": 1,
  "title": "Review",
  "nodes": [
    {"type":"text","text":"Item 42"},
    {"type":"field","id":"note","label":"Note"},
    {"type":"action","label":"Save","tool":"annotate","arguments":{"id":"42"},"fields":{"text":"note"}}
  ]
}
```

The host collects field values, displays the target tool and arguments, and asks for confirmation. A document-level revision is passed to an action as `revision`. Return the next document in `structuredContent`, or return a regular result and the host rereads the resource. For writes, have the server enforce its revision and idempotency rules. One document can contain up to 256 nodes. An app offering both formats supplies both resource descriptors and the picker identifies each representation.

## Maintenance

The fork pins `Cargo.lock` package versions to the release it is based on (0.172.0), so `codex --version` matches the helper package it runs with; that lockfile churn conflicts on every rebase onto a newer release and is redone there. Upstream struct literals of `ResponsesApiRequest` gain the `max_output_tokens` field, and `SlashCommand` matches gain `Panel`.
