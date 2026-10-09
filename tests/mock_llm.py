#!/usr/bin/env python3
"""Mock LLM servers for the Rift audit harness (OpenAI-style SSE + Ollama NDJSON + misc failure modes).
Usage: python3 -I mock_llm.py <logdir>   -> prints "PORT <n>" on stdout, serves until killed.
Route: /<scenario>/v1/chat/completions  or  /<scenario>/api/chat
"""
import json, os, socket, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = sys.argv[1] if len(sys.argv) > 1 else "."
os.makedirs(LOG, exist_ok=True)

def log(name, text):
    with open(os.path.join(LOG, name), "a") as f:
        f.write(text + "\n")

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass

    def _scn(self):
        return self.path.strip("/").split("/")[0]

    def do_POST(self):
        scn = self._scn()
        n = int(self.headers.get("Content-Length", "0") or 0)
        body = self.rfile.read(n)
        log(f"req_{scn}.log", json.dumps({"path": self.path, "auth": self.headers.get("Authorization"), "accept": self.headers.get("Accept"), "len": len(body)}))
        try:
            parsed = json.loads(body)
            valid = True
        except Exception as e:
            parsed, valid = None, False
            log(f"badjson_{scn}.log", f"{e}: {body[:300]!r}")
        with open(os.path.join(LOG, f"lastbody_{scn}.json"), "wb") as f:
            f.write(body)
        getattr(self, "s_" + scn, self.s_unknown)(parsed, valid)

    def send_head(self, code=200, ctype="text/event-stream", chunked=True):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Cache-Control", "no-cache")
        if chunked:
            self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

    def w(self, s):
        data = s.encode() if isinstance(s, str) else s
        self.wfile.write(b"%x\r\n" % len(data) + data + b"\r\n")
        self.wfile.flush()

    def end(self):
        self.wfile.write(b"0\r\n\r\n"); self.wfile.flush()

    def plain(self, code, body, ctype="application/json"):
        b = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def chunk(self, text):
        return "data: " + json.dumps({"choices": [{"index": 0, "delta": {"content": text}}]}) + "\n\n"

    # ---- OpenAI-style SSE ----
    def s_sse_ok(self, p, valid):
        self.send_head()
        self.w('data: {"choices":[{"delta":{"role":"assistant","content":""}}]}\n\n')
        for t in ["Use ", "`ls 中`", " and\nmore ", "done"]:
            self.w(self.chunk(t)); time.sleep(0.05)
        self.w("data: [DONE]\n\n"); self.end()

    def s_sse_stall(self, p, valid):
        self.send_head()
        self.w(self.chunk("first ")); self.w(self.chunk("second "))
        t0 = time.time()
        try:
            while time.time() - t0 < 40:
                time.sleep(1)
                self.w(": keepalive\n\n")
        except Exception as e:
            log("stall_closed.log", f"client closed after {time.time()-t0:.1f}s: {type(e).__name__}")
            return
        log("stall_closed.log", "client never closed within 40s")

    def s_sse_endless(self, p, valid):
        self.send_head()
        t0 = time.time()
        try:
            while time.time() - t0 < 60:
                self.w(self.chunk("x")); time.sleep(0.1)
        except Exception as e:
            log("endless_closed.log", f"client closed after {time.time()-t0:.1f}s: {type(e).__name__}")
            return
        log("endless_closed.log", "client never closed within 60s")

    def s_sse_500_json(self, p, valid):
        self.plain(500, '{"error":{"message":"boom upstream","type":"server_error"}}')

    def s_sse_500_html(self, p, valid):
        self.plain(502, "<html><body>Bad Gateway</body></html>", "text/html")

    def s_sse_401(self, p, valid):
        self.plain(401, '{"error":{"message":"Incorrect API key provided"}}')

    def s_sse_429(self, p, valid):
        self.send_response(429); self.send_header("Retry-After", "3"); self.send_header("Content-Length", "0"); self.end_headers()

    def s_sse_malformed(self, p, valid):
        self.send_head()
        self.w("data: {bad json\n\n"); self.w("data: \n\n"); self.w("data: [1,2\n\n")
        self.w(self.chunk("ok")); self.w("data: [DONE]\n\n"); self.end()

    def s_sse_garbage_only(self, p, valid):
        self.send_head(ctype="text/html")
        self.w("<html>not an sse stream</html>\n"); self.end()

    def s_sse_crlf(self, p, valid):
        self.send_head()
        self.w(self.chunk("a").replace("\n", "\r\n")); self.w(self.chunk("b").replace("\n", "\r\n")); self.w("data: [DONE]\r\n\r\n"); self.end()

    def s_sse_cut(self, p, valid):
        self.send_head()
        self.w(self.chunk("partial "))
        self.wfile.flush()
        self.connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, b"\x01\x00\x00\x00\x00\x00\x00\x00")
        # close() alone leaves the fd open while rfile/wfile exist, so nothing was ever reset;
        # shutdown() really tears the connection down mid-chunked-body.
        try:
            self.connection.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.close_connection = True
        self.connection.close()  # RST mid-stream

    def s_sse_unicode_escape(self, p, valid):
        self.send_head()
        self.w('data: {"choices":[{"delta":{"content":"\\u4e2d\\u6587 \\ud83d\\ude00 \\u00e9 \\n tab\\t q\\" bs\\\\ sl\\/"}}]}\n\n')
        self.w("data: [DONE]\n\n"); self.end()

    def s_sse_reasoning(self, p, valid):
        self.send_head()
        self.w('data: {"choices":[{"delta":{"reasoning_content":"thinking..."}}]}\n\n')
        self.w(self.chunk("answer")); self.w("data: [DONE]\n\n"); self.end()

    def s_sse_longline(self, p, valid):
        self.send_head()
        try:
            self.w("data: " + "a" * (1 << 20))
            for _ in range(150):
                self.w("a" * (1 << 20))  # 150 MB and never a newline
        except Exception as e:
            log("longline_closed.log", f"client closed: {type(e).__name__}")
            return
        self.end()

    def s_sse_noresp(self, p, valid):
        time.sleep(45)

    def s_sse_slowstart(self, p, valid):
        time.sleep(12)
        self.s_sse_ok(p, valid)

    # ---- Ollama NDJSON ----
    def s_nd_ok(self, p, valid):
        self.send_head(ctype="application/x-ndjson")
        for t in ["Hel", "lo ", "世界"]:
            self.w(json.dumps({"model": "m", "message": {"role": "assistant", "content": t}, "done": False}) + "\n"); time.sleep(0.05)
        self.w(json.dumps({"model": "m", "message": {"role": "assistant", "content": ""}, "done": True}) + "\n"); self.end()

    def s_nd_error(self, p, valid):
        self.send_head(ctype="application/x-ndjson")
        self.w(json.dumps({"message": {"content": "partial"}, "done": False}) + "\n")
        self.w(json.dumps({"error": "model runner crashed"}) + "\n"); self.end()

    def s_nd_trunc(self, p, valid):
        self.send_head(ctype="application/x-ndjson")
        self.w(json.dumps({"message": {"content": "cut"}, "done": False}) + "\n")
        self.w('{"message":{"content":"half')  # truncated JSON, then close
        self.end()

    def s_nd_404(self, p, valid):
        self.plain(404, '{"error":"model \\"nope\\" not found, try pulling it first"}')

    # ---- non-stream (backend.rs) ----
    def s_echo(self, p, valid):
        if not valid:
            return self.plain(400, '{"error":{"message":"request body is not valid JSON"}}')
        self.plain(200, json.dumps({"choices": [{"message": {"role": "assistant", "content": "echo:" + p["messages"][-1]["content"][:40]}}]}))

    def s_echo_ascii(self, p, valid):  # python-style ensure_ascii output
        self.plain(200, json.dumps({"choices": [{"message": {"role": "assistant", "content": "中文 é 😀"}}]}))

    def s_echo_null(self, p, valid):  # tool-call style: content null before a later "content" key
        self.plain(200, '{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"function":{"name":"f","arguments":"{\\"content\\":\\"WRONG\\"}"}}]}}]}')

    def s_echo_nd(self, p, valid):
        if not valid:
            return self.plain(400, '{"error":"invalid json"}')
        self.plain(200, json.dumps({"message": {"role": "assistant", "content": "ollama-echo"}, "done": True}))

    def s_unknown(self, p, valid):
        self.plain(404, '{"error":"no scenario"}')

class S(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 64

srv = S(("127.0.0.1", 0), H)
print("PORT", srv.server_address[1], flush=True)
srv.serve_forever()
