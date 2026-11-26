"""Local renderer embedded in the Codex TUI. Plugin content never runs in this process."""
import asyncio
import base64
import curses
import json
import os
import sys
import textwrap
import unicodedata

HTML = "text/html;profile=mcp-app"
NATIVE = "application/vnd.codex.terminal-panel+json"
HOST = "https://codex-panel.invalid"
PROXY = "https://codex-sandbox.invalid"


def safe(value):
    return "".join(c if c == "\n" or unicodedata.category(c)[0] != "C" else " " for c in str(value))


class Wire:
    def __init__(self):
        self.reader = os.fdopen(os.dup(0), "rb")
        self.writer = os.fdopen(os.dup(1), "wb")
        self.lock = asyncio.Lock()

    def exchange(self, method, params):
        self.writer.write(json.dumps(dict(method=method, params=params)).encode() + b"\n")
        self.writer.flush()
        line = self.reader.readline(16 * 1024 * 1024 + 1)
        if not line.endswith(b"\n"):
            raise ValueError("Oversize or incomplete host response")
        response = json.loads(line)
        if "error" in response:
            raise ValueError(response["error"])
        return response["result"]

    async def rpc(self, method, **params):
        async with self.lock:
            return await asyncio.to_thread(self.exchange, method, params)


class Screen:
    def __init__(self, screen):
        self.screen = screen
        screen.keypad(True)
        screen.nodelay(True)

    def draw(self, title, lines, footer, offset=0):
        h, w = self.screen.getmaxyx()
        self.screen.erase()

        def line(row, value, attr=0):
            if w > 1 and 0 <= row < h:
                try:
                    self.screen.addnstr(row, 0, safe(value).replace("\n", " "), w - 1, attr)
                except curses.error:
                    pass

        line(0, title, curses.A_BOLD)
        for i, value in enumerate(lines[offset:offset + max(0, h - 3)]):
            line(i + 1, value)
        line(h - 1, footer, curses.A_REVERSE)
        self.screen.refresh()

    async def key(self):
        while True:
            try:
                return self.screen.get_wch()
            except curses.error:
                await asyncio.sleep(0.04)

    async def choose(self, title, rows, message="", selected=0):
        while True:
            h, _ = self.screen.getmaxyx()
            self.draw(title, [("› " if i == selected else "  ") + s for i, s in enumerate(rows)]
                      or [message], "↑ ↓  Select     Enter  Open     Esc  Back", max(0, selected - max(1, h - 5)))
            key = await self.key()
            if key in ("\x1b", "q"):
                return None
            if rows and key in ("\n", "\r", curses.KEY_ENTER):
                return selected
            if key in ("j", curses.KEY_DOWN):
                selected = min(max(0, len(rows) - 1), selected + 1)
            if key in ("k", curses.KEY_UP):
                selected = max(0, selected - 1)

    async def edit(self, label, value):
        while True:
            self.draw(label, [value], "Type to edit     Enter  Save     Esc  Cancel")
            key = await self.key()
            if key == "\x1b":
                return None
            if key in ("\n", "\r", curses.KEY_ENTER):
                return value
            if key in ("\b", "\x7f", curses.KEY_BACKSPACE):
                value = value[:-1]
            elif isinstance(key, str) and key.isprintable() and len(value) < 4096:
                value += key


async def inventory(wire, **filters):
    rows, cursors, cursor = [], set(), None
    while True:
        page = await wire.rpc("mcpServerStatus/list", detail="toolsAndAuthOnly", cursor=cursor, **filters)
        rows.extend(page["data"])
        cursor = page.get("nextCursor")
        if not cursor:
            return rows
        if cursor in cursors:
            raise ValueError("Inventory repeated a cursor")
        cursors.add(cursor)


def formats(tool, html_enabled, historical_uri=None):
    meta = tool.get("_meta") or {}
    native = (meta.get("codex/terminal-panel") or {}).get("resourceUri")
    html = historical_uri or (meta.get("ui") or {}).get("resourceUri") or meta.get("ui/resourceUri")
    return [(mime, uri) for mime, uri in ((NATIVE, native), (HTML, html if html_enabled else None))
            if isinstance(uri, str) and uri.startswith("ui://")]


class Panel:
    def __init__(self, row, tool, mime, uri, item):
        self.target = {"server": row["name"], "environmentId": row.get("environmentId")}
        self.identity = tuple(row.get(k) for k in ("pluginId", "httpOrigin", "serverInfo"))
        self.tool = tool
        self.mime = mime
        self.uri = uri
        self.item = item

    def label(self):
        mode = "Terminal" if self.mime == NATIVE else "HTML"
        origin = self.item["id"][:10] if self.item else "entrypoint"
        return f"{self.target['server']} / {self.tool} · {mode} · {origin}"

    async def read(self, wire):
        params = dict(self.target, uri=self.uri)
        if self.item:
            params["originCallId"] = self.item["id"]
            app = self.item.get("appContext") or {}
            if app.get("connectorId"):
                params["connectorId"] = app["connectorId"]
        result = await wire.rpc("mcpServer/resource/read", **params)
        items = result["contents"]
        if len(items) != 1 or items[0].get("uri") != self.uri:
            raise ValueError("Resource identity changed")
        item = items[0]
        if item.get("mimeType", "").replace(" ", "").lower() != self.mime:
            raise ValueError("Unexpected panel media type")
        data = item.get("text")
        if data is None:
            data = base64.b64decode(item["blob"], validate=True).decode("utf-8")
        if len(data.encode()) > 8 * 1024 * 1024:
            raise ValueError("Panel resource too large")
        return data

    async def call(self, wire, screen, name, args):
        # The current call RPC cannot bind a hosted connector account to its originating call.
        if self.target["server"] == "codex_apps" or (self.item or {}).get("appContext"):
            raise ValueError("This app requires account-bound tool routing")
        if not isinstance(name, str) or not isinstance(args, dict):
            raise ValueError("Invalid tool request")
        rows = await inventory(wire, serverName=self.target["server"],
                               environmentId=self.target["environmentId"])
        matches = [row for row in rows if row["name"] == self.target["server"]
                   and row.get("environmentId") == self.target["environmentId"]]
        if len(matches) != 1 or tuple(matches[0].get(k) for k in ("pluginId", "httpOrigin", "serverInfo")) != self.identity:
            raise ValueError("Panel registration changed")
        tool = matches[0]["tools"].get(name)
        if not tool or "app" not in ((tool.get("_meta") or {}).get("ui") or {}).get("visibility", ["model", "app"]):
            raise ValueError("Tool unavailable to this app")
        title = f"{self.target['server']} / {name}"
        screen.draw(title, textwrap.wrap(safe(json.dumps(args, ensure_ascii=False)), max(12, screen.screen.getmaxyx()[1]-2)),
                    "Run this tool?  y  Run     any other key  Cancel")
        if await screen.key() != "y":
            raise ValueError("Action cancelled")
        return await wire.rpc("mcpServer/tool/call", **self.target, tool=name, arguments=args)


async def discover(wire, html_enabled):
    rows = await inventory(wire)
    panels = []
    by_key = {(row["name"], row.get("environmentId")): row for row in rows}
    for row in rows:
        for name, tool in (row.get("tools") or {}).items():
            for mime, uri in formats(tool, html_enabled):
                if mime == NATIVE:
                    panels.append(Panel(row, name, mime, uri, None))
    cursor = None
    seen = set()
    while True:
        page = await wire.rpc("thread/turns/list", itemsView="full", limit=8, sortDirection="desc", cursor=cursor)
        for turn in page["data"]:
            for item in reversed(turn["items"]):
                if item.get("type") != "mcpToolCall" or item.get("status") != "completed":
                    continue
                row = by_key.get((item["server"], item.get("environmentId")))
                if row is None:
                    continue
                tool = (row.get("tools") or {}).get(item["tool"])
                if tool is None:
                    continue
                uri = (item.get("mcpAppUi") or {}).get("resourceUri") or item.get("mcpAppResourceUri")
                for mime, address in formats(tool, html_enabled, uri):
                    panels.append(Panel(row, item["tool"], mime, address, item))
        cursor = page.get("nextCursor")
        if not cursor or panels or cursor in seen:
            return panels
        seen.add(cursor)


async def native(panel, wire, screen, data):
    model = json.loads(data)
    values = {}
    index = 0
    while True:
        if model.get("version") != 1 or not isinstance(model.get("nodes"), list) or len(model["nodes"]) > 256:
            raise ValueError("Unsupported terminal panel document")
        nodes = model["nodes"]
        labels = []
        for node in nodes:
            label = str(node.get("label", node.get("text", "")))
            if node["type"] == "field":
                label += " = " + str(values.setdefault(node["id"], node.get("value", "")))
            labels.append(label)
        index = await screen.choose(model.get("title", panel.tool), labels, selected=min(index, max(0,len(nodes)-1)))
        if index is None:
            return
        node = nodes[index]
        if node["type"] == "field":
            value = await screen.edit(node["label"], str(values[node["id"]]))
            if value is not None:
                values[node["id"]] = value
        elif node["type"] == "action":
            args = dict(node.get("arguments", {}))
            args.update({name: values[source] for name, source in node.get("fields", {}).items()})
            if "revision" in model:
                args["revision"] = model["revision"]
            response = await panel.call(wire, screen, node["tool"], args)
            if response.get("isError"):
                raise ValueError("Panel action failed")
            model = response.get("structuredContent") or json.loads(await panel.read(wire))
            values.clear()

# The trusted document and sandbox proxy have separate virtual HTTPS origins.
# The view itself has an opaque origin. Only the trusted document can invoke the broker.
HOST_PAGE = """<!doctype html><meta charset=utf-8><body><script>
const proxy = document.createElement('iframe');
proxy.sandbox = 'allow-scripts allow-same-origin';
proxy.src = 'https://codex-sandbox.invalid/';
window.deliver = msg => proxy.contentWindow.postMessage(msg,'https://codex-sandbox.invalid');
window.addEventListener('message', e => {
  if(e.source===proxy.contentWindow && e.origin==='https://codex-sandbox.invalid') {
    window.panelReceive(e.data).catch(()=>{});
  }
});
document.body.append(proxy);
</script>"""
PROXY_PAGE = """<!doctype html><meta charset=utf-8><body><script>
const app = document.createElement('iframe');
app.sandbox = 'allow-scripts';
const host = 'https://codex-panel.invalid';
window.addEventListener('message', e => {
  if(e.source===parent && e.origin===host) {
    if(e.data?.method==='ui/notifications/sandbox-resource-ready') {
      app.srcdoc=e.data.params.html;
      document.body.append(app);
    } else if(app.contentWindow) app.contentWindow.postMessage(e.data,'*');
  } else if(e.source===app.contentWindow && !String(e.data?.method||'').startsWith('ui/notifications/sandbox-')) {
    parent.postMessage(e.data,host);
  }
});
parent.postMessage({jsonrpc:'2.0',method:'ui/notifications/sandbox-proxy-ready'},host);
</script>"""


class WebPanel:
    def __init__(self, panel, wire, data):
        self.panel = panel
        self.wire = wire
        self.data = data
        self.actions = asyncio.Queue(maxsize=1)
        self.initialized = False
        self.closed = False

    async def send(self, message):
        await self.page.evaluate("msg => window.deliver(msg)", {"jsonrpc":"2.0", **message})

    async def receive(self, source, msg):
        if self.closed or source["frame"] != self.page.main_frame or not isinstance(msg, dict):
            return
        method = msg.get("method")
        rid = msg.get("id")
        if method == "ui/notifications/sandbox-proxy-ready":
            await self.send(dict(method="ui/notifications/sandbox-resource-ready", params={"html":self.data}))
        elif method == "ui/initialize" and not self.initialized:
            await self.send(dict(id=rid, result={
                "protocolVersion":"2026-01-26",
                "hostInfo":{"name":"codex-tui", "version":"1"},
                "hostCapabilities":{"serverTools":{}},
                "hostContext":{"displayMode":"fullscreen","availableDisplayModes":["fullscreen"],
                               "platform":"desktop","theme":"dark"},
            }))
        elif method == "ui/notifications/initialized" and not self.initialized:
            self.initialized = True
            item = self.panel.item or {}
            await self.send(dict(method="ui/notifications/tool-input", params={"arguments":item.get("arguments",{})}))
            if item.get("result"):
                await self.send(dict(method="ui/notifications/tool-result", params=item["result"]))
        elif self.initialized and method == "tools/call" and rid is not None and not self.actions.full():
            self.actions.put_nowait((rid, msg.get("params") or {}))
        elif method and rid is not None:
            await self.send(dict(id=rid, error={"code":-32601,"message":"Unsupported or busy panel request"}))

    async def start(self, playwright):
        self.browser = await playwright.chromium.launch(
            executable_path=os.environ["CODEX_PANEL_CHROMIUM"], chromium_sandbox=True)
        self.context = await self.browser.new_context(accept_downloads=False, service_workers="block")
        self.page = await self.context.new_page()
        self.page.set_default_timeout(3000)

        async def route(req):
            url = req.request.url
            if url == HOST + "/":
                html = HOST_PAGE
                csp = "default-src 'none'; script-src 'unsafe-inline'; frame-src " + PROXY
            elif url == PROXY + "/":
                html = PROXY_PAGE
                csp = ("default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; "
                       "img-src data: blob:; font-src data:; frame-src about:; object-src 'none'; "
                       "form-action 'none'; base-uri 'none'; frame-ancestors " + HOST)
            else:
                await req.abort()
                return
            await req.fulfill(status=200, content_type="text/html", body=html, headers={
                "Content-Security-Policy":csp, "Referrer-Policy":"no-referrer",
                "Permissions-Policy":"camera=(),microphone=(),geolocation=(),clipboard-write=()",
            })

        await self.context.route("**/*", route)
        await self.page.expose_binding("panelReceive", self.receive)
        await self.page.goto(HOST + "/")
        await asyncio.wait_for(self.ready(), 6)

    async def ready(self):
        while not self.initialized:
            await asyncio.sleep(0.05)
        self.frame = next(frame for frame in self.page.frames if frame.url == "about:srcdoc")

    async def action(self, screen):
        rid, params = self.actions.get_nowait()
        try:
            result = await self.panel.call(self.wire, screen, params.get("name"), params.get("arguments",{}))
            await self.send(dict(id=rid,result=result))
        except ValueError as error:
            await self.send(dict(id=rid,error={"code":-32000,"message":str(error)}))

    async def run(self, screen):
        offset = 0
        size = None
        while True:
            if not self.actions.empty():
                await self.action(screen)
            h, w = screen.screen.getmaxyx()
            if size != (w,h):
                size = (w,h)
                await self.page.set_viewport_size({"width":max(180,w*8),"height":max(100,h*16)})
            tree = await self.frame.locator("body").aria_snapshot(timeout=2000)
            lines = [line for s in tree.splitlines() for line in textwrap.wrap(s,width=max(10,w-2))]
            screen.draw(self.panel.tool, lines, "Tab  Focus    Enter  Activate    PgUp/PgDn  Scroll    Esc  Close", offset)
            try:
                key = await asyncio.wait_for(screen.key(),0.25)
            except TimeoutError:
                continue
            if key == "\x1b":
                return
            if key in (curses.KEY_NPAGE,curses.KEY_PPAGE):
                offset = max(0,min(max(0,len(lines)-h+3),offset+(h-3)*(1 if key==curses.KEY_NPAGE else -1)))
                continue
            keys = {"\t":"Tab", curses.KEY_BTAB:"Shift+Tab","\n":"Enter","\r":"Enter",
                    curses.KEY_DOWN:"ArrowDown",curses.KEY_UP:"ArrowUp",curses.KEY_LEFT:"ArrowLeft",
                    curses.KEY_RIGHT:"ArrowRight", curses.KEY_BACKSPACE:"Backspace","\x7f":"Backspace"," ":"Space"}
            if key in keys:
                await self.page.keyboard.press(keys[key])
            elif isinstance(key,str) and key.isprintable():
                await self.page.keyboard.insert_text(key)

    async def close(self):
        self.closed = True
        if self.initialized:
            await self.send(dict(id="teardown",method="ui/resource-teardown",params={}))
        if hasattr(self,"browser"):
            await self.browser.close()


async def render(panel, wire, screen):
    data = await panel.read(wire)
    if panel.mime == NATIVE:
        await native(panel, wire, screen, data)
        return
    from playwright.async_api import async_playwright
    async with async_playwright() as playwright:
        app = WebPanel(panel,wire,data)
        try:
            await app.start(playwright)
            await app.run(screen)
        finally:
            await app.close()


async def main(wire, screen):
    try:
        panels = await discover(wire, bool(os.environ.get("CODEX_PANEL_CHROMIUM")))
        while True:
            index = await screen.choose("MCP panels", [p.label() for p in panels], "No panels available on this thread.")
            if index is None:
                return
            try:
                await render(panels[index],wire,screen)
            except Exception as error:
                await screen.choose("Panel unavailable", [], safe(error))
    except Exception as error:
        await screen.choose("Could not open panels", [], safe(error))


if __name__ == "__main__":
    wire = Wire()
    with open("/dev/tty", "rb+", buffering=0) as tty:
        os.dup2(tty.fileno(),0)
        os.dup2(tty.fileno(),1)
        curses.wrapper(lambda stdscr: asyncio.run(main(wire, Screen(stdscr))))
