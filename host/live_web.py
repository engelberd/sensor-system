from __future__ import annotations

import json
import threading
import time
from collections import deque
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Deque, Optional
from urllib.parse import parse_qs, urlparse


@dataclass(frozen=True)
class LiveSample:
    node_id: int
    sample_seq: int
    x: int
    y: int
    z: int
    packet_seq: int


@dataclass(frozen=True)
class LiveGap:
    node_id: int
    expected_sample_seq: int
    received_sample_seq: int
    packet_seq: int


class LiveBuffer:
    def __init__(self, max_samples: int = 50_000, max_gaps: int = 1_000) -> None:
        self._lock = threading.Lock()
        self._cv = threading.Condition(self._lock)
        self._samples: dict[int, Deque[LiveSample]] = {}
        self._gaps: dict[int, Deque[LiveGap]] = {}
        self._max_samples = max_samples
        self._max_gaps = max_gaps
        self._seq = 0
        self._meta: dict[str, Any] = {}

    def set_meta(self, meta: dict[str, Any]) -> None:
        with self._cv:
            self._meta = dict(meta)
            self._seq += 1
            self._cv.notify_all()

    def meta(self) -> dict[str, Any]:
        with self._lock:
            return dict(self._meta)

    def publish_samples(self, node_id: int, samples: list[LiveSample]) -> None:
        if not samples:
            return
        with self._cv:
            q = self._samples.get(node_id)
            if q is None:
                q = deque(maxlen=self._max_samples)
                self._samples[node_id] = q
            q.extend(samples)
            self._seq += 1
            self._cv.notify_all()

    def publish_gap(self, gap: LiveGap) -> None:
        with self._cv:
            q = self._gaps.get(gap.node_id)
            if q is None:
                q = deque(maxlen=self._max_gaps)
                self._gaps[gap.node_id] = q
            q.append(gap)
            self._seq += 1
            self._cv.notify_all()

    def wait_for_update(self, last_seq: int, timeout_s: float) -> int:
        with self._cv:
            if self._seq != last_seq:
                return self._seq
            self._cv.wait(timeout=timeout_s)
            return self._seq

    def snapshot(self, node_id: int) -> tuple[list[LiveSample], list[LiveGap], int]:
        with self._lock:
            samples = list(self._samples.get(node_id, ()))
            gaps = list(self._gaps.get(node_id, ()))
            return samples, gaps, self._seq


INDEX_HTML = """<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Sensor System Live</title>
    <style>
      :root { color-scheme: dark; }
      body { font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, Ubuntu; margin: 16px; }
      h1 { font-size: 18px; margin: 0 0 12px; }
      .row { display: flex; flex-wrap: wrap; gap: 12px; align-items: center; margin-bottom: 12px; }
      .card { border: 1px solid #2a2a2a; border-radius: 10px; padding: 12px; background: #111; }
      label { display: inline-flex; gap: 8px; align-items: center; }
      select, input { background: #0b0b0b; border: 1px solid #2a2a2a; color: #eee; border-radius: 6px; padding: 6px 8px; }
      canvas { width: 100%; height: 280px; display: block; background: #070707; border-radius: 10px; }
      .grid { display: grid; grid-template-columns: 1fr; gap: 12px; }
      @media (min-width: 1100px) { .grid { grid-template-columns: 1fr 1fr; } }
      .muted { color: #aaa; font-size: 12px; }
      .stat { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 12px; white-space: pre; }
    </style>
  </head>
  <body>
    <h1>Sensor System Live</h1>
    <div class="row card">
      <label>Node <select id="node"></select></label>
      <label>Axis <select id="axis"><option value="x">x</option><option value="y">y</option><option value="z">z</option></select></label>
      <label>Window <select id="win"><option value="hann">hann</option><option value="rect">rect</option></select></label>
      <label>FFT N <select id="fftN"><option>256</option><option>512</option><option selected>1024</option><option>2048</option></select></label>
      <label>Plot N <input id="plotN" type="number" min="128" max="20000" step="128" value="2048" /></label>
      <span class="muted">PSD scale: 0..-80 dB/Hz (relative)</span>
      <span class="muted" id="odr"></span>
    </div>
    <div class="grid">
      <div class="card">
        <div class="muted">Time domain</div>
        <canvas id="time"></canvas>
        <div class="stat" id="tstat"></div>
      </div>
      <div class="card">
        <div class="muted">PSD (FFT, dB/Hz, relative)</div>
        <canvas id="fft"></canvas>
        <div class="stat" id="fstat"></div>
      </div>
    </div>

    <script>
      const state = {
        nodeId: 1,
        axis: "x",
        outputOdrHz: 0,
        samples: [],
        gaps: [],
      };

      function $(id){ return document.getElementById(id); }

      function resizeCanvas(c){
        const dpr = window.devicePixelRatio || 1;
        const rect = c.getBoundingClientRect();
        c.width = Math.max(2, Math.floor(rect.width * dpr));
        c.height = Math.max(2, Math.floor(rect.height * dpr));
      }

      function drawSeries(canvas, values, color="#62d0ff"){
        const ctx = canvas.getContext("2d");
        ctx.clearRect(0,0,canvas.width,canvas.height);
        if (!values.length) return;
        let minV = values[0], maxV = values[0];
        for (let i=1;i<values.length;i++){ const v=values[i]; if (v<minV) minV=v; if (v>maxV) maxV=v; }
        const pad = (maxV-minV)*0.05 || 1;
        minV -= pad; maxV += pad;
        const w=canvas.width, h=canvas.height;
        ctx.strokeStyle = "#222"; ctx.lineWidth = 1;
        for (let i=0;i<=4;i++){
          const y = Math.floor((i/4)*h);
          ctx.beginPath(); ctx.moveTo(0,y); ctx.lineTo(w,y); ctx.stroke();
        }
        ctx.strokeStyle = color; ctx.lineWidth = 2;
        ctx.beginPath();
        for (let i=0;i<values.length;i++){
          const x = (i/(values.length-1))*w;
          const y = h - ((values[i]-minV)/(maxV-minV))*h;
          if (i===0) ctx.moveTo(x,y); else ctx.lineTo(x,y);
        }
        ctx.stroke();
        ctx.fillStyle = "#aaa"; ctx.font = "12px ui-monospace, monospace";
        ctx.fillText(`min=${minV.toFixed(2)} max=${maxV.toFixed(2)}`, 8, 16);
      }

      function hannWindow(n, i){
        return 0.5 - 0.5 * Math.cos((2*Math.PI*i)/(n-1));
      }

      // In-place radix-2 FFT (iterative). re/im are Float64Array length N.
      function fftRadix2(re, im){
        const n = re.length;
        // bit-reversal
        for (let i=1,j=0;i<n;i++){
          let bit = n >> 1;
          for (; j & bit; bit >>= 1) j ^= bit;
          j ^= bit;
          if (i < j){
            [re[i], re[j]] = [re[j], re[i]];
            [im[i], im[j]] = [im[j], im[i]];
          }
        }
        for (let len=2; len<=n; len<<=1){
          const ang = -2*Math.PI/len;
          const wlenRe = Math.cos(ang), wlenIm = Math.sin(ang);
          for (let i=0; i<n; i+=len){
            let wRe=1, wIm=0;
            for (let j=0; j<len/2; j++){
              const uRe = re[i+j], uIm = im[i+j];
              const vRe = re[i+j+len/2]*wRe - im[i+j+len/2]*wIm;
              const vIm = re[i+j+len/2]*wIm + im[i+j+len/2]*wRe;
              re[i+j] = uRe + vRe;
              im[i+j] = uIm + vIm;
              re[i+j+len/2] = uRe - vRe;
              im[i+j+len/2] = uIm - vIm;
              const nextWRe = wRe*wlenRe - wIm*wlenIm;
              const nextWIm = wRe*wlenIm + wIm*wlenRe;
              wRe = nextWRe; wIm = nextWIm;
            }
          }
        }
      }

      function computePsdDb(values, sampleRateHz, winName){
        const n = values.length;
        const fs = sampleRateHz || 0;
        if (!fs) return { freqs: new Float64Array(0), psdDb: new Float64Array(0) };

        const win = new Float64Array(n);
        let winPowSum = 0;
        for (let i=0;i<n;i++){
          const w = (winName === "hann") ? hannWindow(n, i) : 1.0;
          win[i] = w;
          winPowSum += w*w;
        }

        const re = new Float64Array(n);
        const im = new Float64Array(n);
        for (let i=0;i<n;i++){
          re[i] = values[i] * win[i];
        }
        fftRadix2(re, im);
        const half = n/2;
        const psdDb = new Float64Array(half);
        const freqs = new Float64Array(half);

        // Periodogram PSD:
        // Pxx[k] = |X[k]|^2 / (fs * sum(w^2))
        // One-sided: multiply by 2 for k in (1..half-1).
        const denom = fs * winPowSum;
        for (let k=0;k<half;k++){
          const p = (re[k]*re[k] + im[k]*im[k]) / Math.max(1e-12, denom);
          const oneSided = (k === 0) ? p : (2.0 * p);
          psdDb[k] = 10*Math.log10(oneSided + 1e-24);
          freqs[k] = (k * fs) / n;
        }
        return { freqs, psdDb };
      }

      function colorMapTurbo(t){
        // Simple approximate "turbo" colormap. t in [0,1]
        t = Math.max(0, Math.min(1, t));
        const r = Math.max(0, Math.min(1, 1.0 - Math.abs(1.0 - 2.0*t)));
        const g = Math.max(0, Math.min(1, 1.0 - Math.abs(0.5 - 2.0*t)));
        const b = Math.max(0, Math.min(1, 1.0 - Math.abs(0.0 - 2.0*t)));
        return [Math.floor(255*r), Math.floor(255*g), Math.floor(255*b)];
      }

      function drawPsd(canvas, freqs, psdDb, minDb, maxDb){
        const ctx = canvas.getContext("2d");
        ctx.clearRect(0,0,canvas.width,canvas.height);
        if (!psdDb.length) return;

        const w = canvas.width, h = canvas.height;

        // Grid
        ctx.strokeStyle = "#222"; ctx.lineWidth = 1;
        for (let i=0;i<=4;i++){
          const y = Math.floor((i/4)*h);
          ctx.beginPath(); ctx.moveTo(0,y); ctx.lineTo(w,y); ctx.stroke();
        }
        for (let i=0;i<=4;i++){
          const x = Math.floor((i/4)*w);
          ctx.beginPath(); ctx.moveTo(x,0); ctx.lineTo(x,h); ctx.stroke();
        }

        // Plot
        ctx.strokeStyle = "#8cff6a"; ctx.lineWidth = 2;
        ctx.beginPath();
        for (let i=0;i<psdDb.length;i++){
          const x = (i / Math.max(1, psdDb.length - 1)) * w;
          const db = Math.max(minDb, Math.min(maxDb, psdDb[i]));
          const y = h - ((db - minDb) / (maxDb - minDb)) * h;
          if (i===0) ctx.moveTo(x,y); else ctx.lineTo(x,y);
        }
        ctx.stroke();

        // Labels
        ctx.fillStyle = "#aaa"; ctx.font = "12px ui-monospace, monospace";
        const fMax = freqs.length ? freqs[freqs.length - 1] : 0;
        ctx.fillText(`0..${fMax.toFixed(1)} Hz`, 8, 16);
        ctx.fillText(`${maxDb}..${minDb} dB/Hz`, 8, 32);

        // X tick labels (0, 1/4, 1/2, 3/4, max)
        ctx.fillStyle = "#888";
        const ticks = 4;
        for (let i=0;i<=ticks;i++){
          const frac = i/ticks;
          const x = Math.floor(frac*w);
          const f = frac*fMax;
          const label = f.toFixed(1);
          const tw = ctx.measureText(label).width;
          ctx.fillText(label, Math.max(2, Math.min(w - tw - 2, x - tw/2)), h - 6);
        }
      }

      async function loadMeta(){
        const resp = await fetch("/api/meta");
        const meta = await resp.json();
        const nodes = meta.nodes || [];
        const nodeSel = $("node");
        nodeSel.innerHTML = "";
        for (const n of nodes){
          const opt = document.createElement("option");
          opt.value = String(n.node_id);
          opt.textContent = String(n.node_id);
          nodeSel.appendChild(opt);
        }
        state.outputOdrHz = meta.output_odr_hz || 0;
        $("odr").textContent = state.outputOdrHz ? `output_odr=${state.outputOdrHz} Hz` : "";
        if (nodes.length){
          state.nodeId = nodes[0].node_id;
          nodeSel.value = String(state.nodeId);
        }
      }

      function connectStream(){
        const url = `/api/stream?node_id=${encodeURIComponent(state.nodeId)}`;
        const es = new EventSource(url);
        es.onmessage = (evt) => {
          const msg = JSON.parse(evt.data);
          if (msg.type === "samples"){
            for (const s of msg.samples){ state.samples.push(s); }
            const maxKeep = 200000;
            if (state.samples.length > maxKeep) state.samples.splice(0, state.samples.length - maxKeep);
          } else if (msg.type === "gap"){
            state.gaps.push(msg.gap);
            if (state.gaps.length > 10000) state.gaps.splice(0, state.gaps.length - 10000);
          } else if (msg.type === "meta"){
            if (typeof msg.output_odr_hz === "number") state.outputOdrHz = msg.output_odr_hz;
            $("odr").textContent = state.outputOdrHz ? `output_odr=${state.outputOdrHz} Hz` : "";
          }
        };
        es.onerror = () => {
          es.close();
          setTimeout(connectStream, 1000);
        };
        return es;
      }

      function axisValue(sample){
        const a = state.axis;
        return a === "x" ? sample.x : (a === "y" ? sample.y : sample.z);
      }

      function render(){
        const plotN = Math.max(128, Math.min(20000, parseInt($("plotN").value || "2048", 10)));
        const fftN = parseInt($("fftN").value || "1024", 10);
        const win = $("win").value || "hann";

        const values = state.samples.slice(-plotN).map(axisValue);
        drawSeries($("time"), values);

        $("tstat").textContent = state.samples.length
          ? `samples_buffered=${state.samples.length} last_seq=${state.samples[state.samples.length-1].sample_seq}`
          : "waiting for samples...";

        if (state.outputOdrHz && values.length >= fftN && ((fftN & (fftN-1)) === 0)){
          const seg = values.slice(-fftN);
          const { freqs, psdDb } = computePsdDb(seg, state.outputOdrHz, win);
          const maxDb = 0;
          const minDb = -80;

          // Peak (ignore DC bin).
          let peakK = 1;
          let peakDb = psdDb.length > 1 ? psdDb[1] : -Infinity;
          for (let k=2;k<psdDb.length;k++){
            if (psdDb[k] > peakDb){ peakDb = psdDb[k]; peakK = k; }
          }
          const peakHz = freqs[peakK] || 0;

          // Plot relative-to-peak so the fixed 0..-80 scale is meaningful.
          const rel = new Float64Array(psdDb.length);
          for (let k=0;k<psdDb.length;k++){ rel[k] = psdDb[k] - peakDb; }
          drawPsd($("fft"), freqs, rel, minDb, maxDb);

          $("fstat").textContent =
            `fftN=${fftN} win=${win} peak≈${peakHz.toFixed(2)}Hz @ ${peakDb.toFixed(1)} dB/Hz (shown as relative 0..-80)`;
        } else {
          $("fstat").textContent = state.outputOdrHz ? "waiting for enough samples for FFT..." : "output_odr unknown";
        }

        requestAnimationFrame(render);
      }

      function boot(){
        const timeC = $("time"), fftC = $("fft");
        resizeCanvas(timeC); resizeCanvas(fftC);
        window.addEventListener("resize", () => { resizeCanvas(timeC); resizeCanvas(fftC); });

        $("axis").addEventListener("change", (e) => { state.axis = e.target.value; });
        $("node").addEventListener("change", (e) => {
          state.nodeId = parseInt(e.target.value, 10);
          state.samples = [];
          state.gaps = [];
          if (window._es) { window._es.close(); }
          window._es = connectStream();
        });
        loadMeta().then(() => { window._es = connectStream(); render(); });
      }

      boot();
    </script>
  </body>
</html>
"""


class LiveHandler(BaseHTTPRequestHandler):
    server_version = "sensor-system-live/0.1"

    def _send_json(self, payload: Any, status: int = 200) -> None:
        data = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def _send_html(self, html: str) -> None:
        data = html.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:  # noqa: N802
        parsed = urlparse(self.path)
        if parsed.path == "/":
            return self._send_html(INDEX_HTML)
        if parsed.path == "/api/meta":
            buf: LiveBuffer = self.server.live_buffer  # type: ignore[attr-defined]
            return self._send_json(buf.meta())
        if parsed.path == "/api/stream":
            qs = parse_qs(parsed.query)
            node_id = int(qs.get("node_id", ["1"])[0])
            return self._handle_stream(node_id)

        self.send_error(HTTPStatus.NOT_FOUND, "Not Found")

    def log_message(self, fmt: str, *args: object) -> None:
        # Keep this quiet by default (the recorder already logs).
        return

    def _handle_stream(self, node_id: int) -> None:
        buf: LiveBuffer = self.server.live_buffer  # type: ignore[attr-defined]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "keep-alive")
        self.end_headers()

        last_seq = -1
        # Send current meta immediately.
        try:
            meta = buf.meta()
            self.wfile.write(b"data: " + json.dumps({"type": "meta", **meta}).encode("utf-8") + b"\n\n")
            self.wfile.flush()
        except BrokenPipeError:
            return

        while True:
            try:
                last_seq = buf.wait_for_update(last_seq, timeout_s=1.0)
                samples, gaps, _ = buf.snapshot(node_id)
                # Send only a tail to avoid huge first burst.
                sample_tail = samples[-4096:]
                gap_tail = gaps[-64:]
                if sample_tail:
                    payload = {
                        "type": "samples",
                        "samples": [s.__dict__ for s in sample_tail],
                    }
                    self.wfile.write(b"data: " + json.dumps(payload).encode("utf-8") + b"\n\n")
                if gap_tail:
                    # Send gaps as individual events (rare).
                    for g in gap_tail[-8:]:
                        payload = {"type": "gap", "gap": g.__dict__}
                        self.wfile.write(b"data: " + json.dumps(payload).encode("utf-8") + b"\n\n")
                self.wfile.flush()
                time.sleep(0.05)
            except BrokenPipeError:
                return
            except ConnectionResetError:
                return


class LiveServer:
    def __init__(self, host: str, port: int, buffer: LiveBuffer) -> None:
        self.host = host
        self.port = port
        self.buffer = buffer
        self._httpd: Optional[ThreadingHTTPServer] = None
        self._thread: Optional[threading.Thread] = None

    def start(self) -> None:
        if self._httpd is not None:
            return
        httpd = ThreadingHTTPServer((self.host, self.port), LiveHandler)
        httpd.daemon_threads = True
        httpd.live_buffer = self.buffer  # type: ignore[attr-defined]
        self._httpd = httpd
        self._thread = threading.Thread(target=httpd.serve_forever, name="sensor-system-live", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        if self._httpd is None:
            return
        self._httpd.shutdown()
        self._httpd.server_close()
        self._httpd = None
