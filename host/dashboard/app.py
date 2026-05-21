from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from datetime import datetime, timezone
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse

from host.common.system_config import HostSystemConfig


DASHBOARD_VERSION = "0.1.0"
MAX_EVENT_LIMIT = 500
INDEX_HTML = """<!doctype html>
<html lang="pl">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Sensor System Host Panel</title>
    <style>
      :root {
        --bg: #f6f1e8;
        --bg-accent: #efe5d1;
        --panel: rgba(255, 252, 247, 0.88);
        --panel-strong: rgba(255, 249, 240, 0.96);
        --line: rgba(66, 47, 24, 0.12);
        --text: #25180d;
        --muted: #715843;
        --good: #1f7a54;
        --warn: #b46a18;
        --bad: #b53b31;
        --info: #2f5d80;
        --shadow: 0 18px 50px rgba(75, 50, 18, 0.12);
        --radius: 20px;
        --font-body: "Avenir Next", "Segoe UI", "Helvetica Neue", sans-serif;
        --font-display: "Iowan Old Style", "Palatino Linotype", "Book Antiqua", Georgia, serif;
      }

      * { box-sizing: border-box; }

      html { scroll-behavior: smooth; }

      body {
        margin: 0;
        min-height: 100vh;
        color: var(--text);
        font-family: var(--font-body);
        background:
          radial-gradient(circle at top left, rgba(239, 193, 110, 0.35), transparent 26rem),
          radial-gradient(circle at top right, rgba(77, 132, 173, 0.18), transparent 24rem),
          linear-gradient(180deg, var(--bg) 0%, #fbf8f2 48%, #f2ece2 100%);
      }

      body::before {
        content: "";
        position: fixed;
        inset: 0;
        pointer-events: none;
        background-image:
          linear-gradient(rgba(86, 63, 35, 0.03) 1px, transparent 1px),
          linear-gradient(90deg, rgba(86, 63, 35, 0.03) 1px, transparent 1px);
        background-size: 24px 24px;
        mask-image: linear-gradient(180deg, rgba(0, 0, 0, 0.28), transparent 90%);
      }

      .shell {
        width: min(1380px, calc(100vw - 28px));
        margin: 24px auto 40px;
      }

      .hero, .panel {
        backdrop-filter: blur(18px);
        border: 1px solid var(--line);
        box-shadow: var(--shadow);
      }

      .hero {
        position: relative;
        overflow: hidden;
        padding: 26px;
        border-radius: 28px;
        background:
          linear-gradient(140deg, rgba(255, 247, 228, 0.96), rgba(248, 244, 237, 0.88)),
          var(--panel-strong);
        animation: rise 480ms ease-out;
      }

      .hero::after {
        content: "";
        position: absolute;
        inset: auto -4rem -5rem auto;
        width: 18rem;
        height: 18rem;
        border-radius: 999px;
        background: radial-gradient(circle, rgba(191, 133, 52, 0.18), transparent 68%);
      }

      .hero-top {
        display: flex;
        justify-content: space-between;
        gap: 18px;
        align-items: flex-start;
      }

      .eyebrow {
        margin: 0 0 6px;
        letter-spacing: 0.14em;
        text-transform: uppercase;
        font-size: 11px;
        color: var(--muted);
      }

      h1 {
        margin: 0;
        font-family: var(--font-display);
        font-size: clamp(34px, 5vw, 56px);
        line-height: 0.98;
        max-width: 12ch;
      }

      .hero-copy {
        margin: 14px 0 0;
        max-width: 68ch;
        color: var(--muted);
        line-height: 1.55;
        font-size: 15px;
      }

      .actions {
        display: flex;
        gap: 10px;
        flex-wrap: wrap;
        margin-top: 16px;
      }

      .btn {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        gap: 8px;
        border: 1px solid rgba(63, 47, 27, 0.14);
        border-radius: 999px;
        padding: 11px 16px;
        font: inherit;
        background: rgba(255, 255, 255, 0.72);
        color: var(--text);
        cursor: pointer;
        transition: transform 160ms ease, background 160ms ease, border-color 160ms ease;
      }

      .btn:hover {
        transform: translateY(-1px);
        background: rgba(255, 255, 255, 0.92);
        border-color: rgba(63, 47, 27, 0.24);
      }

      .btn.secondary {
        background: rgba(245, 234, 217, 0.72);
      }

      .hero-meta {
        min-width: min(100%, 300px);
        display: grid;
        gap: 10px;
      }

      .meta-card {
        padding: 14px 16px;
        border-radius: 18px;
        background: rgba(255, 255, 255, 0.7);
        border: 1px solid rgba(69, 50, 26, 0.08);
      }

      .meta-card strong {
        display: block;
        font-size: 22px;
        line-height: 1.1;
        margin-top: 6px;
      }

      .chip-row {
        display: flex;
        gap: 8px;
        flex-wrap: wrap;
      }

      .chip {
        display: inline-flex;
        align-items: center;
        gap: 8px;
        padding: 7px 11px;
        border-radius: 999px;
        font-size: 12px;
        font-weight: 600;
        letter-spacing: 0.01em;
        border: 1px solid transparent;
      }

      .chip.good { color: var(--good); background: rgba(31, 122, 84, 0.10); border-color: rgba(31, 122, 84, 0.18); }
      .chip.warn { color: var(--warn); background: rgba(180, 106, 24, 0.11); border-color: rgba(180, 106, 24, 0.18); }
      .chip.bad { color: var(--bad); background: rgba(181, 59, 49, 0.10); border-color: rgba(181, 59, 49, 0.16); }
      .chip.info { color: var(--info); background: rgba(47, 93, 128, 0.11); border-color: rgba(47, 93, 128, 0.16); }
      .chip.muted { color: var(--muted); background: rgba(113, 88, 67, 0.08); border-color: rgba(113, 88, 67, 0.12); }

      .section {
        margin-top: 18px;
        animation: rise 620ms ease-out;
      }

      .section-head {
        display: flex;
        justify-content: space-between;
        align-items: baseline;
        gap: 16px;
        margin: 0 0 12px;
      }

      .section-title {
        margin: 0;
        font-size: 13px;
        letter-spacing: 0.12em;
        text-transform: uppercase;
        color: var(--muted);
      }

      .section-note {
        margin: 0;
        font-size: 13px;
        color: var(--muted);
      }

      .metrics {
        display: grid;
        grid-template-columns: repeat(4, minmax(0, 1fr));
        gap: 12px;
      }

      .metric {
        padding: 18px;
        border-radius: var(--radius);
        background: var(--panel);
      }

      .metric .label {
        font-size: 12px;
        color: var(--muted);
        text-transform: uppercase;
        letter-spacing: 0.12em;
      }

      .metric .value {
        margin-top: 10px;
        font-size: clamp(26px, 3vw, 38px);
        font-weight: 700;
        line-height: 1;
      }

      .metric .sub {
        margin-top: 8px;
        color: var(--muted);
        font-size: 13px;
      }

      .channels {
        display: grid;
        grid-template-columns: repeat(2, minmax(0, 1fr));
        gap: 12px;
      }

      .channel-card {
        padding: 18px;
        border-radius: 24px;
        background: var(--panel);
      }

      .channel-top,
      .runtime-grid,
      .config-grid {
        display: grid;
        gap: 10px;
      }

      .channel-top {
        grid-template-columns: minmax(0, 1fr) auto;
        align-items: start;
      }

      .channel-name {
        margin: 0;
        font-size: 26px;
        font-family: var(--font-display);
      }

      .channel-subtitle, .mono, .empty {
        color: var(--muted);
      }

      .channel-subtitle {
        margin-top: 6px;
        font-size: 14px;
      }

      .runtime-grid {
        grid-template-columns: repeat(4, minmax(0, 1fr));
        margin-top: 16px;
      }

      .runtime-card {
        padding: 14px;
        border-radius: 16px;
        background: rgba(255, 255, 255, 0.62);
        border: 1px solid rgba(69, 50, 26, 0.07);
      }

      .runtime-card .label {
        display: block;
        font-size: 11px;
        text-transform: uppercase;
        letter-spacing: 0.1em;
        color: var(--muted);
      }

      .runtime-card strong {
        display: block;
        margin-top: 7px;
        font-size: 18px;
        line-height: 1.2;
      }

      table {
        width: 100%;
        border-collapse: collapse;
        margin-top: 16px;
      }

      th, td {
        text-align: left;
        padding: 10px 8px;
        vertical-align: top;
        border-bottom: 1px solid rgba(69, 50, 26, 0.08);
        font-size: 13px;
      }

      th {
        color: var(--muted);
        font-size: 11px;
        text-transform: uppercase;
        letter-spacing: 0.1em;
      }

      .node-name {
        font-weight: 700;
      }

      .node-meta {
        margin-top: 4px;
        font-size: 12px;
        color: var(--muted);
      }

      .status-dot {
        display: inline-block;
        width: 8px;
        height: 8px;
        border-radius: 999px;
        margin-right: 7px;
        vertical-align: middle;
      }

      .status-dot.good { background: var(--good); }
      .status-dot.warn { background: var(--warn); }
      .status-dot.bad { background: var(--bad); }
      .status-dot.info { background: var(--info); }
      .status-dot.muted { background: #9c8a76; }

      .events {
        display: grid;
        grid-template-columns: 1.2fr 0.8fr;
        gap: 12px;
      }

      .event-panel,
      .config-panel {
        padding: 18px;
        border-radius: 24px;
        background: var(--panel);
      }

      .event-list {
        display: grid;
        gap: 10px;
      }

      .event-item {
        padding: 13px 14px;
        border-radius: 16px;
        background: rgba(255, 255, 255, 0.62);
        border: 1px solid rgba(69, 50, 26, 0.07);
      }

      .event-main {
        display: flex;
        justify-content: space-between;
        gap: 10px;
        align-items: baseline;
      }

      .event-title {
        font-weight: 700;
      }

      .event-meta {
        margin-top: 6px;
        color: var(--muted);
        font-size: 12px;
        line-height: 1.5;
      }

      .config-grid {
        grid-template-columns: repeat(2, minmax(0, 1fr));
      }

      .config-block {
        padding: 16px;
        border-radius: 18px;
        background: rgba(255, 255, 255, 0.62);
        border: 1px solid rgba(69, 50, 26, 0.07);
      }

      .config-block h3 {
        margin: 0 0 12px;
        font-size: 13px;
        text-transform: uppercase;
        letter-spacing: 0.1em;
        color: var(--muted);
      }

      .kv {
        display: grid;
        grid-template-columns: auto 1fr;
        gap: 7px 12px;
        font-size: 13px;
      }

      .kv dt {
        color: var(--muted);
      }

      .kv dd {
        margin: 0;
        word-break: break-word;
      }

      .api-list {
        display: flex;
        flex-wrap: wrap;
        gap: 8px;
        margin-top: 12px;
      }

      .api-list a {
        color: var(--text);
        text-decoration: none;
      }

      .mono {
        font-family: ui-monospace, "SFMono-Regular", Menlo, Consolas, monospace;
        font-size: 12px;
      }

      .empty {
        padding: 22px;
        border-radius: 18px;
        background: rgba(255, 255, 255, 0.46);
        border: 1px dashed rgba(69, 50, 26, 0.16);
      }

      .footer-note {
        margin-top: 18px;
        color: var(--muted);
        font-size: 13px;
        line-height: 1.5;
      }

      @keyframes rise {
        from { opacity: 0; transform: translateY(8px); }
        to { opacity: 1; transform: translateY(0); }
      }

      @media (max-width: 1120px) {
        .metrics,
        .channels,
        .events,
        .config-grid,
        .runtime-grid {
          grid-template-columns: 1fr 1fr;
        }
      }

      @media (max-width: 820px) {
        .shell {
          width: min(100vw - 18px, 100%);
          margin: 10px auto 24px;
        }

        .hero {
          padding: 20px;
          border-radius: 24px;
        }

        .hero-top,
        .channel-top {
          grid-template-columns: 1fr;
        }

        .metrics,
        .channels,
        .events,
        .config-grid,
        .runtime-grid {
          grid-template-columns: 1fr;
        }

        table, thead, tbody, tr, th, td {
          display: block;
        }

        thead { display: none; }

        tr {
          padding: 14px 0;
          border-bottom: 1px solid rgba(69, 50, 26, 0.08);
        }

        td {
          padding: 6px 0;
          border: 0;
        }

        td::before {
          content: attr(data-label);
          display: block;
          font-size: 10px;
          text-transform: uppercase;
          letter-spacing: 0.1em;
          color: var(--muted);
          margin-bottom: 2px;
        }
      }
    </style>
  </head>
  <body>
    <div class="shell">
      <section class="hero">
        <div class="hero-top">
          <div>
            <p class="eyebrow">Sensor System host dashboard</p>
            <h1 id="hero-title">Panel hosta</h1>
            <p class="hero-copy" id="hero-copy">
              Trwa ładowanie danych runtime, konfiguracji kanałów i ostatnich zdarzeń.
            </p>
            <div class="actions">
              <button class="btn" id="refresh-btn" type="button">Odśwież teraz</button>
              <a class="btn secondary" href="#channels">Przejdź do kanałów</a>
              <a class="btn secondary" href="#events">Ostatnie zdarzenia</a>
            </div>
          </div>
          <div class="hero-meta">
            <div class="meta-card">
              <div class="eyebrow">Stan hosta</div>
              <div class="chip-row" id="hero-chips"></div>
            </div>
            <div class="meta-card">
              <div class="eyebrow">Ostatni odczyt</div>
              <strong id="last-refresh">-</strong>
              <div class="section-note" id="last-runtime">Oczekiwanie na status supervisora</div>
            </div>
          </div>
        </div>
      </section>

      <section class="section">
        <div class="section-head">
          <h2 class="section-title">Przegląd</h2>
          <p class="section-note" id="overview-note">Pobieranie metryk</p>
        </div>
        <div class="metrics" id="metrics"></div>
      </section>

      <section class="section" id="channels">
        <div class="section-head">
          <h2 class="section-title">Kanały i węzły</h2>
          <p class="section-note">Stan pracy, procesy, pliki wyjściowe i telemetria węzłów</p>
        </div>
        <div class="channels" id="channels-grid"></div>
      </section>

      <section class="section events" id="events">
        <div class="event-panel panel">
          <div class="section-head">
            <h2 class="section-title">Zdarzenia</h2>
            <p class="section-note" id="events-note">Ostatnie wpisy z logu JSONL</p>
          </div>
          <div class="event-list" id="events-list"></div>
        </div>
        <div class="config-panel panel">
          <div class="section-head">
            <h2 class="section-title">Konfiguracja i API</h2>
            <p class="section-note">Punkt wyjścia pod późniejsze akcje administracyjne</p>
          </div>
          <div class="config-grid" id="config-grid"></div>
          <div class="api-list mono">
            <a class="chip muted" href="/api/dashboard" target="_blank" rel="noreferrer">/api/dashboard</a>
            <a class="chip muted" href="/api/overview" target="_blank" rel="noreferrer">/api/overview</a>
            <a class="chip muted" href="/api/channels" target="_blank" rel="noreferrer">/api/channels</a>
            <a class="chip muted" href="/api/events" target="_blank" rel="noreferrer">/api/events</a>
            <a class="chip muted" href="/api/config" target="_blank" rel="noreferrer">/api/config</a>
            <a class="chip muted" href="/api/health" target="_blank" rel="noreferrer">/api/health</a>
          </div>
          <p class="footer-note">
            Ta wersja panelu jest celowo read-only. Warstwa API i układ sekcji są gotowe pod kolejne kroki:
            commissioning, start/stop recordera, aktualizacje firmware i bardziej szczegółowy live-view.
          </p>
        </div>
      </section>
    </div>

    <script>
      const REFRESH_MS = 2000;
      let refreshTimer = null;

      function $(id) {
        return document.getElementById(id);
      }

      function escapeHtml(value) {
        return String(value ?? "")
          .replaceAll("&", "&amp;")
          .replaceAll("<", "&lt;")
          .replaceAll(">", "&gt;")
          .replaceAll('"', "&quot;")
          .replaceAll("'", "&#39;");
      }

      function formatNumber(value) {
        if (typeof value !== "number" || !Number.isFinite(value)) {
          return "-";
        }
        return new Intl.NumberFormat("pl-PL").format(value);
      }

      function formatFloat(value, digits = 2) {
        if (typeof value !== "number" || !Number.isFinite(value)) {
          return "-";
        }
        return new Intl.NumberFormat("pl-PL", {
          minimumFractionDigits: digits,
          maximumFractionDigits: digits,
        }).format(value);
      }

      function formatDate(value) {
        if (!value) {
          return "-";
        }
        const date = new Date(value);
        if (Number.isNaN(date.getTime())) {
          return escapeHtml(value);
        }
        return new Intl.DateTimeFormat("pl-PL", {
          dateStyle: "medium",
          timeStyle: "medium",
        }).format(date);
      }

      function formatUnixNs(value) {
        if (typeof value !== "number" || !Number.isFinite(value)) {
          return "-";
        }
        return formatDate(value / 1000000);
      }

      function chipClass(kind) {
        if (kind === "good") return "good";
        if (kind === "warn") return "warn";
        if (kind === "bad") return "bad";
        if (kind === "info") return "info";
        return "muted";
      }

      function chip(label, kind = "muted") {
        return `<span class="chip ${chipClass(kind)}">${escapeHtml(label)}</span>`;
      }

      function statusDot(kind) {
        return `<span class="status-dot ${chipClass(kind)}"></span>`;
      }

      function metricCard(label, value, sub) {
        return `
          <article class="metric panel">
            <div class="label">${escapeHtml(label)}</div>
            <div class="value">${escapeHtml(value)}</div>
            <div class="sub">${escapeHtml(sub)}</div>
          </article>
        `;
      }

      function renderHero(data) {
        const system = data.config.system || {};
        const overview = data.overview || {};
        const supervisor = data.supervisor || {};
        const systemLabel = [system.name, system.site].filter(Boolean).join(" / ");
        $("hero-title").textContent = systemLabel || "Panel hosta";

        const heroCopy = [];
        heroCopy.push(
          `Kanały aktywne: ${formatNumber(overview.channels_running || 0)} z ${formatNumber(overview.channels_enabled || 0)}.`
        );
        heroCopy.push(
          `Węzły online: ${formatNumber(overview.nodes_online || 0)} z ${formatNumber(overview.nodes_total || 0)}.`
        );
        if (supervisor.has_status) {
          heroCopy.push("Supervisor publikuje status runtime i zdarzenia.");
        } else {
          heroCopy.push("Konfiguracja jest dostępna, ale panel czeka jeszcze na pliki runtime z supervisora.");
        }
        $("hero-copy").textContent = heroCopy.join(" ");

        const chips = [];
        chips.push(chip(supervisor.has_status ? "status runtime obecny" : "brak statusu runtime", supervisor.has_status ? "good" : "warn"));
        chips.push(chip(`alerty: ${formatNumber(overview.attention_count || 0)}`, (overview.attention_count || 0) > 0 ? "warn" : "good"));
        chips.push(chip(`błędy: ${formatNumber((overview.events_by_severity || {}).error || 0)}`, ((overview.events_by_severity || {}).error || 0) > 0 ? "bad" : "muted"));
        chips.push(chip(`warn: ${formatNumber((overview.events_by_severity || {}).warning || 0)}`, ((overview.events_by_severity || {}).warning || 0) > 0 ? "warn" : "muted"));
        $("hero-chips").innerHTML = chips.join("");

        $("last-refresh").textContent = formatDate(data.generated_utc);
        $("last-runtime").textContent = supervisor.updated_utc
          ? `Ostatni update runtime: ${formatDate(supervisor.updated_utc)}`
          : "Supervisor jeszcze nie zapisał statusu";
      }

      function renderOverview(data) {
        const overview = data.overview || {};
        $("overview-note").textContent = overview.status_summary || "Brak danych";
        $("metrics").innerHTML = [
          metricCard(
            "Kanały",
            `${formatNumber(overview.channels_running || 0)} / ${formatNumber(overview.channels_enabled || 0)}`,
            `włączone ${formatNumber(overview.channels_total || 0)}`
          ),
          metricCard(
            "Węzły online",
            `${formatNumber(overview.nodes_online || 0)} / ${formatNumber(overview.nodes_total || 0)}`,
            `aktywnych ${formatNumber(overview.nodes_enabled || 0)}`
          ),
          metricCard(
            "Próbki zapisane",
            formatNumber(overview.samples_written_total || 0),
            `wykryte luki ${formatNumber(overview.gaps_detected_total || 0)}`
          ),
          metricCard(
            "Restarty",
            formatNumber(overview.restart_count_total || 0),
            `uwag ${formatNumber(overview.attention_count || 0)}`
          ),
        ].join("");
      }

      function renderChannel(channel) {
        const healthKind = chipClass(channel.health || "muted");
        const stateLabel = channel.enabled
          ? (channel.running ? "RUNNING" : "STOPPED")
          : "DISABLED";
        const runtimeCards = [
          { label: "Port", value: `${channel.port} @ ${channel.baud}` },
          { label: "Proces", value: channel.process_id ? `pid ${channel.process_id}` : "brak" },
          { label: "Plik", value: channel.active_file || "-" },
          { label: "Restarty", value: formatNumber(channel.restart_count || 0) },
        ];
        const nodesHtml = channel.nodes.length
          ? `
            <table>
              <thead>
                <tr>
                  <th>Węzeł</th>
                  <th>Stan</th>
                  <th>ODR</th>
                  <th>Próbki</th>
                  <th>Gaps / Overflow</th>
                  <th>Temperatura</th>
                </tr>
              </thead>
              <tbody>
                ${channel.nodes.map(renderNodeRow).join("")}
              </tbody>
            </table>
          `
          : `<div class="empty">Kanał nie ma jeszcze zdefiniowanych węzłów.</div>`;

        return `
          <article class="channel-card panel">
            <div class="channel-top">
              <div>
                <h3 class="channel-name">${escapeHtml(channel.label || channel.name)}</h3>
                <div class="channel-subtitle">
                  ${escapeHtml(channel.name)}
                  · ${escapeHtml(channel.destination || "-")}
                  · status ${escapeHtml(formatDate(channel.updated_utc))}
                </div>
              </div>
              <div class="chip-row">
                ${chip(stateLabel, channel.enabled ? (channel.running ? "good" : "warn") : "muted")}
                ${chip(channel.health || "unknown", healthKind)}
                ${chip(`alerty ${formatNumber(channel.attention_count || 0)}`, (channel.attention_count || 0) > 0 ? "warn" : "muted")}
              </div>
            </div>
            <div class="runtime-grid">
              ${runtimeCards.map((card) => `
                <div class="runtime-card">
                  <span class="label">${escapeHtml(card.label)}</span>
                  <strong class="mono">${escapeHtml(card.value)}</strong>
                </div>
              `).join("")}
            </div>
            ${nodesHtml}
          </article>
        `;
      }

      function renderNodeRow(node) {
        const onlineKind = node.online ? "good" : (node.has_runtime ? "bad" : "muted");
        const nodeTitle = node.name ? `${node.name}` : `Node ${node.node_id}`;
        const alerts = Array.isArray(node.alerts) && node.alerts.length
          ? node.alerts.join(", ")
          : "brak";
        return `
          <tr>
            <td data-label="Węzeł">
              <div class="node-name">${escapeHtml(nodeTitle)}</div>
              <div class="node-meta mono">id=${escapeHtml(node.node_id)} · oczekiwany ODR ${escapeHtml(node.expected_odr_hz ?? "-")} Hz</div>
            </td>
            <td data-label="Stan">
              ${statusDot(onlineKind)}
              ${escapeHtml(node.online ? "ONLINE" : (node.has_runtime ? "OFFLINE" : "NO-RUNTIME"))}
              <div class="node-meta">${escapeHtml(alerts)}</div>
            </td>
            <td data-label="ODR">
              <span class="mono">${escapeHtml(formatNumber(node.sensor_odr_hz || 0))} / ${escapeHtml(formatFloat(node.output_odr_hz || 0, 1))}</span>
              <div class="node-meta">sensor / output</div>
            </td>
            <td data-label="Próbki">
              <span class="mono">${escapeHtml(formatNumber(node.samples_written || 0))}</span>
              <div class="node-meta">next ${escapeHtml(formatNumber(node.expected_sample_seq || 0))}</div>
            </td>
            <td data-label="Gaps / Overflow">
              <span class="mono">gaps ${escapeHtml(formatNumber(node.gaps_detected || 0))}</span>
              <div class="node-meta">rx ${escapeHtml(formatNumber(node.rx_overflow_session || 0))} · pkt ${escapeHtml(formatNumber(node.packet_overwrite_session || 0))}</div>
            </td>
            <td data-label="Temperatura">
              <span class="mono">${escapeHtml(node.last_temperature_c == null ? "-" : `${formatFloat(node.last_temperature_c, 2)} C`)}</span>
              <div class="node-meta">${escapeHtml(formatUnixNs(node.last_temperature_unix_ns))}</div>
            </td>
          </tr>
        `;
      }

      function renderChannels(data) {
        const channels = data.channels || [];
        $("channels-grid").innerHTML = channels.length
          ? channels.map(renderChannel).join("")
          : `<div class="empty">Brak skonfigurowanych kanałów.</div>`;
      }

      function renderEvents(data) {
        const events = data.events || [];
        $("events-note").textContent = events.length
          ? `Wyświetlane wpisy: ${formatNumber(events.length)}`
          : "Brak zdarzeń do pokazania";
        $("events-list").innerHTML = events.length
          ? events.map((event) => {
              const severity = event.severity || "info";
              const locationBits = [];
              if (event.channel_name) locationBits.push(`kanał ${event.channel_name}`);
              if (event.node_id != null) locationBits.push(`node ${event.node_id}`);
              const details = Object.entries(event)
                .filter(([key]) => !["utc", "severity", "event", "channel_name", "node_id"].includes(key))
                .slice(0, 4)
                .map(([key, value]) => `${key}=${value}`)
                .join(" · ");
              return `
                <article class="event-item">
                  <div class="event-main">
                    <div class="event-title">${escapeHtml(event.event || "event")}</div>
                    ${chip(severity, severity === "error" ? "bad" : (severity === "warning" ? "warn" : "info"))}
                  </div>
                  <div class="event-meta">
                    ${escapeHtml(formatDate(event.utc))}
                    ${locationBits.length ? ` · ${escapeHtml(locationBits.join(" · "))}` : ""}
                    ${details ? `<br>${escapeHtml(details)}` : ""}
                  </div>
                </article>
              `;
            }).join("")
          : `<div class="empty">Log zdarzeń jest pusty albo supervisor jeszcze go nie zapisał.</div>`;
      }

      function renderConfig(data) {
        const config = data.config || {};
        const supervisor = data.supervisor || {};
        const system = config.system || {};
        const storage = config.storage || {};
        const runtime = config.supervisor || {};
        $("config-grid").innerHTML = [
          {
            title: "System",
            rows: [
              ["nazwa", system.name || "-"],
              ["lokalizacja", system.site || "-"],
              ["strefa", system.timezone || "-"],
              ["plik config", config.path || "-"],
            ],
          },
          {
            title: "Storage",
            rows: [
              ["root_dir", storage.root_dir || "-"],
              ["format", storage.format || "-"],
              ["compression", storage.compression || "-"],
              ["window_seconds", storage.window_seconds ?? "-"],
            ],
          },
          {
            title: "Supervisor runtime",
            rows: [
              ["status_file", runtime.status_file || "-"],
              ["event_log", runtime.event_log || "-"],
              ["channel_runtime_dir", runtime.channel_runtime_dir || "-"],
              ["started_utc", supervisor.started_utc || "-"],
            ],
          },
          {
            title: "Szkielet API",
            rows: [
              ["dashboard", "overview + channels + events"],
              ["channels", "stan kanałów i węzłów"],
              ["events", "tail JSONL z limitem"],
              ["health", `wersja ${data.dashboard_version || "-"}`],
            ],
          },
        ].map((block) => `
          <div class="config-block">
            <h3>${escapeHtml(block.title)}</h3>
            <dl class="kv">
              ${block.rows.map(([key, value]) => `
                <dt>${escapeHtml(key)}</dt>
                <dd class="mono">${escapeHtml(value)}</dd>
              `).join("")}
            </dl>
          </div>
        `).join("");
      }

      async function loadDashboard() {
        try {
          const response = await fetch(`/api/dashboard?limit=40`, { cache: "no-store" });
          if (!response.ok) {
            throw new Error(`HTTP ${response.status}`);
          }
          const data = await response.json();
          renderHero(data);
          renderOverview(data);
          renderChannels(data);
          renderEvents(data);
          renderConfig(data);
        } catch (error) {
          $("hero-copy").textContent = `Nie udało się pobrać danych panelu: ${error}`;
          $("overview-note").textContent = "Błąd odczytu";
          $("metrics").innerHTML = metricCard("Status", "Błąd", "Sprawdź proces dashboardu i pliki runtime");
          $("channels-grid").innerHTML = `<div class="empty">Brak danych kanałów.</div>`;
          $("events-list").innerHTML = `<div class="empty">Brak danych zdarzeń.</div>`;
          $("config-grid").innerHTML = `<div class="empty">Brak danych konfiguracji.</div>`;
        }
      }

      function scheduleRefresh() {
        if (refreshTimer !== null) {
          window.clearInterval(refreshTimer);
        }
        refreshTimer = window.setInterval(loadDashboard, REFRESH_MS);
      }

      $("refresh-btn").addEventListener("click", () => {
        loadDashboard();
        scheduleRefresh();
      });

      loadDashboard();
      scheduleRefresh();
    </script>
  </body>
</html>
"""


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def load_json(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def load_tail_jsonl(path: Path, limit: int) -> list[dict[str, Any]]:
    if not path.exists() or limit <= 0:
        return []
    try:
        with path.open("rb") as handle:
            handle.seek(0, 2)
            pos = handle.tell()
            buffer = b""
            lines: list[bytes] = []
            while pos > 0 and len(lines) <= limit:
                read_size = min(4096, pos)
                pos -= read_size
                handle.seek(pos)
                buffer = handle.read(read_size) + buffer
                lines = buffer.splitlines()
    except OSError:
        return []

    events: list[dict[str, Any]] = []
    for line in lines[-limit:]:
        if not line.strip():
            continue
        try:
            events.append(json.loads(line.decode("utf-8")))
        except json.JSONDecodeError:
            continue
    return events


def clamp_limit(raw_value: str | None, default: int) -> int:
    try:
        parsed = int(raw_value) if raw_value is not None else default
    except ValueError:
        parsed = default
    return max(1, min(MAX_EVENT_LIMIT, parsed))


def parse_iso8601(value: str | None) -> datetime | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def ns_to_utc_iso(ns_value: int | None) -> str | None:
    if ns_value is None:
        return None
    try:
        return datetime.fromtimestamp(ns_value / 1_000_000_000, tz=timezone.utc).isoformat()
    except (OverflowError, OSError, ValueError):
        return None


def event_severity_counts(events: list[dict[str, Any]]) -> dict[str, int]:
    counts = {"info": 0, "warning": 0, "error": 0}
    for event in events:
        severity = str(event.get("severity", "info")).lower()
        if severity not in counts:
            counts[severity] = 0
        counts[severity] += 1
    return counts


class DashboardRepository:
    def __init__(self, config_path: str | Path, default_event_limit: int = 40) -> None:
        self.config_path = Path(config_path)
        self.default_event_limit = default_event_limit

    def _system_config(self) -> HostSystemConfig:
        return HostSystemConfig.load(self.config_path)

    def config_payload(self) -> dict[str, Any]:
        config = self._system_config()
        payload = asdict(config)
        payload["path"] = str(self.config_path)
        return payload

    def events_payload(self, limit: int | None = None) -> list[dict[str, Any]]:
        config = self._system_config()
        limit_value = clamp_limit(str(limit) if limit is not None else None, self.default_event_limit)
        return load_tail_jsonl(Path(config.supervisor.event_log), limit_value)

    def dashboard_payload(self, limit: int | None = None) -> dict[str, Any]:
        config = self._system_config()
        limit_value = clamp_limit(str(limit) if limit is not None else None, self.default_event_limit)
        raw_status = load_json(Path(config.supervisor.status_file))
        events = load_tail_jsonl(Path(config.supervisor.event_log), limit_value)
        channels = self._build_channels(config, raw_status)
        overview = self._build_overview(channels, events, raw_status)
        supervisor = {
            "has_status": raw_status is not None,
            "status_file": config.supervisor.status_file,
            "event_log": config.supervisor.event_log,
            "updated_utc": raw_status.get("updated_utc") if raw_status else None,
            "started_utc": raw_status.get("started_utc") if raw_status else None,
            "supervisor_version": raw_status.get("supervisor_version") if raw_status else None,
            "storage_root": raw_status.get("storage_root", config.storage.root_dir) if raw_status else config.storage.root_dir,
        }
        return {
            "dashboard_version": DASHBOARD_VERSION,
            "generated_utc": utc_now_iso(),
            "config": self.config_payload(),
            "supervisor": supervisor,
            "overview": overview,
            "channels": channels,
            "events": events,
        }

    def health_payload(self) -> dict[str, Any]:
        dashboard = self.dashboard_payload(limit=10)
        overview = dashboard["overview"]
        supervisor = dashboard["supervisor"]
        return {
            "ok": True,
            "dashboard_version": DASHBOARD_VERSION,
            "generated_utc": dashboard["generated_utc"],
            "has_status": supervisor["has_status"],
            "channels_running": overview["channels_running"],
            "nodes_online": overview["nodes_online"],
            "attention_count": overview["attention_count"],
        }

    def _build_channels(
        self,
        config: HostSystemConfig,
        raw_status: dict[str, Any] | None,
    ) -> list[dict[str, Any]]:
        runtime_channels: dict[str, dict[str, Any]] = {}
        for raw_channel in raw_status.get("channels", []) if raw_status else []:
            runtime_channels[str(raw_channel.get("name"))] = raw_channel

        channels: list[dict[str, Any]] = []
        for config_channel in config.channels:
            runtime_channel = runtime_channels.pop(config_channel.name, None)
            channels.append(self._merge_channel(config_channel, runtime_channel, config))

        for runtime_channel in runtime_channels.values():
            channels.append(self._runtime_only_channel(runtime_channel))

        return channels

    def _merge_channel(
        self,
        config_channel: Any,
        runtime_channel: dict[str, Any] | None,
        config: HostSystemConfig,
    ) -> dict[str, Any]:
        runtime_nodes = {
            int(node.get("node_id", 0)): node
            for node in runtime_channel.get("nodes", [])
        } if runtime_channel else {}

        nodes = [
            self._merge_node(config_node, runtime_nodes.pop(config_node.node_id, None))
            for config_node in config_channel.nodes
        ]
        for raw_node in runtime_nodes.values():
            nodes.append(self._runtime_only_node(raw_node))

        attention_count = sum(1 for node in nodes if node["alerts"])
        running = bool(runtime_channel.get("running")) if runtime_channel else False
        enabled = bool(config_channel.enabled)
        online_nodes = sum(1 for node in nodes if node["online"])
        health = "disabled"
        if enabled:
            if not runtime_channel:
                health = "waiting"
            elif running and attention_count == 0 and online_nodes == len(nodes):
                health = "healthy"
            elif running:
                health = "degraded"
            else:
                health = "stopped"

        return {
            "name": config_channel.name,
            "label": config_channel.label,
            "configured": True,
            "enabled": enabled,
            "running": running,
            "health": health,
            "attention_count": attention_count,
            "port": config_channel.port,
            "baud": config_channel.baud,
            "process_id": runtime_channel.get("process_id") if runtime_channel else None,
            "restart_count": int(runtime_channel.get("restart_count", 0)) if runtime_channel else 0,
            "last_exit_code": runtime_channel.get("last_exit_code") if runtime_channel else None,
            "updated_utc": runtime_channel.get("updated_utc") if runtime_channel else None,
            "destination": runtime_channel.get("destination", config.storage.root_dir) if runtime_channel else config.storage.root_dir,
            "active_file": runtime_channel.get("active_file") if runtime_channel else None,
            "status_file": runtime_channel.get("status_file") if runtime_channel else str(
                Path(config.supervisor.channel_runtime_dir) / f"{config_channel.name}.status.json"
            ),
            "event_log": runtime_channel.get("event_log") if runtime_channel else str(
                Path(config.supervisor.channel_runtime_dir) / f"{config_channel.name}.events.jsonl"
            ),
            "nodes": nodes,
        }

    def _runtime_only_channel(self, runtime_channel: dict[str, Any]) -> dict[str, Any]:
        nodes = [self._runtime_only_node(raw_node) for raw_node in runtime_channel.get("nodes", [])]
        attention_count = sum(1 for node in nodes if node["alerts"])
        return {
            "name": str(runtime_channel.get("name", "unknown")),
            "label": runtime_channel.get("label"),
            "configured": False,
            "enabled": bool(runtime_channel.get("enabled", True)),
            "running": bool(runtime_channel.get("running", False)),
            "health": "runtime-only",
            "attention_count": attention_count,
            "port": str(runtime_channel.get("port", "-")),
            "baud": int(runtime_channel.get("baud", 0)),
            "process_id": runtime_channel.get("process_id"),
            "restart_count": int(runtime_channel.get("restart_count", 0)),
            "last_exit_code": runtime_channel.get("last_exit_code"),
            "updated_utc": runtime_channel.get("updated_utc"),
            "destination": runtime_channel.get("destination"),
            "active_file": runtime_channel.get("active_file"),
            "status_file": runtime_channel.get("status_file"),
            "event_log": runtime_channel.get("event_log"),
            "nodes": nodes,
        }

    def _merge_node(self, config_node: Any, runtime_node: dict[str, Any] | None) -> dict[str, Any]:
        name = config_node.name or (str(runtime_node.get("name")) if runtime_node and runtime_node.get("name") else None)
        online = bool(runtime_node.get("online", False)) if runtime_node else False
        sensor_odr_hz = int(runtime_node.get("sensor_odr_hz", 0)) if runtime_node else 0
        output_odr_hz = float(runtime_node.get("output_odr_hz", 0.0)) if runtime_node else 0.0
        alerts: list[str] = []
        if config_node.enabled and runtime_node is None:
            alerts.append("brak runtime")
        elif config_node.enabled and not online:
            alerts.append("offline")
        if (
            config_node.expected_odr_hz is not None
            and output_odr_hz > 0
            and abs(output_odr_hz - config_node.expected_odr_hz) > 1e-6
        ):
            alerts.append("odr mismatch")

        return {
            "node_id": config_node.node_id,
            "name": name,
            "configured": True,
            "enabled": bool(config_node.enabled),
            "expected_odr_hz": config_node.expected_odr_hz,
            "has_runtime": runtime_node is not None,
            "online": online,
            "sensor_odr_hz": sensor_odr_hz,
            "output_odr_hz": output_odr_hz,
            "samples_written": int(runtime_node.get("samples_written", 0)) if runtime_node else 0,
            "expected_sample_seq": int(runtime_node.get("expected_sample_seq", 0)) if runtime_node else 0,
            "last_written_seq": int(runtime_node.get("last_written_seq", 0)) if runtime_node else 0,
            "bursts_ok": int(runtime_node.get("bursts_ok", 0)) if runtime_node else 0,
            "bursts_no_data": int(runtime_node.get("bursts_no_data", 0)) if runtime_node else 0,
            "bursts_failed": int(runtime_node.get("bursts_failed", 0)) if runtime_node else 0,
            "gaps_detected": int(runtime_node.get("gaps_detected", 0)) if runtime_node else 0,
            "empty_polls": int(runtime_node.get("empty_polls", 0)) if runtime_node else 0,
            "sensor_loss_total": int(runtime_node.get("sensor_loss_total", 0)) if runtime_node else 0,
            "sensor_loss_session": int(runtime_node.get("sensor_loss_session", 0)) if runtime_node else 0,
            "rx_overflow_total": int(runtime_node.get("rx_overflow_total", 0)) if runtime_node else 0,
            "rx_overflow_session": int(runtime_node.get("rx_overflow_session", 0)) if runtime_node else 0,
            "packet_overwrite_total": int(runtime_node.get("packet_overwrite_total", 0)) if runtime_node else 0,
            "packet_overwrite_session": int(runtime_node.get("packet_overwrite_session", 0)) if runtime_node else 0,
            "last_temperature_c": runtime_node.get("last_temperature_c") if runtime_node else None,
            "last_temperature_unix_ns": runtime_node.get("last_temperature_unix_ns") if runtime_node else None,
            "last_temperature_utc": ns_to_utc_iso(runtime_node.get("last_temperature_unix_ns")) if runtime_node else None,
            "alerts": alerts,
        }

    def _runtime_only_node(self, runtime_node: dict[str, Any]) -> dict[str, Any]:
        alerts: list[str] = []
        if not runtime_node.get("online", False):
            alerts.append("offline")
        return {
            "node_id": int(runtime_node.get("node_id", 0)),
            "name": runtime_node.get("name"),
            "configured": False,
            "enabled": bool(runtime_node.get("enabled", True)),
            "expected_odr_hz": None,
            "has_runtime": True,
            "online": bool(runtime_node.get("online", False)),
            "sensor_odr_hz": int(runtime_node.get("sensor_odr_hz", 0)),
            "output_odr_hz": float(runtime_node.get("output_odr_hz", 0.0)),
            "samples_written": int(runtime_node.get("samples_written", 0)),
            "expected_sample_seq": int(runtime_node.get("expected_sample_seq", 0)),
            "last_written_seq": int(runtime_node.get("last_written_seq", 0)),
            "bursts_ok": int(runtime_node.get("bursts_ok", 0)),
            "bursts_no_data": int(runtime_node.get("bursts_no_data", 0)),
            "bursts_failed": int(runtime_node.get("bursts_failed", 0)),
            "gaps_detected": int(runtime_node.get("gaps_detected", 0)),
            "empty_polls": int(runtime_node.get("empty_polls", 0)),
            "sensor_loss_total": int(runtime_node.get("sensor_loss_total", 0)),
            "sensor_loss_session": int(runtime_node.get("sensor_loss_session", 0)),
            "rx_overflow_total": int(runtime_node.get("rx_overflow_total", 0)),
            "rx_overflow_session": int(runtime_node.get("rx_overflow_session", 0)),
            "packet_overwrite_total": int(runtime_node.get("packet_overwrite_total", 0)),
            "packet_overwrite_session": int(runtime_node.get("packet_overwrite_session", 0)),
            "last_temperature_c": runtime_node.get("last_temperature_c"),
            "last_temperature_unix_ns": runtime_node.get("last_temperature_unix_ns"),
            "last_temperature_utc": ns_to_utc_iso(runtime_node.get("last_temperature_unix_ns")),
            "alerts": alerts,
        }

    def _build_overview(
        self,
        channels: list[dict[str, Any]],
        events: list[dict[str, Any]],
        raw_status: dict[str, Any] | None,
    ) -> dict[str, Any]:
        channels_total = len(channels)
        channels_enabled = sum(1 for channel in channels if channel["enabled"])
        channels_running = sum(1 for channel in channels if channel["running"])
        nodes = [node for channel in channels for node in channel["nodes"]]
        nodes_total = len(nodes)
        nodes_enabled = sum(1 for node in nodes if node["enabled"])
        nodes_online = sum(1 for node in nodes if node["online"])
        samples_written_total = sum(node["samples_written"] for node in nodes)
        gaps_detected_total = sum(node["gaps_detected"] for node in nodes)
        restart_count_total = sum(channel["restart_count"] for channel in channels)
        attention_count = sum(len(node["alerts"]) for node in nodes)
        severity_counts = event_severity_counts(events)
        updated = parse_iso8601(raw_status.get("updated_utc")) if raw_status else None
        age_seconds = None
        if updated is not None:
            age_seconds = max(0.0, (datetime.now(timezone.utc) - updated).total_seconds())

        if raw_status is None:
            summary = "Konfiguracja dostępna, oczekiwanie na runtime z supervisora"
        elif channels_running == channels_enabled and nodes_online == nodes_enabled:
            summary = "Wszystkie aktywne kanały i węzły są online"
        elif channels_running == 0:
            summary = "Supervisor działa, ale żaden kanał nie raportuje pracy"
        else:
            summary = "Część kanałów lub węzłów wymaga uwagi"

        return {
            "channels_total": channels_total,
            "channels_enabled": channels_enabled,
            "channels_running": channels_running,
            "nodes_total": nodes_total,
            "nodes_enabled": nodes_enabled,
            "nodes_online": nodes_online,
            "samples_written_total": samples_written_total,
            "gaps_detected_total": gaps_detected_total,
            "restart_count_total": restart_count_total,
            "attention_count": attention_count,
            "events_by_severity": severity_counts,
            "status_age_s": age_seconds,
            "status_summary": summary,
        }


class DashboardServer(ThreadingHTTPServer):
    def __init__(self, server_address: tuple[str, int], repository: DashboardRepository) -> None:
        super().__init__(server_address, DashboardRequestHandler)
        self.repository = repository


class DashboardRequestHandler(BaseHTTPRequestHandler):
    server_version = f"SensorSystemDashboard/{DASHBOARD_VERSION}"

    @property
    def repository(self) -> DashboardRepository:
        return self.server.repository  # type: ignore[attr-defined]

    def log_message(self, format: str, *args: object) -> None:
        return None

    def do_GET(self) -> None:
        parsed = urlparse(self.path)
        route = parsed.path
        query = parse_qs(parsed.query)

        if route == "/":
            self._write_response(INDEX_HTML.encode("utf-8"), "text/html; charset=utf-8")
            return

        if route == "/api/dashboard":
            limit = clamp_limit(query.get("limit", [None])[0], self.repository.default_event_limit)
            self._write_json(self.repository.dashboard_payload(limit=limit))
            return

        if route == "/api/overview":
            limit = clamp_limit(query.get("limit", [None])[0], self.repository.default_event_limit)
            payload = self.repository.dashboard_payload(limit=limit)["overview"]
            self._write_json(payload)
            return

        if route == "/api/channels":
            limit = clamp_limit(query.get("limit", [None])[0], self.repository.default_event_limit)
            payload = self.repository.dashboard_payload(limit=limit)["channels"]
            self._write_json(payload)
            return

        if route == "/api/events":
            limit = clamp_limit(query.get("limit", [None])[0], self.repository.default_event_limit)
            self._write_json(self.repository.events_payload(limit=limit))
            return

        if route == "/api/config":
            self._write_json(self.repository.config_payload())
            return

        if route == "/api/health":
            self._write_json(self.repository.health_payload())
            return

        self._write_json({"error": "not found", "path": route}, status=HTTPStatus.NOT_FOUND)

    def _write_json(self, payload: Any, status: HTTPStatus = HTTPStatus.OK) -> None:
        encoded = json.dumps(payload, indent=2, sort_keys=True).encode("utf-8")
        self._write_response(encoded, "application/json; charset=utf-8", status=status)

    def _write_response(
        self,
        payload: bytes,
        content_type: str,
        *,
        status: HTTPStatus = HTTPStatus.OK,
    ) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(payload)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Serve a web dashboard for the Sensor System host runtime")
    parser.add_argument("--config", default="host/system_config.json")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8080)
    parser.add_argument("--event-limit", type=int, default=40)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    repository = DashboardRepository(args.config, default_event_limit=args.event_limit)
    server = DashboardServer((args.host, args.port), repository)
    print(f"[dashboard] serving on http://{args.host}:{args.port}/ using {args.config}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("[dashboard] stopping")
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
