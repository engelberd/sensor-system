from __future__ import annotations

import argparse
import json
import os
import secrets
import subprocess
import sys
import threading
import time
from dataclasses import asdict
from datetime import datetime, timedelta, timezone
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, unquote, urlparse

from host.common.data_browser import DataRepository, FileDownload
from host.common.runtime_status import JsonlEventWriter
from host.common.system_config import HostSystemConfig
from host.common.version import PROJECT_VERSION


DASHBOARD_VERSION = PROJECT_VERSION
MAX_EVENT_LIMIT = 500
DEFAULT_LIVE_PREVIEW_LIMIT = 512
MAX_LIVE_PREVIEW_LIMIT = 2048
LIVE_PREVIEW_LEASE_TIMEOUT_S = 20.0
STANDARD_GRAVITY_M_S2 = 9.80665
LIVE_PREVIEW_CHART_MAX_POINTS = 2400
LIVE_PREVIEW_FILE_CACHE_TTL_S = 10.0
ALERT_EVENT_NAMES = {
    "channel_exited",
    "gap_detected",
    "runtime_error",
    "runtime_warning",
    "serial_error",
    "temperature_read_failed",
    "node_firmware_restarted",
}
HIDDEN_DASHBOARD_EVENT_NAMES = {
    "temperature_sampled",
}
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

      .btn.small {
        padding: 8px 12px;
        font-size: 12px;
      }

      .node-firmware-restart-btn {
        margin-top: 8px;
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
        overflow: hidden;
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

      .channel-actions {
        display: flex;
        flex-wrap: wrap;
        gap: 8px;
        margin-top: 12px;
      }

      .runtime-grid {
        grid-template-columns:
          minmax(0, 1.05fr)
          minmax(0, 1.9fr)
          minmax(0, 0.9fr)
          minmax(0, 0.65fr);
        margin-top: 16px;
        align-items: stretch;
      }

      .runtime-card {
        padding: 14px;
        border-radius: 16px;
        background: rgba(255, 255, 255, 0.62);
        border: 1px solid rgba(69, 50, 26, 0.07);
        min-width: 0;
        overflow: hidden;
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
        font-size: clamp(15px, 1.2vw, 17px);
        line-height: 1.28;
        color: var(--text);
        overflow-wrap: anywhere;
        word-break: break-word;
      }

      .runtime-card.runtime-card-file strong {
        font-size: clamp(14px, 1.25vw, 16px);
      }

      .runtime-card.runtime-card-rate strong,
      .runtime-card.runtime-card-restarts strong {
        font-size: clamp(20px, 1.9vw, 28px);
        line-height: 1.05;
      }

      .runtime-card .node-meta {
        margin-top: 8px;
        line-height: 1.45;
        overflow-wrap: anywhere;
        word-break: break-word;
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
        line-height: 1.45;
      }

      .node-actions {
        margin-top: 8px;
        display: flex;
        flex-wrap: wrap;
        gap: 8px;
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

      .logs-grid {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 12px;
      }

      .event-panel,
      .config-panel,
      .logs-panel {
        padding: 18px;
        border-radius: 24px;
        background: var(--panel);
      }

      .event-list,
      .log-list,
      .log-channel-list {
        display: grid;
        gap: 10px;
      }

      .event-item,
      .log-channel-card {
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

      .logs-toolbar {
        display: flex;
        flex-wrap: wrap;
        gap: 10px;
        align-items: end;
        margin-bottom: 12px;
      }

      .field {
        display: grid;
        gap: 6px;
        font-size: 13px;
        color: var(--muted);
      }

      .field input,
      .field select {
        width: 100%;
        border-radius: 12px;
        border: 1px solid rgba(69, 50, 26, 0.12);
        background: rgba(255, 255, 255, 0.9);
        padding: 11px 12px;
        font: inherit;
        color: var(--text);
      }

      .log-subtitle {
        margin: 10px 0 6px;
        font-size: 11px;
        text-transform: uppercase;
        letter-spacing: 0.1em;
        color: var(--muted);
      }

      .log-lines {
        display: grid;
        gap: 6px;
      }

      .log-line {
        padding: 10px 12px;
        border-radius: 12px;
        background: rgba(255, 255, 255, 0.54);
        border: 1px solid rgba(69, 50, 26, 0.06);
        white-space: pre-wrap;
        word-break: break-word;
      }

      .config-grid {
        grid-template-columns: repeat(2, minmax(0, 1fr));
      }

      .data-layout {
        display: grid;
        grid-template-columns: 1.1fr 0.9fr;
        gap: 12px;
      }

      .data-panel {
        padding: 18px;
        border-radius: 24px;
        background: var(--panel);
      }

      .data-controls,
      .data-actions,
      .data-summary-grid {
        display: grid;
        gap: 10px;
      }

      .data-controls {
        grid-template-columns: minmax(0, 1fr) auto auto;
        align-items: end;
      }

      .data-actions {
        grid-template-columns: repeat(3, minmax(0, 1fr));
      }

      .data-summary-grid {
        grid-template-columns: repeat(2, minmax(0, 1fr));
        margin-top: 12px;
      }

      .field {
        display: grid;
        gap: 6px;
        font-size: 13px;
        color: var(--muted);
      }

      .field input {
        width: 100%;
        border-radius: 12px;
        border: 1px solid rgba(69, 50, 26, 0.12);
        background: rgba(255, 255, 255, 0.9);
        padding: 11px 12px;
        font: inherit;
        color: var(--text);
      }

      .data-path {
        margin: 12px 0 0;
        color: var(--muted);
      }

      .data-list {
        display: grid;
        gap: 10px;
        margin-top: 12px;
      }

      .data-item {
        padding: 14px;
        border-radius: 18px;
        background: rgba(255, 255, 255, 0.62);
        border: 1px solid rgba(69, 50, 26, 0.07);
      }

      .data-item-top {
        display: flex;
        justify-content: space-between;
        gap: 12px;
        align-items: start;
      }

      .data-check {
        display: flex;
        align-items: center;
        gap: 10px;
        font-weight: 700;
      }

      .data-check input {
        width: 16px;
        height: 16px;
      }

      .data-item-actions {
        display: flex;
        flex-wrap: wrap;
        gap: 8px;
        margin-top: 10px;
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
        .logs-grid,
        .events,
        .config-grid,
        .runtime-grid,
        .data-layout,
        .data-summary-grid {
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
        .logs-grid,
        .events,
        .config-grid,
        .runtime-grid,
        .data-layout,
        .data-summary-grid,
        .data-controls,
        .data-actions {
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
              <button class="btn" id="restart-all-btn" type="button">Restart all</button>
              <button class="btn secondary" id="purge-all-btn" type="button">Purge all</button>
              <a class="btn secondary" href="#channels">Przejdź do kanałów</a>
              <a class="btn secondary" href="#data">Sekcja data</a>
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

      <section class="section" id="data">
        <div class="section-head">
          <h2 class="section-title">Data</h2>
          <p class="section-note">Przeglądanie katalogu danych, wyszukiwanie i wspólne pobieranie ZIP</p>
        </div>
        <div class="data-layout">
          <div class="data-panel panel">
            <div class="data-controls">
              <label class="field">
                Szukaj po ścieżce
                <input id="data-search" type="text" placeholder="np. line-a 2026-06-17 13" />
              </label>
              <button class="btn" id="data-refresh-btn" type="button">Odśwież</button>
              <button class="btn" id="data-up-btn" type="button">Do góry</button>
            </div>
            <div class="data-actions" style="margin-top: 12px;">
              <button class="btn" id="data-search-btn" type="button">Szukaj</button>
              <button class="btn" id="data-reset-btn" type="button">Katalog</button>
              <button class="btn" id="data-select-visible-btn" type="button">Zaznacz widoczne</button>
              <button class="btn" id="data-clear-selection-btn" type="button">Wyczyść zaznaczenie</button>
              <button class="btn secondary" id="data-download-selected-btn" type="button">Pobierz zaznaczone</button>
            </div>
            <p class="data-path mono" id="data-path">Ładowanie ścieżki data…</p>
            <p class="section-note" id="data-mode">Tryb: oczekiwanie</p>
            <div class="data-list" id="data-list"></div>
          </div>
          <div class="data-panel panel">
            <div class="section-head">
              <h2 class="section-title">Podsumowanie data</h2>
              <p class="section-note">Stan bieżącego widoku i zaznaczeń</p>
            </div>
            <div class="data-summary-grid" id="data-summary"></div>
          </div>
        </div>
      </section>

      <section class="section" id="logs">
        <div class="section-head">
          <h2 class="section-title">Logi kanałów</h2>
          <p class="section-note" id="logs-note">Ostatnie zdarzenia i alerty z kanałów</p>
        </div>
        <div class="logs-toolbar">
          <label class="field">
            Filtr kanału
            <select id="logs-channel-filter">
              <option value="">Wszystkie kanały</option>
            </select>
          </label>
          <button class="btn" id="logs-refresh-btn" type="button">Odśwież logi</button>
        </div>
        <div class="logs-grid">
          <div class="logs-panel panel">
            <div class="section-head">
              <h2 class="section-title">Alerty kanałów</h2>
              <p class="section-note" id="alerts-note">Zdarzenia warning/error z JSONL i procesu</p>
            </div>
            <div class="log-list" id="alerts-list"></div>
          </div>
          <div class="logs-panel panel">
            <div class="section-head">
              <h2 class="section-title">Log ogólny</h2>
              <p class="section-note" id="channel-logs-note">Ostatnie wpisy na kanał</p>
            </div>
            <div class="log-channel-list" id="channel-logs-list"></div>
          </div>
        </div>
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
            <a class="chip muted" href="/api/logs" target="_blank" rel="noreferrer">/api/logs</a>
            <a class="chip muted" href="/api/config" target="_blank" rel="noreferrer">/api/config</a>
            <a class="chip muted" href="/api/data" target="_blank" rel="noreferrer">/api/data</a>
            <a class="chip muted" href="/api/data/search?q=line-a" target="_blank" rel="noreferrer">/api/data/search</a>
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
      const LOG_REFRESH_MS = 6000;
      let refreshTimer = null;
      let lastLogsRefreshAt = 0;
      let currentDataPath = ".";
      let parentDataPath = ".";
      let currentDataItems = [];
      let dataSearchMode = false;
      let currentLogChannelFilter = "";
      const selectedDataPaths = new Set();

      function $(id) {
        return document.getElementById(id);
      }

      async function fetchJson(url, options) {
        const response = await fetch(url, options);
        const text = await response.text();
        let payload = {};
        try {
          payload = text ? JSON.parse(text) : {};
        } catch (error) {
          throw new Error(text || "Niepoprawna odpowiedź serwera");
        }
        if (!response.ok) {
          throw new Error(payload.error || (`HTTP ${response.status}`));
        }
        return payload;
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

      function basenamePath(value) {
        const text = String(value || "").trim();
        if (!text) {
          return "-";
        }
        const parts = text.split("/").filter(Boolean);
        return parts.length ? parts[parts.length - 1] : text;
      }

      function compactPathLabel(value, depth = 2) {
        const text = String(value || "").trim();
        if (!text) {
          return "-";
        }
        const parts = text.split("/").filter(Boolean);
        if (parts.length <= depth) {
          return text;
        }
        return `.../${parts.slice(-depth).join("/")}`;
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
        if ((overview.nodes_without_samples || 0) > 0) {
          heroCopy.push(`Uwaga: ${formatNumber(overview.nodes_without_samples)} online bez próbek.`);
        }
        if (supervisor.has_status) {
          heroCopy.push("Supervisor publikuje status runtime i zdarzenia.");
        } else {
          heroCopy.push("Konfiguracja jest dostępna, ale panel czeka jeszcze na pliki runtime z supervisora.");
        }
        $("hero-copy").textContent = heroCopy.join(" ");

        const chips = [];
        chips.push(chip(supervisor.has_status ? "status runtime obecny" : "brak statusu runtime", supervisor.has_status ? "good" : "warn"));
        if (supervisor.status_stale) {
          chips.push(chip(`dane nieaktualne (${formatNumber(supervisor.status_age_s || 0, 0)} s)`, "bad"));
        }
        if (supervisor.storage_free_bytes != null) {
          chips.push(chip(
            `wolne miejsce: ${formatNumber(supervisor.storage_free_bytes / 1073741824, 1)} GB`,
            supervisor.storage_low ? "warn" : "good"
          ));
        }
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
        let stateLabel = "STOPPED";
        if (!channel.enabled) {
          stateLabel = "DISABLED";
        } else if (channel.control_state === "stopped-manual") {
          stateLabel = "PAUSED";
        } else if (channel.control_state === "restart-pending") {
          stateLabel = "RESTARTING";
        } else if (channel.running) {
          stateLabel = "RUNNING";
        }
        const activeFile = channel.active_file || "";
        const activeFileName = activeFile ? basenamePath(activeFile) : "-";
        const runtimeCards = [
          {
            key: "port",
            label: "Port",
            value: channel.port ? basenamePath(channel.port) : "-",
            sub: channel.baud
              ? `${formatNumber(channel.baud)} baud`
              : null,
            title: channel.port || undefined,
          },
          {
            key: "file",
            label: "Plik",
            value: activeFileName,
            sub: activeFile ? compactPathLabel(activeFile, 3) : "brak aktywnego pliku",
            title: activeFile || undefined,
          },
          {
            key: "rate",
            label: "Rate",
            value: channel.instant_samples_per_second_5s == null ? "-" : `${formatFloat(channel.instant_samples_per_second_5s, 1)}`,
            sub: channel.last_samples_per_second == null
              ? "samples/s · średnia od startu: -"
              : `samples/s · średnia od startu ${formatFloat(channel.last_samples_per_second, 1)}`,
          },
          {
            key: "restarts",
            label: "Restarty",
            value: formatNumber(channel.restart_count || 0),
            sub: "tylko auto",
          },
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
                  <th>Gaps / Overflow / No data</th>
                  <th>Temperatura</th>
                </tr>
              </thead>
              <tbody>
                ${channel.nodes.map((node) => renderNodeRow(channel, node)).join("")}
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
                  · ${escapeHtml(compactPathLabel(channel.destination || "-", 2))}
                  · status ${escapeHtml(formatDate(channel.updated_utc))}
                </div>
              </div>
              <div class="chip-row">
                ${chip(stateLabel, channel.enabled ? (channel.running ? "good" : "warn") : "muted")}
                ${chip(channel.health || "unknown", healthKind)}
                ${chip(`alerty ${formatNumber(channel.attention_count || 0)}`, (channel.attention_count || 0) > 0 ? "warn" : "muted")}
              </div>
            </div>
            <div class="channel-actions">
              <button class="btn channel-action-btn" data-action="start" data-channel-name="${escapeHtml(channel.name)}" type="button" ${channel.enabled ? "" : "disabled"}>Start</button>
              <button class="btn channel-action-btn" data-action="restart" data-channel-name="${escapeHtml(channel.name)}" type="button" ${channel.enabled ? "" : "disabled"}>Restart</button>
              <button class="btn secondary channel-action-btn" data-action="stop" data-channel-name="${escapeHtml(channel.name)}" type="button">Stop</button>
              <button class="btn channel-action-btn" data-action="purge" data-channel-name="${escapeHtml(channel.name)}" type="button" ${channel.enabled ? "" : "disabled"}>Purge line</button>
            </div>
            <div class="runtime-grid">
              ${runtimeCards.map((card) => `
                <div class="runtime-card runtime-card-${escapeHtml(card.key || "default")}">
                  <span class="label">${escapeHtml(card.label)}</span>
                  <strong class="mono"${card.title ? ` title="${escapeHtml(card.title)}"` : ""}>${escapeHtml(card.value)}</strong>
                  ${card.sub ? `<div class="node-meta"${card.title ? ` title="${escapeHtml(card.title)}"` : ""}>${escapeHtml(card.sub)}</div>` : ""}
                </div>
              `).join("")}
            </div>
            ${nodesHtml}
          </article>
        `;
      }

      function renderNodeRow(channel, node) {
        const hasAlerts = Array.isArray(node.alerts) && node.alerts.length > 0;
        const onlineKind = node.online ? (hasAlerts ? "bad" : "good") : (node.has_runtime ? "bad" : "muted");
        const nodeTitle = node.name ? `${node.name}` : `Node ${node.node_id}`;
        const alerts = Array.isArray(node.alerts) && node.alerts.length
          ? node.alerts.join(", ")
          : "brak";
        const firmwareMeta = node.firmware_version
          ? `fw ${node.firmware_version}`
          : "fw -";
        return `
          <tr>
            <td data-label="Węzeł">
              <div class="node-name">${escapeHtml(nodeTitle)}</div>
              <div class="node-meta mono">id=${escapeHtml(node.node_id)} · ${escapeHtml(firmwareMeta)} · oczekiwany ODR ${escapeHtml(node.expected_odr_hz ?? "-")} Hz</div>
            </td>
            <td data-label="Stan">
              ${statusDot(onlineKind)}
              ${escapeHtml(node.online ? (node.sample_flow_state === "stalled" ? "ONLINE / BRAK PRÓBEK" : "ONLINE") : (node.has_runtime ? "OFFLINE" : "NO-RUNTIME"))}
              <div class="node-meta">${escapeHtml(alerts)}</div>
              <button
                class="btn secondary small node-firmware-restart-btn"
                data-channel-name="${escapeHtml(channel.name)}"
                data-node-id="${escapeHtml(node.node_id)}"
                type="button"
                ${channel.enabled && node.enabled ? "" : "disabled"}
              >Restart firmware</button>
            </td>
            <td data-label="ODR">
              <span class="mono">${escapeHtml(formatNumber(node.sensor_odr_hz || 0))} / ${escapeHtml(formatFloat(node.output_odr_hz || 0, 1))}</span>
              <div class="node-meta">sensor / output</div>
            </td>
            <td data-label="Próbki">
              <span class="mono">${escapeHtml(formatNumber(node.samples_written || 0))}</span>
              <div class="node-meta">${escapeHtml(
                node.instant_samples_per_second_5s == null
                  ? `next ${formatNumber(node.expected_sample_seq || 0)}`
                  : `${formatFloat(node.instant_samples_per_second_5s, 1)} samples/s · stab ${formatFloat(node.rate_stability_percent_5s ?? 0, 0)}%`
              )}</div>
            </td>
            <td data-label="Gaps / Overflow / No data">
              <span class="mono">gaps ${escapeHtml(formatNumber(node.gaps_detected || 0))}</span>
              <div class="node-meta">
                rx ${escapeHtml(formatNumber(node.rx_overflow_session || 0))}
                · pkt ${escapeHtml(formatNumber(node.packet_overwrite_session || 0))}
                · no_data ${escapeHtml(formatNumber(node.bursts_no_data || 0))}
              </div>
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
        renderLogChannelFilterOptions(channels);
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

      function renderLogChannelFilterOptions(channels) {
        const select = $("logs-channel-filter");
        const options = [`<option value="">Wszystkie kanały</option>`]
          .concat((channels || []).map((channel) => (
            `<option value="${escapeHtml(channel.name)}">${escapeHtml(channel.label || channel.name)}</option>`
          )));
        select.innerHTML = options.join("");
        const available = new Set((channels || []).map((channel) => channel.name));
        if (currentLogChannelFilter && available.has(currentLogChannelFilter)) {
          select.value = currentLogChannelFilter;
        } else {
          currentLogChannelFilter = "";
          select.value = "";
        }
      }

      function renderAlertItem(item) {
        const severity = item.severity || "info";
        const details = item.summary || item.line || "";
        const locationBits = [];
        if (item.channel_name) locationBits.push(`kanał ${item.channel_name}`);
        if (item.node_id != null) locationBits.push(`node ${item.node_id}`);
        if (item.source_label) locationBits.push(`źródło ${item.source_label}`);
        const kind = severity === "error" ? "bad" : (severity === "warning" ? "warn" : "info");
        return `
          <article class="event-item">
            <div class="event-main">
              <div class="event-title">${escapeHtml(item.event || "alert")}</div>
              ${chip(severity, kind)}
            </div>
            <div class="event-meta">
              ${escapeHtml(item.utc ? formatDate(item.utc) : "brak dokładnego czasu w linii procesu")}
              ${locationBits.length ? ` · ${escapeHtml(locationBits.join(" · "))}` : ""}
              ${details ? `<br>${escapeHtml(details)}` : ""}
            </div>
          </article>
        `;
      }

      function renderChannelLogCard(channel) {
        const eventItems = Array.isArray(channel.events) ? channel.events.slice().reverse().slice(0, 6) : [];
        const processLines = Array.isArray(channel.process_lines) ? channel.process_lines.slice().reverse().slice(0, 6) : [];
        return `
          <article class="log-channel-card">
            <div class="event-main">
              <div class="event-title">${escapeHtml(channel.label || channel.name || "-")}</div>
              <div class="chip-row">
                ${chip(channel.running ? "RUNNING" : "STOPPED", channel.running ? "good" : "warn")}
                ${chip(`alerty ${formatNumber(channel.alert_count || 0)}`, (channel.alert_count || 0) > 0 ? "warn" : "muted")}
              </div>
            </div>
            <div class="event-meta">${escapeHtml(channel.name || "-")}</div>
            <div class="log-subtitle">Zdarzenia JSONL</div>
            <div class="log-list">
              ${eventItems.length
                ? eventItems.map((event) => renderAlertItem(event)).join("")
                : `<div class="empty">Brak ostatnich zdarzeń JSONL dla kanału.</div>`}
            </div>
            <div class="log-subtitle">Ogon procesu</div>
            <div class="log-lines">
              ${processLines.length
                ? processLines.map((line) => `<div class="log-line mono">${escapeHtml(line)}</div>`).join("")
                : `<div class="empty">Brak ostatnich linii procesu dla kanału.</div>`}
            </div>
          </article>
        `;
      }

      function renderLogs(data) {
        const alerts = data.alerts || [];
        const channels = data.channels || [];
        $("alerts-note").textContent = alerts.length
          ? `Wyświetlane alerty: ${formatNumber(alerts.length)}`
          : "Brak alertów do pokazania";
        $("channel-logs-note").textContent = channels.length
          ? `Kanały w widoku: ${formatNumber(channels.length)}`
          : "Brak kanałów w widoku";
        $("logs-note").textContent = currentLogChannelFilter
          ? `Filtr: ${currentLogChannelFilter}`
          : "Widok zbiorczy wszystkich kanałów";
        $("alerts-list").innerHTML = alerts.length
          ? alerts.map((item) => renderAlertItem(item)).join("")
          : `<div class="empty">Brak alertów warning/error w ostatnim oknie logów.</div>`;
        $("channel-logs-list").innerHTML = channels.length
          ? channels.map((channel) => renderChannelLogCard(channel)).join("")
          : `<div class="empty">Brak logów kanałów dla bieżącego filtra.</div>`;
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

      function dataMetric(label, value, sub) {
        return `
          <article class="metric panel">
            <div class="label">${escapeHtml(label)}</div>
            <div class="value">${escapeHtml(value)}</div>
            <div class="sub">${escapeHtml(sub)}</div>
          </article>
        `;
      }

      function renderDataSummary(items, label) {
        const files = items.filter((item) => item.type === "file");
        const dirs = items.filter((item) => item.type === "directory");
        $("data-summary").innerHTML = [
          dataMetric("Pozycje", formatNumber(items.length), label),
          dataMetric("Katalogi", formatNumber(dirs.length), "widoczne w obecnym widoku"),
          dataMetric("Pliki", formatNumber(files.length), "widoczne w obecnym widoku"),
          dataMetric("Zaznaczone", formatNumber(selectedDataPaths.size), "do wspólnego pobrania"),
        ].join("");
      }

      function renderDataItems(items) {
        $("data-list").innerHTML = items.length
          ? items.map((item) => `
              <article class="data-item">
                <div class="data-item-top">
                  <label class="data-check">
                    <input class="data-select" type="checkbox" data-path="${encodeURIComponent(item.relative_path)}" ${selectedDataPaths.has(item.relative_path) ? "checked" : ""} />
                    <span>${escapeHtml(item.name)}</span>
                  </label>
                  ${chip(item.type === "directory" ? "katalog" : "plik", item.type === "directory" ? "info" : "good")}
                </div>
                <div class="node-meta mono">${escapeHtml(item.relative_path)}</div>
                <div class="node-meta">size=${escapeHtml(item.size_bytes)} · modified=${escapeHtml(item.modified_utc)}</div>
                <div class="data-item-actions">
                  ${item.type === "directory"
                    ? `<button class="btn data-browse-btn" data-path="${encodeURIComponent(item.relative_path)}" type="button">Otwórz</button>
                       <a class="btn secondary" href="${item.download_url}">Pobierz ZIP</a>`
                    : `<a class="btn secondary" href="${item.download_url}">Pobierz</a>`}
                </div>
              </article>
            `).join("")
          : `<div class="empty">Brak pozycji dla bieżącego widoku.</div>`;
      }

      async function browseData(path = ".") {
        const suffix = path && path !== "." ? `?path=${encodeURIComponent(path)}` : "";
        const payload = await fetchJson(`/api/data${suffix}`);
        dataSearchMode = false;
        currentDataPath = payload.relative_path || ".";
        parentDataPath = payload.parent_relative_path || ".";
        currentDataItems = payload.items || [];
        $("data-path").textContent = `root: ${payload.root} / ${currentDataPath}`;
        $("data-mode").textContent = "Tryb: przeglądanie katalogu data";
        renderDataSummary(currentDataItems, "pozycji w katalogu");
        renderDataItems(currentDataItems);
      }

      async function searchData() {
        const query = $("data-search").value.trim();
        if (!query) {
          await browseData(currentDataPath);
          return;
        }
        const payload = await fetchJson(`/api/data/search?q=${encodeURIComponent(query)}`);
        dataSearchMode = true;
        currentDataItems = payload.items || [];
        $("data-path").textContent = `root: ${payload.root}`;
        $("data-mode").textContent = `Tryb: wyszukiwanie "${query}"${payload.truncated ? " (przycięte do limitu)" : ""}`;
        renderDataSummary(currentDataItems, "wyników wyszukiwania");
        renderDataItems(currentDataItems);
      }

      function clearDataSelection() {
        selectedDataPaths.clear();
        renderDataSummary(currentDataItems, dataSearchMode ? "wyników wyszukiwania" : "pozycji w katalogu");
        renderDataItems(currentDataItems);
      }

      function selectVisibleData() {
        currentDataItems.forEach((item) => selectedDataPaths.add(item.relative_path));
        renderDataSummary(currentDataItems, dataSearchMode ? "wyników wyszukiwania" : "pozycji w katalogu");
        renderDataItems(currentDataItems);
      }

      async function downloadSelectedData() {
        if (selectedDataPaths.size === 0) {
          throw new Error("Najpierw zaznacz co pobrać.");
        }
        const response = await fetch("/api/data/download-bundle", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ paths: Array.from(selectedDataPaths) }),
        });
        const disposition = response.headers.get("Content-Disposition") || "";
        if (!response.ok) {
          const payload = await response.json().catch(() => ({}));
          throw new Error(payload.error || (`HTTP ${response.status}`));
        }
        const blob = await response.blob();
        const match = disposition.match(/filename="([^"]+)"/);
        const filename = match ? match[1] : "data-selection.zip";
        const url = URL.createObjectURL(blob);
        const anchor = document.createElement("a");
        anchor.href = url;
        anchor.download = filename;
        document.body.appendChild(anchor);
        anchor.click();
        anchor.remove();
        URL.revokeObjectURL(url);
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

      async function loadLogs(force = false) {
        const now = Date.now();
        if (!force && (now - lastLogsRefreshAt) < LOG_REFRESH_MS) {
          return;
        }
        const suffix = currentLogChannelFilter
          ? `?limit=12&channel=${encodeURIComponent(currentLogChannelFilter)}`
          : `?limit=12`;
        const payload = await fetchJson(`/api/logs${suffix}`);
        renderLogs(payload);
        lastLogsRefreshAt = Date.now();
      }

      async function refreshAll(forceLogs = false) {
        await loadDashboard();
        await loadLogs(forceLogs);
      }

      async function performChannelAction(channelName, action) {
        if (action === "purge") {
          const confirmed = window.confirm(`Wyczyścić logi runtime i zrestartować ${channelName}?`);
          if (!confirmed) {
            return;
          }
        }
        await fetchJson(`/api/channels/${encodeURIComponent(channelName)}/${encodeURIComponent(action)}`, {
          method: "POST",
        });
        $("overview-note").textContent = `Wysłano polecenie ${action} dla ${channelName}`;
        window.setTimeout(() => {
          loadDashboard();
        }, 900);
      }

      async function restartNodeFirmware(channelName, nodeId, button) {
        const confirmed = window.confirm(
          `Zrestartować firmware node ${nodeId} na ${channelName}? Recorder tej linii zostanie na chwilę zatrzymany i uruchomiony ponownie.`
        );
        if (!confirmed) {
          return;
        }
        const previousLabel = button.textContent;
        button.disabled = true;
        button.textContent = "Restartowanie...";
        try {
          await fetchJson(
            `/api/channels/${encodeURIComponent(channelName)}/nodes/${encodeURIComponent(nodeId)}/restart-firmware`,
            { method: "POST" }
          );
          $("overview-note").textContent = `Zrestartowano firmware node ${nodeId} na ${channelName}`;
          window.setTimeout(() => loadDashboard(), 900);
        } finally {
          button.disabled = false;
          button.textContent = previousLabel;
        }
      }

      async function performSupervisorAction(action) {
        if (action === "restart_all") {
          const confirmed = window.confirm("Zrestartować wszystkie kanały i zacząć pomiar od nowa?");
          if (!confirmed) {
            return;
          }
        }
        if (action === "purge_all") {
          const confirmed = window.confirm("Wyczyścić runtime i logi wszystkich kanałów, a potem uruchomić je od nowa?");
          if (!confirmed) {
            return;
          }
        }
        await fetchJson(`/api/supervisor/${encodeURIComponent(action)}`, {
          method: "POST",
        });
        $("overview-note").textContent = `Wysłano polecenie ${action} do supervisora`;
        window.setTimeout(() => {
          refreshAll(true).catch((error) => {
            $("logs-note").textContent = `Błąd odświeżania: ${error.message || error}`;
          });
        }, 900);
      }

      function scheduleRefresh() {
        if (refreshTimer !== null) {
          window.clearInterval(refreshTimer);
        }
        refreshTimer = window.setInterval(() => {
          refreshAll(false).catch((error) => {
            $("logs-note").textContent = `Błąd odświeżania: ${error.message || error}`;
          });
        }, REFRESH_MS);
      }

      $("refresh-btn").addEventListener("click", () => {
        refreshAll(true).catch((error) => alert(error.message));
        scheduleRefresh();
      });
      $("restart-all-btn").addEventListener("click", () => {
        performSupervisorAction("restart_all").catch((error) => alert(error.message));
      });
      $("purge-all-btn").addEventListener("click", () => {
        performSupervisorAction("purge_all").catch((error) => alert(error.message));
      });

      $("logs-refresh-btn").addEventListener("click", () => {
        loadLogs(true).catch((error) => alert(error.message));
      });

      $("logs-channel-filter").addEventListener("change", (event) => {
        currentLogChannelFilter = event.target.value || "";
        loadLogs(true).catch((error) => alert(error.message));
      });

      $("data-refresh-btn").addEventListener("click", () => {
        browseData(currentDataPath).catch((error) => alert(error.message));
      });

      $("data-up-btn").addEventListener("click", () => {
        browseData(parentDataPath).catch((error) => alert(error.message));
      });

      $("data-search-btn").addEventListener("click", () => {
        searchData().catch((error) => alert(error.message));
      });

      $("data-reset-btn").addEventListener("click", () => {
        $("data-search").value = "";
        browseData(currentDataPath).catch((error) => alert(error.message));
      });

      $("data-select-visible-btn").addEventListener("click", () => {
        selectVisibleData();
      });

      $("data-clear-selection-btn").addEventListener("click", () => {
        clearDataSelection();
      });

      $("data-download-selected-btn").addEventListener("click", () => {
        downloadSelectedData().catch((error) => alert(error.message));
      });

      $("data-search").addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          searchData().catch((error) => alert(error.message));
        }
      });

      $("data-list").addEventListener("click", (event) => {
        const target = event.target;
        if (target.classList.contains("data-browse-btn")) {
          browseData(decodeURIComponent(target.dataset.path || ".")).catch((error) => alert(error.message));
        }
      });

      $("data-list").addEventListener("change", (event) => {
        const target = event.target;
        if (target.classList.contains("data-select")) {
          const path = decodeURIComponent(target.dataset.path || "");
          if (!path) {
            return;
          }
          if (target.checked) {
            selectedDataPaths.add(path);
          } else {
            selectedDataPaths.delete(path);
          }
          renderDataSummary(currentDataItems, dataSearchMode ? "wyników wyszukiwania" : "pozycji w katalogu");
        }
      });

      $("channels-grid").addEventListener("click", (event) => {
        const target = event.target;
        if (target.classList.contains("node-firmware-restart-btn")) {
          const channelName = target.dataset.channelName || "";
          const nodeId = target.dataset.nodeId || "";
          if (!channelName || !nodeId) {
            return;
          }
          restartNodeFirmware(channelName, nodeId, target).catch((error) => alert(error.message));
          return;
        }
        if (!target.classList.contains("channel-action-btn")) {
          return;
        }
        const channelName = target.dataset.channelName || "";
        const action = target.dataset.action || "";
        if (!channelName || !action) {
          return;
        }
        performChannelAction(channelName, action).catch((error) => alert(error.message));
      });

      refreshAll(true).catch((error) => {
        $("hero-copy").textContent = `Nie udało się pobrać danych panelu: ${error}`;
        $("logs-note").textContent = "Błąd odczytu logów";
      });
      browseData().catch((error) => {
        $("data-path").textContent = `Błąd ładowania data: ${error}`;
        $("data-mode").textContent = "Tryb: błąd";
        $("data-list").innerHTML = `<div class="empty">Nie udało się załadować sekcji data.</div>`;
        $("data-summary").innerHTML = dataMetric("Status", "Błąd", "Sprawdź katalog danych i endpointy");
      });
      scheduleRefresh();
    </script>
  </body>
</html>
"""

LIVE_PREVIEW_HTML = """<!doctype html>
<html lang="pl">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Sensor System Live Preview</title>
    <style>
      :root {
        --bg: #f6f1e8;
        --panel: rgba(255, 252, 247, 0.9);
        --line: rgba(66, 47, 24, 0.12);
        --text: #25180d;
        --muted: #715843;
        --x: #c4511b;
        --y: #2f6f97;
        --z: #2d8a5f;
        --warn: #b53b31;
        --shadow: 0 18px 50px rgba(75, 50, 18, 0.12);
        --font-body: "Avenir Next", "Segoe UI", "Helvetica Neue", sans-serif;
        --font-display: "Iowan Old Style", "Palatino Linotype", "Book Antiqua", Georgia, serif;
      }

      * { box-sizing: border-box; }

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

      .shell {
        width: min(1240px, calc(100vw - 28px));
        margin: 24px auto 36px;
      }

      .panel {
        padding: 20px;
        border-radius: 24px;
        background: var(--panel);
        border: 1px solid var(--line);
        box-shadow: var(--shadow);
        backdrop-filter: blur(18px);
      }

      .hero {
        display: grid;
        grid-template-columns: minmax(0, 1fr) auto;
        gap: 18px;
        align-items: start;
      }

      .eyebrow {
        margin: 0 0 8px;
        letter-spacing: 0.14em;
        text-transform: uppercase;
        font-size: 11px;
        color: var(--muted);
      }

      h1 {
        margin: 0;
        font-family: var(--font-display);
        font-size: clamp(28px, 4vw, 46px);
        line-height: 1;
      }

      .copy {
        margin: 12px 0 0;
        color: var(--muted);
        line-height: 1.55;
      }

      .toolbar {
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
        background: rgba(255, 255, 255, 0.74);
        color: var(--text);
        cursor: pointer;
        text-decoration: none;
      }

      .btn:disabled {
        cursor: not-allowed;
        opacity: 0.55;
      }

      .meta {
        min-width: min(100%, 320px);
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
        margin-top: 6px;
        font-size: 20px;
        line-height: 1.1;
      }

      .status {
        margin-top: 16px;
      }

      .status-line {
        display: inline-flex;
        align-items: center;
        gap: 8px;
        padding: 8px 12px;
        border-radius: 999px;
        background: rgba(255, 255, 255, 0.7);
        border: 1px solid rgba(69, 50, 26, 0.08);
        color: var(--muted);
        font-size: 13px;
      }

      .dot {
        width: 8px;
        height: 8px;
        border-radius: 999px;
        background: var(--muted);
      }

      .dot.good { background: var(--z); }
      .dot.warn { background: #b46a18; }
      .dot.bad { background: var(--warn); }

      .grid {
        display: grid;
        grid-template-columns: 1fr;
        gap: 14px;
        margin-top: 18px;
      }

      canvas {
        width: 100%;
        height: min(62vh, 560px);
        display: block;
        border-radius: 20px;
        background:
          linear-gradient(180deg, rgba(250, 247, 240, 0.96), rgba(243, 236, 224, 0.96));
      }

      .legend {
        display: flex;
        gap: 14px;
        flex-wrap: wrap;
        margin-top: 14px;
        color: var(--muted);
        font-size: 13px;
      }

      .controls {
        display: grid;
        grid-template-columns: minmax(0, 180px) auto minmax(0, 180px) auto auto minmax(0, 250px) auto repeat(3, auto) minmax(0, 1fr);
        gap: 10px;
        align-items: end;
        margin: 0 0 16px;
      }

      .field {
        display: grid;
        gap: 6px;
        font-size: 13px;
        color: var(--muted);
      }

      .field input {
        width: 100%;
        border-radius: 12px;
        border: 1px solid rgba(69, 50, 26, 0.12);
        background: rgba(255, 255, 255, 0.9);
        padding: 11px 12px;
        font: inherit;
        color: var(--text);
      }

      .control-note {
        justify-self: end;
        color: var(--muted);
        font-size: 12px;
      }

      .legend-item {
        display: inline-flex;
        align-items: center;
        gap: 8px;
      }

      .legend-swatch {
        width: 18px;
        height: 3px;
        border-radius: 999px;
      }

      .notes,
      .stats {
        display: grid;
        grid-template-columns: repeat(4, minmax(0, 1fr));
        gap: 10px;
      }

      .stat-card {
        padding: 14px;
        border-radius: 18px;
        background: rgba(255, 255, 255, 0.68);
        border: 1px solid rgba(69, 50, 26, 0.07);
      }

      .stat-card .label {
        font-size: 11px;
        text-transform: uppercase;
        letter-spacing: 0.1em;
        color: var(--muted);
      }

      .stat-card strong {
        display: block;
        margin-top: 7px;
        font-size: 18px;
        line-height: 1.2;
      }

      .stat-card .sub {
        margin-top: 8px;
        color: var(--muted);
        font-size: 12px;
        line-height: 1.45;
      }

      .warning {
        margin-top: 14px;
        color: var(--warn);
      }

      .mono {
        font-family: ui-monospace, "SFMono-Regular", Menlo, Consolas, monospace;
        font-size: 12px;
      }

      @media (max-width: 980px) {
        .hero,
        .controls,
        .notes,
        .stats {
          grid-template-columns: 1fr;
        }

        .shell {
          width: min(100vw - 18px, 100%);
          margin: 10px auto 24px;
        }
      }
    </style>
  </head>
  <body>
    <div class="shell">
      <section class="panel">
        <div class="hero">
          <div>
            <p class="eyebrow">Podgląd osi na żywo</p>
            <h1 id="page-title">Ładowanie podglądu czujnika…</h1>
            <p class="copy" id="page-copy">
              Ten widok czyta tylko ogon aktywnego pliku HDF5, więc nie uruchamia dodatkowego strumienia z portu szeregowego.
            </p>
            <div class="toolbar">
              <a class="btn" href="/">Powrót do dashboardu</a>
              <button class="btn" id="refresh-btn" type="button">Odśwież teraz</button>
            </div>
            <div class="status">
              <div class="status-line">
                <span class="dot" id="status-dot"></span>
                <span id="status-text">Przygotowanie sesji podglądu…</span>
              </div>
              <div class="warning" id="status-detail"></div>
            </div>
          </div>
          <div class="meta">
            <div class="meta-card">
              <div class="eyebrow">Kanał</div>
              <strong id="channel-label">-</strong>
              <div class="copy mono" id="channel-name">-</div>
            </div>
            <div class="meta-card">
              <div class="eyebrow">Węzeł</div>
              <strong id="node-label">-</strong>
              <div class="copy mono" id="node-runtime">-</div>
            </div>
          </div>
        </div>
      </section>

      <section class="panel grid">
        <div>
          <div class="controls">
            <div class="field">
              Okno pliku
              <div class="mono" id="file-window-label">10 min</div>
            </div>
            <label class="field">
              FFT próbek
              <input id="fft-size" type="number" min="32" max="2048" step="32" value="256" />
            </label>
            <button class="btn" id="apply-fft-btn" type="button">Ustaw FFT</button>
            <label class="field">
              Skala wykresu [m/s^2]
              <input id="y-scale" type="number" min="0.001" max="500" step="0.001" placeholder="wspólna dla X/Y/Z" />
            </label>
            <button class="btn" id="apply-y-scale-btn" type="button">Ustaw skalę</button>
            <button class="btn" id="auto-y-scale-btn" type="button">Auto skala</button>
            <label class="field">
              Skocz do czasu
              <input id="time-jump" type="datetime-local" step="1" />
            </label>
            <button class="btn" id="jump-time-btn" type="button">Pokaż czas</button>
            <button class="btn" id="history-older-btn" type="button">Starsze</button>
            <button class="btn" id="history-newer-btn" type="button">Nowsze</button>
            <button class="btn" id="history-live-btn" type="button">Powrót na live</button>
            <div class="control-note mono" id="history-position">Pozycja: live tail</div>
          </div>
          <canvas id="chart"></canvas>
          <div class="legend">
            <span class="legend-item"><span class="legend-swatch" style="background: var(--x);"></span>oś X</span>
            <span class="legend-item"><span class="legend-swatch" style="background: var(--y);"></span>oś Y</span>
            <span class="legend-item"><span class="legend-swatch" style="background: var(--z);"></span>oś Z</span>
          </div>
          <div style="height: 14px;"></div>
          <canvas id="fft-chart"></canvas>
          <div class="legend">
            <span class="legend-item"><span class="legend-swatch" style="background: var(--x);"></span>FFT X</span>
            <span class="legend-item"><span class="legend-swatch" style="background: var(--y);"></span>FFT Y</span>
            <span class="legend-item"><span class="legend-swatch" style="background: var(--z);"></span>FFT Z</span>
          </div>
        </div>
        <div class="stats" id="stats"></div>
        <div class="notes" id="notes"></div>
      </section>
    </div>

    <script>
      const DEFAULT_LIMIT = 512;
      const VISIBLE_POLL_MS = 1000;
      const HIDDEN_POLL_MS = 4000;
      const state = {
        channelName: "",
        nodeId: 0,
        clientId: "",
        token: "",
        leaseTimeoutS: 20,
        timer: null,
        latestSeq: null,
        snapshot: null,
        selectedFile: null,
        pendingTargetUtc: null,
        fftSize: 256,
        manualYScaleAbs: null,
        yScaleInput: "",
        timeJumpInput: "",
      };

      function $(id) {
        return document.getElementById(id);
      }

      function params() {
        return new URLSearchParams(window.location.search);
      }

      async function fetchJson(url, options) {
        const response = await fetch(url, options);
        const text = await response.text();
        let payload = {};
        try {
          payload = text ? JSON.parse(text) : {};
        } catch (error) {
          throw new Error(text || "Niepoprawna odpowiedź serwera");
        }
        if (!response.ok) {
          const message = payload && payload.error ? payload.error : `HTTP ${response.status}`;
          const error = new Error(message);
          error.status = response.status;
          throw error;
        }
        return payload;
      }

      function formatNumber(value, digits = 2) {
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
          return value;
        }
        return new Intl.DateTimeFormat("pl-PL", {
          dateStyle: "medium",
          timeStyle: "medium",
        }).format(date);
      }

      function clampFftSize(value, upperBound) {
        const parsed = Number(value);
        const bounded = Number.isFinite(parsed) ? Math.max(32, Math.min(2048, Math.round(parsed))) : 256;
        let power = 32;
        while (power * 2 <= bounded) {
          power *= 2;
        }
        return Math.max(32, Math.min(power, upperBound || 2048));
      }

      function clampYScale(value) {
        const parsed = Number(value);
        if (!Number.isFinite(parsed)) {
          return null;
        }
        return Math.max(0.001, Math.min(500, parsed));
      }

      function utcToLocalInputValue(value) {
        if (!value) {
          return "";
        }
        const date = new Date(value);
        if (Number.isNaN(date.getTime())) {
          return "";
        }
        const pad = (part) => String(part).padStart(2, "0");
        return [
          date.getFullYear(),
          pad(date.getMonth() + 1),
          pad(date.getDate()),
        ].join("-") + "T" + [
          pad(date.getHours()),
          pad(date.getMinutes()),
          pad(date.getSeconds()),
        ].join(":");
      }

      function localInputValueToUtc(value) {
        if (!value) {
          return null;
        }
        const date = new Date(value);
        if (Number.isNaN(date.getTime())) {
          return null;
        }
        return date.toISOString();
      }

      function centeredSeries(values) {
        if (!Array.isArray(values) || values.length === 0) {
          return [];
        }
        const mean = values.reduce((sum, value) => sum + Number(value || 0), 0) / values.length;
        return values.map((value) => Number(value || 0) - mean);
      }

      function buildPlotModel(snapshot) {
        const x = centeredSeries(snapshot.x || []);
        const y = centeredSeries(snapshot.y || []);
        const z = centeredSeries(snapshot.z || []);
        const allValues = [...x, ...y, ...z];
        const autoScaleAbs = Math.max(
          0.001,
          allValues.reduce((maxValue, value) => Math.max(maxValue, Math.abs(value)), 0) * 1.15 || 0.01,
        );
        return {
          x,
          y,
          z,
          autoScaleAbs,
          scaleAbs: state.manualYScaleAbs == null ? autoScaleAbs : state.manualYScaleAbs,
        };
      }

      function hannWindow(n, i) {
        return 0.5 - 0.5 * Math.cos((2 * Math.PI * i) / Math.max(1, n - 1));
      }

      function fftRadix2(re, im) {
        const n = re.length;
        for (let i = 1, j = 0; i < n; i += 1) {
          let bit = n >> 1;
          for (; j & bit; bit >>= 1) {
            j ^= bit;
          }
          j ^= bit;
          if (i < j) {
            [re[i], re[j]] = [re[j], re[i]];
            [im[i], im[j]] = [im[j], im[i]];
          }
        }
        for (let len = 2; len <= n; len <<= 1) {
          const ang = -2 * Math.PI / len;
          const wlenRe = Math.cos(ang);
          const wlenIm = Math.sin(ang);
          for (let i = 0; i < n; i += len) {
            let wRe = 1;
            let wIm = 0;
            for (let j = 0; j < len / 2; j += 1) {
              const uRe = re[i + j];
              const uIm = im[i + j];
              const vRe = re[i + j + len / 2] * wRe - im[i + j + len / 2] * wIm;
              const vIm = re[i + j + len / 2] * wIm + im[i + j + len / 2] * wRe;
              re[i + j] = uRe + vRe;
              im[i + j] = uIm + vIm;
              re[i + j + len / 2] = uRe - vRe;
              im[i + j + len / 2] = uIm - vIm;
              const nextWRe = wRe * wlenRe - wIm * wlenIm;
              const nextWIm = wRe * wlenIm + wIm * wlenRe;
              wRe = nextWRe;
              wIm = nextWIm;
            }
          }
        }
      }

      function computePsdDb(values, sampleRateHz, fftSize) {
        const fs = Number(sampleRateHz || 0);
        if (!fs || !Array.isArray(values) || values.length < 32) {
          return { freqs: [], psdDb: [] };
        }
        const n = clampFftSize(fftSize, values.length);
        const segment = values.slice(-n);
        const re = new Float64Array(n);
        const im = new Float64Array(n);
        let winPowSum = 0;
        for (let i = 0; i < n; i += 1) {
          const w = hannWindow(n, i);
          re[i] = segment[i] * w;
          winPowSum += w * w;
        }
        fftRadix2(re, im);
        const half = Math.floor(n / 2);
        const freqs = new Array(half);
        const psdDb = new Array(half);
        const denom = fs * Math.max(1e-12, winPowSum);
        for (let k = 0; k < half; k += 1) {
          const p = (re[k] * re[k] + im[k] * im[k]) / denom;
          const oneSided = k === 0 ? p : 2 * p;
          freqs[k] = (k * fs) / n;
          psdDb[k] = 10 * Math.log10(oneSided + 1e-24);
        }
        return { freqs, psdDb };
      }

      function setStatus(kind, text, detail = "") {
        $("status-dot").className = `dot ${kind}`;
        $("status-text").textContent = text;
        $("status-detail").textContent = detail;
      }

      function ensureClientId() {
        const key = `live-preview-client:${state.channelName}:${state.nodeId}`;
        let value = window.sessionStorage.getItem(key);
        if (!value) {
          value = window.crypto && window.crypto.randomUUID
            ? window.crypto.randomUUID()
            : `client-${Date.now()}-${Math.random().toString(16).slice(2)}`;
          window.sessionStorage.setItem(key, value);
        }
        state.clientId = value;
      }

      function resizeCanvas(canvas) {
        const ratio = window.devicePixelRatio || 1;
        const rect = canvas.getBoundingClientRect();
        canvas.width = Math.max(300, Math.floor(rect.width * ratio));
        canvas.height = Math.max(220, Math.floor(rect.height * ratio));
      }

      function drawChart(snapshot) {
        const canvas = $("chart");
        resizeCanvas(canvas);
        const ctx = canvas.getContext("2d");
        const width = canvas.width;
        const height = canvas.height;
        ctx.clearRect(0, 0, width, height);

        const padding = { top: 22, right: 18, bottom: 40, left: 68 };
        const plotLeft = padding.left;
        const plotTop = padding.top;
        const plotWidth = width - padding.left - padding.right;
        const plotHeight = height - padding.top - padding.bottom;

        ctx.fillStyle = "#f7f2e8";
        ctx.fillRect(plotLeft, plotTop, plotWidth, plotHeight);

        const plot = buildPlotModel(snapshot);
        const limit = Math.max(0.001, Number(plot.scaleAbs || 1));
        const odr = Math.max(1, Number(snapshot.output_odr_hz || 1));
        const sampleCount = snapshot.sample_count || 0;
        const windowSeconds = sampleCount > 1 ? (sampleCount - 1) / odr : 0;

        ctx.strokeStyle = "rgba(76, 58, 35, 0.13)";
        ctx.lineWidth = 1;
        for (let i = 0; i <= 6; i += 1) {
          const y = plotTop + (plotHeight / 6) * i;
          ctx.beginPath();
          ctx.moveTo(plotLeft, y);
          ctx.lineTo(plotLeft + plotWidth, y);
          ctx.stroke();
        }
        for (let i = 0; i <= 6; i += 1) {
          const x = plotLeft + (plotWidth / 6) * i;
          ctx.beginPath();
          ctx.moveTo(x, plotTop);
          ctx.lineTo(x, plotTop + plotHeight);
          ctx.stroke();
        }

        ctx.strokeStyle = "rgba(57, 42, 22, 0.24)";
        ctx.lineWidth = 1.5;
        ctx.strokeRect(plotLeft, plotTop, plotWidth, plotHeight);

        function drawAxisSeries(values, color) {
          if (!Array.isArray(values) || values.length === 0) {
            return;
          }
          ctx.beginPath();
          ctx.strokeStyle = color;
          ctx.lineWidth = 2;
          values.forEach((value, index) => {
            const x = plotLeft + (plotWidth * index) / Math.max(1, values.length - 1);
            const normalized = (Number(value) + limit) / (2 * limit);
            const y = plotTop + plotHeight - normalized * plotHeight;
            if (index === 0) {
              ctx.moveTo(x, y);
            } else {
              ctx.lineTo(x, y);
            }
          });
          ctx.stroke();
        }

        drawAxisSeries(plot.x, "#c4511b");
        drawAxisSeries(plot.y, "#2f6f97");
        drawAxisSeries(plot.z, "#2d8a5f");

        ctx.fillStyle = "#5e4b38";
        ctx.font = `${12 * (window.devicePixelRatio || 1)}px ui-monospace, monospace`;
        ctx.textAlign = "right";
        ctx.fillText(`${formatNumber(limit)} ${snapshot.accel_unit}`, plotLeft - 10, plotTop + 4);
        ctx.fillText("0", plotLeft - 10, plotTop + plotHeight / 2 + 4);
        ctx.fillText(`${formatNumber(-limit)} ${snapshot.accel_unit}`, plotLeft - 10, plotTop + plotHeight - 2);

        ctx.textAlign = "center";
        ctx.fillText(`czas [s], okno ~${formatNumber(windowSeconds, 1)} · składowa stała usunięta`, plotLeft + plotWidth / 2, height - 10);

        ctx.save();
        ctx.translate(16, plotTop + plotHeight / 2);
        ctx.rotate(-Math.PI / 2);
        ctx.fillText(`przyspieszenie [${snapshot.accel_unit}]`, 0, 0);
        ctx.restore();
      }

      function drawFft(snapshot) {
        const canvas = $("fft-chart");
        resizeCanvas(canvas);
        const ctx = canvas.getContext("2d");
        const width = canvas.width;
        const height = canvas.height;
        ctx.clearRect(0, 0, width, height);

        const fftX = centeredSeries(snapshot.fft_x || snapshot.x || []);
        const fftY = centeredSeries(snapshot.fft_y || snapshot.y || []);
        const fftZ = centeredSeries(snapshot.fft_z || snapshot.z || []);
        const xPsd = computePsdDb(fftX, snapshot.output_odr_hz, state.fftSize);
        const yPsd = computePsdDb(fftY, snapshot.output_odr_hz, state.fftSize);
        const zPsd = computePsdDb(fftZ, snapshot.output_odr_hz, state.fftSize);
        const series = [
          { freqs: xPsd.freqs, values: xPsd.psdDb, color: "#c4511b" },
          { freqs: yPsd.freqs, values: yPsd.psdDb, color: "#2f6f97" },
          { freqs: zPsd.freqs, values: zPsd.psdDb, color: "#2d8a5f" },
        ];
        const populated = series
          .map((item) => ({
            color: item.color,
            freqs: item.freqs.slice(1),
            values: item.values.slice(1),
          }))
          .filter((item) => item.values.length > 1);
        if (populated.length === 0) {
          ctx.fillStyle = "#5e4b38";
          ctx.font = `${14 * (window.devicePixelRatio || 1)}px ui-monospace, monospace`;
          ctx.fillText("Za mało próbek do FFT", 16, 28);
          return;
        }

        const padding = { top: 22, right: 18, bottom: 40, left: 68 };
        const plotLeft = padding.left;
        const plotTop = padding.top;
        const plotWidth = width - padding.left - padding.right;
        const plotHeight = height - padding.top - padding.bottom;
        const peakDb = populated.reduce((maxValue, item) => Math.max(maxValue, ...item.values), -Infinity);
        const normalized = populated.map((item) => ({
          color: item.color,
          freqs: item.freqs,
          values: item.values.map((value) => value - peakDb),
        }));
        const minRelativeDb = normalized.reduce((minValue, item) => Math.min(minValue, ...item.values), 0);
        const floorDb = Math.max(-140, Math.min(-20, Math.floor((minRelativeDb - 3) / 10) * 10));
        const ceilDb = 0;
        const spanDb = Math.max(20, ceilDb - floorDb);
        const minFreq = Math.min(...normalized.flatMap((item) => item.freqs.filter((freq) => freq > 0)));
        const maxFreq = Math.max(...normalized.flatMap((item) => item.freqs));
        const logMin = Math.log10(Math.max(minFreq, 1e-6));
        const logMax = Math.log10(Math.max(maxFreq, minFreq * 1.01));

        ctx.fillStyle = "#f7f2e8";
        ctx.fillRect(plotLeft, plotTop, plotWidth, plotHeight);
        ctx.strokeStyle = "rgba(76, 58, 35, 0.13)";
        ctx.lineWidth = 1;
        for (let i = 0; i <= 6; i += 1) {
          const y = plotTop + (plotHeight / 6) * i;
          ctx.beginPath();
          ctx.moveTo(plotLeft, y);
          ctx.lineTo(plotLeft + plotWidth, y);
          ctx.stroke();
        }

        const xTicks = [];
        let decade = Math.floor(logMin);
        while (decade <= Math.ceil(logMax)) {
          for (const factor of [1, 2, 5]) {
            const freq = factor * (10 ** decade);
            if (freq >= minFreq && freq <= maxFreq) {
              xTicks.push(freq);
            }
          }
          decade += 1;
        }
        xTicks.forEach((freq) => {
          const x = plotLeft + plotWidth * ((Math.log10(freq) - logMin) / Math.max(1e-9, logMax - logMin));
          ctx.beginPath();
          ctx.moveTo(x, plotTop);
          ctx.lineTo(x, plotTop + plotHeight);
          ctx.stroke();
        });
        ctx.strokeStyle = "rgba(57, 42, 22, 0.24)";
        ctx.lineWidth = 1.5;
        ctx.strokeRect(plotLeft, plotTop, plotWidth, plotHeight);

        function freqToX(freq) {
          return plotLeft + plotWidth * ((Math.log10(Math.max(freq, minFreq)) - logMin) / Math.max(1e-9, logMax - logMin));
        }

        function drawSpectrum(item) {
          ctx.beginPath();
          ctx.strokeStyle = item.color;
          ctx.lineWidth = 2;
          item.values.forEach((value, index) => {
            const freq = item.freqs[index];
            const x = freqToX(freq);
            const y = plotTop + plotHeight - ((value - floorDb) / spanDb) * plotHeight;
            if (index === 0) {
              ctx.moveTo(x, y);
            } else {
              ctx.lineTo(x, y);
            }
          });
          ctx.stroke();
        }

        normalized.forEach(drawSpectrum);
        ctx.fillStyle = "#5e4b38";
        ctx.font = `${12 * (window.devicePixelRatio || 1)}px ui-monospace, monospace`;
        ctx.textAlign = "right";
        ctx.fillText(`${formatNumber(ceilDb, 0)} dB`, plotLeft - 10, plotTop + 4);
        ctx.fillText(`${formatNumber(floorDb + spanDb / 2, 0)} dB`, plotLeft - 10, plotTop + plotHeight / 2 + 4);
        ctx.fillText(`${formatNumber(floorDb, 0)} dB`, plotLeft - 10, plotTop + plotHeight - 2);

        ctx.textAlign = "center";
        xTicks.forEach((freq) => {
          const x = freqToX(freq);
          const label = freq >= 10 ? formatNumber(freq, 0) : formatNumber(freq, 2);
          ctx.fillText(label, x, plotTop + plotHeight + 16);
        });

        ctx.fillText(`częstotliwość [Hz], skala log · FFT ${state.fftSize} próbek`, plotLeft + plotWidth / 2, height - 10);
        ctx.save();
        ctx.translate(16, plotTop + plotHeight / 2);
        ctx.rotate(-Math.PI / 2);
        ctx.fillText("widmo względne [dB]", 0, 0);
        ctx.restore();
      }

      function renderStats(snapshot) {
        const plot = buildPlotModel(snapshot);
        const cards = [
          {
            label: "Zakres",
            value: `+-${formatNumber(plot.scaleAbs, 3)} ${snapshot.accel_unit}`,
            sub: state.manualYScaleAbs == null
              ? `auto · wspólny zakres dla X/Y/Z po usunięciu stałej · sensor range ${formatNumber(snapshot.range_g, 0)} g`
              : `manual · wspólny zakres dla X/Y/Z po usunięciu stałej · sensor range ${formatNumber(snapshot.range_g, 0)} g`,
          },
          {
            label: "ODR output",
            value: `${formatNumber(snapshot.output_odr_hz, 1)} Hz`,
            sub: `sensor ${formatNumber(snapshot.sensor_odr_hz, 0)} Hz · FFT ${state.fftSize}`,
          },
          {
            label: "Próbki w oknie",
            value: String(snapshot.raw_sample_count || snapshot.total_samples || 0),
            sub: `wykres ${snapshot.sample_count || 0} pkt · FFT ${snapshot.fft_sample_count || 0} pkt`,
          },
          {
            label: "Ostatnie odświeżenie",
            value: formatDate(snapshot.generated_utc),
            sub: snapshot.file_name || "-",
          },
        ];
        $("stats").innerHTML = cards.map((card) => `
          <article class="stat-card">
            <div class="label">${card.label}</div>
            <strong>${card.value}</strong>
            <div class="sub mono">${card.sub}</div>
          </article>
        `).join("");
      }

      function renderNotes(snapshot) {
        const cards = [
          {
            label: "Kanał runtime",
            value: snapshot.channel_label || snapshot.channel_name,
            sub: snapshot.channel_name,
          },
          {
            label: "Węzeł",
            value: snapshot.node_label || `Node ${snapshot.node_id}`,
            sub: `id=${snapshot.node_id}`,
          },
          {
            label: "Plik źródłowy",
            value: snapshot.file_name || "-",
            sub: snapshot.file_path || "-",
          },
          {
            label: "Tryb bezpieczny",
            value: "1 aktywna sesja",
            sub: `timeout lease ${formatNumber(state.leaseTimeoutS, 0)} s`,
          },
          {
            label: "Pozycja okna",
            value: snapshot.is_live_tail ? "live tail" : "historia",
            sub: `${snapshot.window_start_index || 0} / ${snapshot.total_samples || 0} -> ${snapshot.window_end_index || 0} / ${snapshot.total_samples || 0}`,
          },
          {
            label: "Czas okna",
            value: snapshot.window_start_utc_estimated ? formatDate(snapshot.window_start_utc_estimated) : "-",
            sub: snapshot.window_end_utc_estimated ? formatDate(snapshot.window_end_utc_estimated) : "brak znacznika czasu",
          },
        ];
        $("notes").innerHTML = cards.map((card) => `
          <article class="stat-card">
            <div class="label">${card.label}</div>
            <strong>${card.value}</strong>
            <div class="sub mono">${card.sub}</div>
          </article>
        `).join("");
      }

      function updateHeader(snapshot) {
        $("page-title").textContent = `${snapshot.channel_label || snapshot.channel_name} / ${snapshot.node_label || `Node ${snapshot.node_id}`}`;
        $("channel-label").textContent = snapshot.channel_label || snapshot.channel_name;
        $("channel-name").textContent = snapshot.channel_name;
        $("node-label").textContent = snapshot.node_label || `Node ${snapshot.node_id}`;
        $("node-runtime").textContent = `ODR ${formatNumber(snapshot.output_odr_hz, 1)} Hz · ${formatNumber(snapshot.range_g, 0)} g`;
      }

      function updateHistoryControls(snapshot) {
        const startIndex = Number(snapshot.window_start_index || 0);
        const endIndex = Number(snapshot.window_end_index || 0);
        const totalSamples = Number(snapshot.total_samples || 0);
        $("history-position").textContent = snapshot.is_active_file
          ? `Plik aktywny · ${snapshot.file_index + 1} / ${snapshot.file_count} · ${startIndex} -> ${endIndex} / ${totalSamples}`
          : `Plik historii · ${snapshot.file_index + 1} / ${snapshot.file_count} · ${snapshot.file_name}`;
        $("history-older-btn").disabled = !snapshot.previous_file_path;
        $("history-newer-btn").disabled = !snapshot.next_file_path;
        $("history-live-btn").disabled = snapshot.is_active_file;
        $("file-window-label").textContent = `${formatNumber(snapshot.file_window_seconds / 60, 0)} min · ${snapshot.file_name}`;
        if (document.activeElement !== $("fft-size")) {
          $("fft-size").value = String(state.fftSize);
        }
        if (document.activeElement !== $("y-scale")) {
          $("y-scale").value = state.yScaleInput;
        }
        if (document.activeElement !== $("time-jump")) {
          const fallbackValue = utcToLocalInputValue(snapshot.window_end_utc_estimated || snapshot.capture_start_utc);
          $("time-jump").value = state.timeJumpInput || fallbackValue;
        }
      }

      function isHistoryMode() {
        return !!(state.snapshot && !state.snapshot.is_active_file);
      }

      async function acquire() {
        ensureClientId();
        setStatus("warn", "Rezerwacja sesji podglądu…");
        const payload = await fetchJson("/api/live/acquire", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            channel_name: state.channelName,
            node_id: state.nodeId,
            client_id: state.clientId,
            file_mode: true,
          }),
        });
        state.token = payload.token;
        state.leaseTimeoutS = payload.lease_timeout_s || state.leaseTimeoutS;
        state.snapshot = payload.snapshot || null;
        if (state.snapshot) {
          state.latestSeq = state.snapshot.last_sample_seq || null;
          state.selectedFile = state.snapshot.is_active_file ? null : state.snapshot.file_path;
          updateHeader(state.snapshot);
          drawChart(state.snapshot);
          drawFft(state.snapshot);
          renderStats(state.snapshot);
          renderNotes(state.snapshot);
          updateHistoryControls(state.snapshot);
        }
        setStatus("good", "Podgląd aktywny", "Ten widok utrzymuje wyłączną sesję, więc druga karta nie uruchomi równoległego odczytu.");
      }

      async function loadSnapshot() {
        if (!state.token) {
          return;
        }
        let url = `/api/live/data?channel=${encodeURIComponent(state.channelName)}&node=${encodeURIComponent(state.nodeId)}&token=${encodeURIComponent(state.token)}&file_mode=1`;
        if (state.selectedFile) {
          url += `&selected_file=${encodeURIComponent(state.selectedFile)}`;
        }
        if (state.pendingTargetUtc) {
          url += `&target_utc=${encodeURIComponent(state.pendingTargetUtc)}`;
        }
        const payload = await fetchJson(url);
        const snapshot = payload.snapshot || payload;
        state.snapshot = snapshot;
        const latestSeq = snapshot.last_sample_seq || null;
        state.selectedFile = snapshot.is_active_file ? null : snapshot.file_path;
        state.pendingTargetUtc = null;
        updateHeader(snapshot);
        renderStats(snapshot);
        renderNotes(snapshot);
        updateHistoryControls(snapshot);
        state.latestSeq = latestSeq;
        drawChart(snapshot);
        drawFft(snapshot);
        setStatus(
          "good",
          snapshot.is_live_tail ? "Podgląd aktywny" : "Podgląd historii",
          snapshot.is_live_tail
            ? `Ostatni odczyt: ${formatDate(snapshot.generated_utc)}`
            : `Przeglądasz starsze próbki. Auto-odświeżanie live jest wstrzymane.`
        );
      }

      function scheduleNextPoll() {
        if (state.timer !== null) {
          window.clearTimeout(state.timer);
          state.timer = null;
        }
        if (isHistoryMode()) {
          return;
        }
        const delay = document.hidden ? HIDDEN_POLL_MS : VISIBLE_POLL_MS;
        state.timer = window.setTimeout(async () => {
          try {
            await loadSnapshot();
          } catch (error) {
            if (error.status === 409) {
              setStatus("bad", "Sesja podglądu została przejęta lub wygasła", error.message);
              return;
            }
            setStatus("warn", "Błąd odświeżania", String(error.message || error));
          }
          scheduleNextPoll();
        }, delay);
      }

      async function release() {
        if (!state.token) {
          return;
        }
        const payload = JSON.stringify({ token: state.token });
        state.token = "";
        if (navigator.sendBeacon) {
          const blob = new Blob([payload], { type: "application/json" });
          navigator.sendBeacon("/api/live/release", blob);
          return;
        }
        await fetch("/api/live/release", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: payload,
          keepalive: true,
        }).catch(() => undefined);
      }

      function applyFftSize() {
        const upperBound = state.snapshot ? Math.max(32, Number(state.snapshot.fft_sample_count || state.snapshot.raw_sample_count || 2048)) : 2048;
        state.fftSize = clampFftSize($("fft-size").value, upperBound);
        if (state.snapshot) {
          drawFft(state.snapshot);
          renderStats(state.snapshot);
          updateHistoryControls(state.snapshot);
        }
      }

      function applyYScale() {
        state.yScaleInput = $("y-scale").value.trim();
        if (!state.yScaleInput) {
          state.manualYScaleAbs = null;
        } else {
          state.manualYScaleAbs = clampYScale(state.yScaleInput);
          if (state.manualYScaleAbs == null) {
            throw new Error("Podaj poprawną skalę wykresu.");
          }
        }
        if (state.snapshot) {
          renderStats(state.snapshot);
          renderNotes(state.snapshot);
          drawChart(state.snapshot);
          drawFft(state.snapshot);
          updateHistoryControls(state.snapshot);
        }
      }

      function resetYScale() {
        state.manualYScaleAbs = null;
        state.yScaleInput = "";
        if (state.snapshot) {
          renderStats(state.snapshot);
          renderNotes(state.snapshot);
          drawChart(state.snapshot);
          drawFft(state.snapshot);
          updateHistoryControls(state.snapshot);
        }
      }

      async function goOlder() {
        if (!state.snapshot || !state.snapshot.previous_file_path) {
          return;
        }
        state.selectedFile = state.snapshot.previous_file_path;
        await loadSnapshot();
        scheduleNextPoll();
      }

      async function goNewer() {
        if (!state.snapshot) {
          return;
        }
        state.selectedFile = state.snapshot.next_file_path || null;
        await loadSnapshot();
        scheduleNextPoll();
      }

      async function goLiveTail() {
        state.selectedFile = null;
        await loadSnapshot();
        scheduleNextPoll();
      }

      async function jumpToTime() {
        if (!state.snapshot) {
          return;
        }
        state.timeJumpInput = $("time-jump").value;
        const targetUtc = localInputValueToUtc(state.timeJumpInput);
        if (!targetUtc) {
          throw new Error("Podaj poprawną datę i godzinę.");
        }
        state.pendingTargetUtc = targetUtc;
        await loadSnapshot();
        scheduleNextPoll();
      }

      async function init() {
        const query = params();
        state.channelName = query.get("channel") || "";
        state.nodeId = Number(query.get("node") || "0");
        state.selectedFile = null;
        state.pendingTargetUtc = null;
        state.fftSize = clampFftSize(query.get("fft") || 256, 2048);
        state.yScaleInput = "";
        state.timeJumpInput = "";
        $("fft-size").value = String(state.fftSize);
        if (!state.channelName || !state.nodeId) {
          setStatus("bad", "Brak parametrów podglądu", "Link powinien zawierać kanał i numer węzła.");
          $("refresh-btn").disabled = true;
          $("apply-fft-btn").disabled = true;
          $("apply-y-scale-btn").disabled = true;
          $("auto-y-scale-btn").disabled = true;
          $("jump-time-btn").disabled = true;
          $("history-older-btn").disabled = true;
          $("history-newer-btn").disabled = true;
          $("history-live-btn").disabled = true;
          return;
        }
        try {
          await acquire();
          await loadSnapshot();
          scheduleNextPoll();
        } catch (error) {
          const detail = String(error.message || error);
          if (error.status === 409) {
            setStatus("bad", "Podgląd jest już otwarty w innej sesji", detail);
          } else {
            setStatus("bad", "Nie udało się uruchomić podglądu", detail);
          }
          $("page-copy").textContent = "Podgląd nie startuje drugiego procesu odczytu. Najpierw zwolnij aktywną sesję albo sprawdź, czy kanał naprawdę zapisuje dane.";
          $("refresh-btn").disabled = true;
        }
      }

      $("refresh-btn").addEventListener("click", () => {
        loadSnapshot().catch((error) => {
          setStatus("warn", "Błąd odświeżania", String(error.message || error));
        });
      });

      $("apply-fft-btn").addEventListener("click", () => {
        applyFftSize();
      });

      $("apply-y-scale-btn").addEventListener("click", () => {
        try {
          applyYScale();
        } catch (error) {
          setStatus("warn", "Błąd ustawiania skali", String(error.message || error));
        }
      });

      $("auto-y-scale-btn").addEventListener("click", () => {
        resetYScale();
      });

      $("fft-size").addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          applyFftSize();
        }
      });

      $("y-scale").addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          try {
            applyYScale();
          } catch (error) {
            setStatus("warn", "Błąd ustawiania skali", String(error.message || error));
          }
        }
      });

      $("y-scale").addEventListener("input", (event) => {
        state.yScaleInput = event.target.value;
      });

      $("time-jump").addEventListener("input", (event) => {
        state.timeJumpInput = event.target.value;
      });

      $("jump-time-btn").addEventListener("click", () => {
        jumpToTime().catch((error) => {
          setStatus("warn", "Błąd skoku do czasu", String(error.message || error));
        });
      });

      $("time-jump").addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          jumpToTime().catch((error) => {
            setStatus("warn", "Błąd skoku do czasu", String(error.message || error));
          });
        }
      });

      $("history-older-btn").addEventListener("click", () => {
        goOlder().catch((error) => {
          setStatus("warn", "Błąd przewijania historii", String(error.message || error));
        });
      });

      $("history-newer-btn").addEventListener("click", () => {
        goNewer().catch((error) => {
          setStatus("warn", "Błąd przewijania historii", String(error.message || error));
        });
      });

      $("history-live-btn").addEventListener("click", () => {
        goLiveTail().catch((error) => {
          setStatus("warn", "Błąd powrotu do live", String(error.message || error));
        });
      });

      window.addEventListener("resize", () => {
        if (state.snapshot) {
          drawChart(state.snapshot);
          drawFft(state.snapshot);
        }
      });

      document.addEventListener("visibilitychange", () => {
        if (state.token) {
          scheduleNextPoll();
        }
      });

      window.addEventListener("beforeunload", () => {
        release();
      });

      init();
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


def tail_text(path: Path, limit: int) -> list[str]:
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
    return [line.decode("utf-8", errors="replace") for line in lines[-limit:] if line.strip()]


def load_last_samples_rate(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    try:
        with path.open("rb") as handle:
            handle.seek(0, 2)
            pos = handle.tell()
            buffer = b""
            lines: list[bytes] = []
            while pos > 0 and len(lines) <= 40:
                read_size = min(4096, pos)
                pos -= read_size
                handle.seek(pos)
                buffer = handle.read(read_size) + buffer
                lines = buffer.splitlines()
    except OSError:
        return None

    for line in reversed(lines):
        try:
            text = line.decode("utf-8", errors="replace").strip()
        except Exception:
            continue
        if "[REC]" not in text or "samples/s" not in text:
            continue
        rate_marker = "rate="
        rate_index = text.find(rate_marker)
        if rate_index < 0:
            continue
        rate_chunk = text[rate_index + len(rate_marker):].split("samples/s", 1)[0].strip()
        try:
            rate_value = float(rate_chunk)
        except ValueError:
            continue
        return {"samples_per_second": rate_value, "line": text}
    return None


def aggregate_runtime_rate(nodes: list[dict[str, Any]]) -> dict[str, float | None]:
    instant_values = [
        float(node.get("instant_samples_per_second_5s"))
        for node in nodes
        if node.get("instant_samples_per_second_5s") is not None
    ]
    stability_values = [
        float(node.get("rate_stability_percent_5s"))
        for node in nodes
        if node.get("rate_stability_percent_5s") is not None
    ]
    return {
        "instant_samples_per_second_5s": sum(instant_values) if instant_values else None,
        "rate_stability_percent_5s": (sum(stability_values) / len(stability_values)) if stability_values else None,
    }


def clamp_limit(raw_value: str | None, default: int) -> int:
    try:
        parsed = int(raw_value) if raw_value is not None else default
    except ValueError:
        parsed = default
    return max(1, min(MAX_EVENT_LIMIT, parsed))


def clamp_live_limit(raw_value: str | int | None, default: int = DEFAULT_LIVE_PREVIEW_LIMIT) -> int:
    try:
        parsed = int(raw_value) if raw_value is not None else default
    except (TypeError, ValueError):
        parsed = default
    return max(1, min(MAX_LIVE_PREVIEW_LIMIT, parsed))


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


def add_seconds_to_iso(value: str | None, seconds: float) -> str | None:
    base = parse_iso8601(value)
    if base is None:
        return None
    try:
        return (base + timedelta(seconds=seconds)).isoformat()
    except OverflowError:
        return None


def seconds_between_iso(start_value: str | None, end_value: str | None) -> float | None:
    start = parse_iso8601(start_value)
    end = parse_iso8601(end_value)
    if start is None or end is None:
        return None
    return max(0.0, (end - start).total_seconds())


def event_severity_counts(events: list[dict[str, Any]]) -> dict[str, int]:
    counts = {"info": 0, "warning": 0, "error": 0}
    for event in events:
        severity = str(event.get("severity", "info")).lower()
        if severity not in counts:
            counts[severity] = 0
        counts[severity] += 1
    return counts


def normalize_log_event(
    event: dict[str, Any],
    *,
    source: str,
    channel_name: str | None = None,
) -> dict[str, Any]:
    normalized = dict(event)
    normalized["source"] = source
    if channel_name and not normalized.get("channel_name"):
        normalized["channel_name"] = channel_name
    severity = str(normalized.get("severity", "info")).lower()
    normalized["severity"] = severity
    return normalized


def event_matches_channel(event: dict[str, Any], channel_name: str | None) -> bool:
    if not channel_name:
        return True
    raw_channel = event.get("channel_name")
    return isinstance(raw_channel, str) and raw_channel == channel_name


def is_dashboard_hidden_event(event: dict[str, Any]) -> bool:
    return str(event.get("event", "")).lower() in HIDDEN_DASHBOARD_EVENT_NAMES


def is_emptyish(value: Any) -> bool:
    return value is None or value == "" or value == [] or value == {}


def is_alert_event(event: dict[str, Any]) -> bool:
    severity = str(event.get("severity", "info")).lower()
    if severity in {"warning", "error"}:
        return True
    event_name = str(event.get("event", "")).lower()
    if event_name in ALERT_EVENT_NAMES:
        return True
    return any(token in event_name for token in ("warn", "error", "gap", "overflow", "overwrite", "loss"))


def summarize_event_fields(event: dict[str, Any]) -> str:
    preferred_keys = [
        "error",
        "reason",
        "expected_sample_seq",
        "received_sample_seq",
        "packet_seq",
        "destination",
        "port",
        "baud",
        "samples_written",
        "stop_reason",
        "window_start_utc",
    ]
    reserved = {"utc", "severity", "event", "channel_name", "node_id", "source"}
    parts: list[str] = []
    for key in preferred_keys:
        value = event.get(key)
        if is_emptyish(value):
            continue
        parts.append(f"{key}={value}")
    if len(parts) < 4:
        for key, value in event.items():
            if key in reserved or key in preferred_keys or is_emptyish(value):
                continue
            parts.append(f"{key}={value}")
            if len(parts) >= 4:
                break
    return " · ".join(parts[:4])


def make_alert_entry(event: dict[str, Any]) -> dict[str, Any]:
    alert = dict(event)
    alert["summary"] = summarize_event_fields(event)
    source = str(alert.get("source", "event"))
    source_label_map = {
        "supervisor_event": "supervisor",
        "channel_event": "kanał",
        "process_log": "proces",
    }
    alert["source_label"] = source_label_map.get(source, source)
    return alert


def parse_process_log_alert(line: str, *, channel_name: str) -> dict[str, Any] | None:
    text = line.strip()
    if not text:
        return None
    lowered = text.lower()
    metrics: dict[str, int] = {}
    for chunk in text.replace(",", " ").split():
        if "=" not in chunk:
            continue
        key, value = chunk.split("=", 1)
        if key not in {"no_data", "failed", "gaps", "sensor_loss", "rx_ovf", "pkt_ovf"}:
            continue
        try:
            parsed_value = int(value)
        except ValueError:
            continue
        if parsed_value > 0:
            metrics[key] = parsed_value

    severity = None
    event_name = None
    if "[error]" in lowered:
        severity = "error"
        event_name = "process_error"
    elif "[warn]" in lowered:
        severity = "warning"
        event_name = "process_warning"
    elif metrics:
        severity = "warning"
        event_name = "process_counters_attention"
    elif "overwrite" in lowered or "overflow" in lowered:
        severity = "warning"
        event_name = "process_overwrite_attention"

    if severity is None or event_name is None:
        return None

    return {
        "utc": None,
        "severity": severity,
        "event": event_name,
        "channel_name": channel_name,
        "source": "process_log",
        "source_label": "proces",
        "summary": text,
        "line": text,
        "metrics": metrics,
    }


class ChannelControlRepository:
    def __init__(self, config_path: str | Path) -> None:
        self.config_path = Path(config_path)
        self._remote_restart_lock = threading.Lock()

    def perform(self, channel_name: str, action: str) -> dict[str, Any]:
        normalized_action = action.strip().lower()
        if normalized_action not in {"start", "stop", "restart", "purge"}:
            raise ValueError(f"unsupported channel action '{action}'")
        config = HostSystemConfig.load(self.config_path)
        channel = next((item for item in config.channels if item.name == channel_name), None)
        if channel is None:
            raise FileNotFoundError(f"unknown channel '{channel_name}'")
        if normalized_action in {"start", "restart", "purge"} and not channel.enabled:
            raise ValueError(f"channel '{channel_name}' is disabled in config")

        runtime_dir = Path(config.supervisor.channel_runtime_dir)
        runtime_dir.mkdir(parents=True, exist_ok=True)
        command_file = runtime_dir / f"{channel_name}.command.json"
        payload = {
            "action": normalized_action,
            "channel_name": channel_name,
            "requested_utc": utc_now_iso(),
        }
        tmp_path = command_file.with_suffix(command_file.suffix + ".tmp")
        tmp_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        os.replace(tmp_path, command_file)
        return {
            "ok": True,
            "channel_name": channel_name,
            "action": normalized_action,
            "command_file": str(command_file),
            "requested_utc": payload["requested_utc"],
        }

    def perform_supervisor(self, action: str) -> dict[str, Any]:
        normalized_action = action.strip().lower()
        if normalized_action not in {"restart_all", "purge_all"}:
            raise ValueError(f"unsupported supervisor action '{action}'")
        config = HostSystemConfig.load(self.config_path)
        runtime_dir = Path(config.supervisor.channel_runtime_dir)
        runtime_dir.mkdir(parents=True, exist_ok=True)
        command_file = runtime_dir / "supervisor.command.json"
        payload = {
            "action": normalized_action,
            "requested_utc": utc_now_iso(),
        }
        tmp_path = command_file.with_suffix(command_file.suffix + ".tmp")
        tmp_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        os.replace(tmp_path, command_file)
        return {
            "ok": True,
            "action": normalized_action,
            "command_file": str(command_file),
            "requested_utc": payload["requested_utc"],
        }

    def restart_remote_node(self, channel_name: str, node_id: int) -> dict[str, Any]:
        config = HostSystemConfig.load(self.config_path)
        channel = next((item for item in config.channels if item.name == channel_name), None)
        if channel is None:
            raise FileNotFoundError(f"unknown channel '{channel_name}'")
        if not channel.enabled:
            raise ValueError(f"channel '{channel_name}' is disabled in config")
        node = next((item for item in channel.nodes if item.node_id == node_id), None)
        if node is None:
            raise FileNotFoundError(f"unknown node '{node_id}' in channel '{channel_name}'")
        if not node.enabled:
            raise ValueError(f"node '{node_id}' in channel '{channel_name}' is disabled in config")
        if not self._remote_restart_lock.acquire(blocking=False):
            raise ChannelControlConflictError("another firmware restart is already in progress")

        control_script = Path(__file__).resolve().parents[1] / "host_channel_control.py"
        command = [
            sys.executable,
            str(control_script),
            "--system-config",
            str(self.config_path.resolve()),
            "restart-remote",
            "--channel",
            channel_name,
            "--node",
            str(node_id),
        ]
        started_at = time.monotonic()
        try:
            result = subprocess.run(
                command,
                capture_output=True,
                text=True,
                timeout=60.0,
                check=False,
            )
        except subprocess.TimeoutExpired as exc:
            raise ChannelControlConflictError(
                f"firmware restart timed out for {channel_name}/node-{node_id}"
            ) from exc
        except OSError as exc:
            raise ChannelControlConflictError(
                f"could not start firmware restart helper: {exc}"
            ) from exc
        finally:
            self._remote_restart_lock.release()

        output = "\n".join(part.strip() for part in (result.stdout, result.stderr) if part.strip())
        duration_s = max(0.0, time.monotonic() - started_at)
        if result.returncode != 0:
            raise ChannelControlConflictError(
                output or f"firmware restart failed with exit code {result.returncode}"
            )

        requested_utc = utc_now_iso()
        JsonlEventWriter(config.supervisor.event_log).emit(
            "node_firmware_restarted",
            severity="warning",
            node_id=node_id,
            fields={
                "channel_name": channel_name,
                "message": "Zrestartowano firmware czujnika i ponownie uruchomiono recorder kanału.",
                "duration_s": duration_s,
                "detail": output,
            },
        )
        return {
            "ok": True,
            "channel_name": channel_name,
            "node_id": node_id,
            "action": "restart_firmware",
            "requested_utc": requested_utc,
            "duration_s": duration_s,
            "output": output,
        }


class ChannelControlConflictError(RuntimeError):
    pass


class LivePreviewConflictError(RuntimeError):
    pass


class LivePreviewLeaseManager:
    def __init__(self, lease_timeout_s: float = LIVE_PREVIEW_LEASE_TIMEOUT_S) -> None:
        self.lease_timeout_s = lease_timeout_s
        self._lock = threading.Lock()
        self._token: str | None = None
        self._client_id: str | None = None
        self._channel_name: str | None = None
        self._node_id: int | None = None
        self._expires_at_monotonic = 0.0

    def acquire(self, channel_name: str, node_id: int, client_id: str) -> str:
        now = time.monotonic()
        with self._lock:
            self._expire_if_needed(now)
            if self._token is not None and self._client_id != client_id:
                raise LivePreviewConflictError(
                    f"aktywny podgląd już działa dla {self._channel_name}/node-{self._node_id}"
                )
            self._token = secrets.token_urlsafe(18)
            self._client_id = client_id
            self._channel_name = channel_name
            self._node_id = node_id
            self._expires_at_monotonic = now + self.lease_timeout_s
            return self._token

    def touch(self, token: str, channel_name: str, node_id: int) -> None:
        now = time.monotonic()
        with self._lock:
            self._expire_if_needed(now)
            if (
                self._token != token
                or self._channel_name != channel_name
                or self._node_id != node_id
            ):
                raise LivePreviewConflictError("sesja podglądu wygasła albo została przejęta")
            self._expires_at_monotonic = now + self.lease_timeout_s

    def release(self, token: str | None) -> None:
        if not token:
            return
        with self._lock:
            if self._token == token:
                self._token = None
                self._client_id = None
                self._channel_name = None
                self._node_id = None
                self._expires_at_monotonic = 0.0

    def _expire_if_needed(self, now: float) -> None:
        if self._token is not None and now >= self._expires_at_monotonic:
            self._token = None
            self._client_id = None
            self._channel_name = None
            self._node_id = None
            self._expires_at_monotonic = 0.0


class DashboardRepository:
    def __init__(self, config_path: str | Path, default_event_limit: int = 40) -> None:
        self.config_path = Path(config_path)
        self.default_event_limit = default_event_limit
        self.data_repository = DataRepository(self.config_path)
        self.channel_controls = ChannelControlRepository(self.config_path)
        self.live_preview_leases = LivePreviewLeaseManager()
        self._live_file_cache: dict[str, tuple[float, list[Path]]] = {}
        self._live_file_bounds_cache: dict[str, tuple[float, tuple[datetime | None, datetime | None]]] = {}

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
        return [
            event
            for event in load_tail_jsonl(Path(config.supervisor.event_log), limit_value)
            if not is_dashboard_hidden_event(event)
        ]

    def dashboard_payload(self, limit: int | None = None) -> dict[str, Any]:
        config = self._system_config()
        limit_value = clamp_limit(str(limit) if limit is not None else None, self.default_event_limit)
        raw_status = load_json(Path(config.supervisor.status_file))
        events = [
            event
            for event in load_tail_jsonl(Path(config.supervisor.event_log), limit_value)
            if not is_dashboard_hidden_event(event)
        ]
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
            "status_age_s": overview["status_age_s"],
            "status_stale": overview["status_stale"],
            "storage_total_bytes": raw_status.get("storage_total_bytes") if raw_status else None,
            "storage_free_bytes": raw_status.get("storage_free_bytes") if raw_status else None,
            "storage_used_percent": raw_status.get("storage_used_percent") if raw_status else None,
            "storage_low": overview["storage_low"],
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
            "system_healthy": bool(supervisor["has_status"] and overview["attention_count"] == 0),
            "dashboard_version": DASHBOARD_VERSION,
            "generated_utc": dashboard["generated_utc"],
            "has_status": supervisor["has_status"],
            "channels_running": overview["channels_running"],
            "nodes_online": overview["nodes_online"],
            "nodes_receiving_samples": overview["nodes_receiving_samples"],
            "nodes_without_samples": overview["nodes_without_samples"],
            "attention_count": overview["attention_count"],
        }

    def logs_payload(self, limit: int | None = None, channel_name: str | None = None) -> dict[str, Any]:
        config = self._system_config()
        limit_value = clamp_limit(str(limit) if limit is not None else None, 12)
        raw_status = load_json(Path(config.supervisor.status_file))
        channels = self._build_channels(config, raw_status)
        supervisor_events_raw = load_tail_jsonl(Path(config.supervisor.event_log), max(limit_value * 3, limit_value))
        supervisor_events = [
            normalize_log_event(event, source="supervisor_event")
            for event in supervisor_events_raw
            if event_matches_channel(event, channel_name) and not is_dashboard_hidden_event(event)
        ]

        alerts = [make_alert_entry(event) for event in supervisor_events if is_alert_event(event)]
        channel_logs: list[dict[str, Any]] = []
        for channel in channels:
            name = str(channel.get("name"))
            if channel_name and name != channel_name:
                continue

            event_log_path = Path(str(channel.get("event_log"))) if channel.get("event_log") else None
            process_log_path = Path(str(channel.get("process_log"))) if channel.get("process_log") else None
            channel_events = []
            if event_log_path is not None:
                channel_events = [
                    normalize_log_event(event, source="channel_event", channel_name=name)
                    for event in load_tail_jsonl(event_log_path, limit_value)
                    if not is_dashboard_hidden_event(event)
                ]
            process_lines = tail_text(process_log_path, limit_value) if process_log_path is not None else []
            process_alerts = [
                alert
                for alert in (
                    parse_process_log_alert(line, channel_name=name)
                    for line in process_lines
                )
                if alert is not None
            ]
            alerts.extend(make_alert_entry(event) for event in channel_events if is_alert_event(event))
            alerts.extend(process_alerts)
            channel_logs.append(
                {
                    "name": name,
                    "label": channel.get("label"),
                    "running": bool(channel.get("running", False)),
                    "health": channel.get("health"),
                    "event_log": str(event_log_path) if event_log_path is not None else None,
                    "process_log": str(process_log_path) if process_log_path is not None else None,
                    "events": channel_events,
                    "process_lines": process_lines,
                    "alert_count": sum(1 for event in channel_events if is_alert_event(event)) + len(process_alerts),
                }
            )

        alerts.sort(
            key=lambda event: (
                parse_iso8601(str(event.get("utc"))) or datetime.min.replace(tzinfo=timezone.utc),
                str(event.get("channel_name") or ""),
                str(event.get("event") or ""),
            ),
            reverse=True,
        )

        return {
            "generated_utc": utc_now_iso(),
            "limit": limit_value,
            "channel_filter": channel_name,
            "supervisor_event_log": config.supervisor.event_log,
            "supervisor_events": supervisor_events,
            "alerts": alerts[: max(limit_value * 4, limit_value)],
            "channels": channel_logs,
        }

    def data_payload(self, raw_relative: str | None) -> dict[str, Any]:
        return self.data_repository.list(raw_relative)

    def data_search_payload(self, raw_query: str | None) -> dict[str, Any]:
        return self.data_repository.search(raw_query)

    def data_download(self, raw_relative: str | None) -> FileDownload:
        return self.data_repository.download(raw_relative)

    def data_download_bundle(self, raw_paths: list[str]) -> FileDownload:
        return self.data_repository.download_bundle(raw_paths)

    def channel_action(self, channel_name: str, action: str) -> dict[str, Any]:
        return self.channel_controls.perform(channel_name, action)

    def restart_node_firmware(self, channel_name: str, node_id: int) -> dict[str, Any]:
        return self.channel_controls.restart_remote_node(channel_name, node_id)

    def supervisor_action(self, action: str) -> dict[str, Any]:
        return self.channel_controls.perform_supervisor(action)

    def live_preview_acquire(
        self,
        channel_name: str,
        node_id: int,
        client_id: str,
        limit: int | None = None,
        *,
        file_mode: bool = False,
        selected_file: str | None = None,
        target_utc: str | None = None,
    ) -> dict[str, Any]:
        if not channel_name.strip():
            raise ValueError("missing channel_name")
        if not client_id.strip():
            raise ValueError("missing client_id")
        limit_value = clamp_live_limit(limit)
        _, _, channel, node = self._resolve_live_target(channel_name, node_id)
        self._resolve_live_file(channel)
        token = self.live_preview_leases.acquire(channel["name"], node["node_id"], client_id)
        try:
            snapshot = self.live_preview_data(
                channel["name"],
                node["node_id"],
                token=token,
                limit=limit_value,
                end_index=None,
                file_mode=file_mode,
                selected_file=selected_file,
                target_utc=target_utc,
            )["snapshot"]
        except Exception:
            self.live_preview_leases.release(token)
            raise
        return {
            "ok": True,
            "token": token,
            "lease_timeout_s": self.live_preview_leases.lease_timeout_s,
            "snapshot": snapshot,
        }

    def live_preview_data(
        self,
        channel_name: str,
        node_id: int,
        *,
        token: str,
        limit: int | None = None,
        end_index: int | None = None,
        file_mode: bool = False,
        selected_file: str | None = None,
        target_utc: str | None = None,
    ) -> dict[str, Any]:
        if not channel_name.strip():
            raise ValueError("missing channel_name")
        if not token.strip():
            raise ValueError("missing token")
        limit_value = clamp_live_limit(limit)
        self.live_preview_leases.touch(token, channel_name, node_id)
        config, raw_status, channel, node = self._resolve_live_target(channel_name, node_id)
        if file_mode:
            file_path, previous_file, next_file, active_file, file_index, file_count = self._resolve_live_file_context(
                config,
                channel,
                selected_file=selected_file,
                target_utc=target_utc,
            )
            return {
                "snapshot": self._read_live_file_snapshot(
                    config,
                    raw_status,
                    channel,
                    node,
                    file_path,
                    previous_file=previous_file,
                    next_file=next_file,
                    active_file=active_file,
                    file_index=file_index,
                    file_count=file_count,
                )
            }
        file_path = self._resolve_live_file(channel)
        return {
            "snapshot": self._read_live_snapshot(
                config,
                raw_status,
                channel,
                node,
                file_path,
                limit_value,
                end_index=end_index,
            )
        }

    def live_preview_release(self, token: str | None) -> dict[str, Any]:
        self.live_preview_leases.release(token)
        return {"ok": True}

    def _resolve_live_target(
        self,
        channel_name: str,
        node_id: int,
    ) -> tuple[HostSystemConfig, dict[str, Any] | None, dict[str, Any], dict[str, Any]]:
        config = self._system_config()
        raw_status = load_json(Path(config.supervisor.status_file))
        channels = self._build_channels(config, raw_status)
        for channel in channels:
            if channel["name"] != channel_name:
                continue
            for node in channel["nodes"]:
                if int(node["node_id"]) != int(node_id):
                    continue
                if not channel["running"]:
                    raise ValueError("kanał nie pracuje, więc podgląd live jest niedostępny")
                if not node["has_runtime"]:
                    raise ValueError("węzeł nie ma aktywnego runtime")
                return config, raw_status, channel, node
            raise ValueError(f"channel '{channel_name}' does not have node {node_id}")
        raise ValueError(f"unknown channel '{channel_name}'")

    def _resolve_live_file(self, channel: dict[str, Any]) -> Path:
        data_root = self.data_repository.root_path()
        candidates: list[Path] = []
        for raw_path in [channel.get("active_file"), channel.get("destination")]:
            if not isinstance(raw_path, str) or not raw_path.strip():
                continue
            source = Path(raw_path.strip())
            if source.is_absolute():
                candidates.append(source.resolve())
            else:
                candidates.append((Path.cwd() / source).resolve())
                candidates.append((data_root / source).resolve())

        for candidate in candidates:
            if candidate.exists() and candidate.is_file() and candidate.suffix.lower() in {".h5", ".hdf5"}:
                return candidate

        matching_files: list[Path] = []
        needle = channel["name"].casefold()
        if data_root.exists():
            for pattern in ("*.h5", "*.hdf5"):
                for candidate in data_root.rglob(pattern):
                    relative = candidate.relative_to(data_root).as_posix().casefold()
                    if needle in relative:
                        matching_files.append(candidate)
        if matching_files:
            matching_files.sort(key=lambda path: path.stat().st_mtime, reverse=True)
            return matching_files[0]
        raise FileNotFoundError(f"nie znaleziono aktywnego pliku HDF5 dla kanału {channel['name']}")

    def _channel_live_files(self, channel_name: str) -> list[Path]:
        now = time.monotonic()
        cached = self._live_file_cache.get(channel_name)
        if cached is not None and cached[0] > now:
            return list(cached[1])

        data_root = self.data_repository.root_path()
        channel_root = data_root / channel_name
        candidates: list[Path] = []
        patterns = ("*.h5", "*.hdf5")
        search_root = channel_root if channel_root.exists() else data_root
        for pattern in patterns:
            for candidate in search_root.rglob(pattern):
                if channel_root.exists():
                    candidates.append(candidate)
                else:
                    relative = candidate.relative_to(data_root).as_posix().casefold()
                    if f"{channel_name.casefold()}/" in relative or channel_name.casefold() in candidate.name.casefold():
                        candidates.append(candidate)
        files = sorted({path.resolve() for path in candidates}, key=lambda path: str(path))
        self._live_file_cache[channel_name] = (now + LIVE_PREVIEW_FILE_CACHE_TTL_S, files)
        return list(files)

    def _resolve_live_file_context(
        self,
        config: HostSystemConfig,
        channel: dict[str, Any],
        *,
        selected_file: str | None,
        target_utc: str | None,
    ) -> tuple[Path, Path | None, Path | None, Path, int, int]:
        active_file = self._resolve_live_file(channel)
        files = self._channel_live_files(channel["name"])
        if active_file not in files:
            files.append(active_file)
            files.sort(key=lambda path: str(path))
        file_path = active_file
        if target_utc:
            resolved = self._resolve_live_file_from_target_time(config, channel["name"], target_utc, files)
            if resolved is not None:
                file_path = resolved
        elif selected_file:
            candidate = Path(selected_file)
            if not candidate.is_absolute():
                candidate = self.data_repository.root_path() / candidate
            candidate = candidate.resolve()
            if candidate in files:
                file_path = candidate
            else:
                raise FileNotFoundError(f"nie znaleziono pliku podglądu: {selected_file}")

        index = files.index(file_path)
        previous_file = files[index - 1] if index > 0 else None
        next_file = files[index + 1] if index + 1 < len(files) else None
        return file_path, previous_file, next_file, active_file, index, len(files)

    def _resolve_live_file_from_target_time(
        self,
        config: HostSystemConfig,
        channel_name: str,
        target_utc: str,
        files: list[Path],
    ) -> Path | None:
        target_dt = parse_iso8601(target_utc)
        if target_dt is None:
            return None

        bounded_matches: list[tuple[datetime, Path]] = []
        for candidate in files:
            start_dt, end_dt = self._read_live_file_bounds(candidate)
            if start_dt is None:
                continue
            if end_dt is not None and start_dt <= target_dt < end_dt:
                return candidate
            if start_dt <= target_dt:
                bounded_matches.append((start_dt, candidate))
        if bounded_matches:
            bounded_matches.sort(key=lambda item: item[0])
            return bounded_matches[-1][1]

        expected_key = target_dt.strftime("%Y-%m-%d/%Y-%m-%d_%H-%M-%S")
        keyed_files = sorted(
            (
                (f"{path.parent.name}/{path.stem}", path)
                for path in files
            ),
            key=lambda item: item[0],
        )
        fallback: Path | None = None
        for key, path in keyed_files:
            if key <= expected_key:
                fallback = path
            else:
                break
        return fallback or (keyed_files[0][1] if keyed_files else None)

    def _downsample_values(self, values: list[float], sequences: list[int], max_points: int) -> tuple[list[float], list[int]]:
        if len(values) <= max_points:
            return values, sequences
        step = max(1, len(values) // max_points)
        sampled_values = values[::step]
        sampled_sequences = sequences[::step]
        if sampled_sequences[-1] != sequences[-1]:
            sampled_values.append(values[-1])
            sampled_sequences.append(sequences[-1])
        return sampled_values, sampled_sequences

    def _read_live_file_snapshot(
        self,
        config: HostSystemConfig,
        raw_status: dict[str, Any] | None,
        channel: dict[str, Any],
        node: dict[str, Any],
        file_path: Path,
        *,
        previous_file: Path | None,
        next_file: Path | None,
        active_file: Path,
        file_index: int,
        file_count: int,
    ) -> dict[str, Any]:
        try:
            import h5py  # type: ignore
        except ImportError as exc:
            raise RuntimeError("brakuje biblioteki h5py wymaganej do podglądu live") from exc

        try:
            try:
                handle = h5py.File(file_path, "r", locking=False)
            except TypeError:
                handle = h5py.File(file_path, "r")
            with handle:
                group = handle["nodes"][str(node["node_id"])]
                dataset = group["samples"]
                total_samples = int(dataset.shape[0])
                raw_window = dataset[0:total_samples]
                fft_start = max(0, total_samples - MAX_LIVE_PREVIEW_LIMIT)
                fft_window = dataset[fft_start:total_samples]
                output_odr_hz = float(group.attrs.get("output_odr_hz", node.get("output_odr_hz") or 0.0))
                sensor_odr_hz = float(group.attrs.get("sensor_odr_hz", node.get("sensor_odr_hz") or 0.0))
                range_g_attr = group.attrs.get("range_g")
                range_g = float(range_g_attr if range_g_attr is not None else (self._configured_range_g(config, channel["name"], node["node_id"]) or 2))
                accel_unit_raw = group.attrs.get("accel_unit", "m/s^2")
                accel_unit = accel_unit_raw.decode("utf-8") if isinstance(accel_unit_raw, bytes) else str(accel_unit_raw)
                capture_start_raw = (
                    handle.attrs.get("window_start_utc")
                    or handle.attrs.get("file_created_utc")
                    or handle.attrs.get("created_utc")
                )
                capture_end_raw = handle.attrs.get("window_end_utc")
                capture_start_utc = capture_start_raw.decode("utf-8") if isinstance(capture_start_raw, bytes) else (str(capture_start_raw) if capture_start_raw is not None else None)
                capture_end_utc = capture_end_raw.decode("utf-8") if isinstance(capture_end_raw, bytes) else (str(capture_end_raw) if capture_end_raw is not None else None)

                sample_seq = [int(value) for value in raw_window["sample_seq"].tolist()]
                x_values = [round(float(value), 6) for value in raw_window["x"].tolist()]
                y_values = [round(float(value), 6) for value in raw_window["y"].tolist()]
                z_values = [round(float(value), 6) for value in raw_window["z"].tolist()]
                chart_x, chart_seq = self._downsample_values(x_values, sample_seq, LIVE_PREVIEW_CHART_MAX_POINTS)
                chart_y, _ = self._downsample_values(y_values, sample_seq, LIVE_PREVIEW_CHART_MAX_POINTS)
                chart_z, _ = self._downsample_values(z_values, sample_seq, LIVE_PREVIEW_CHART_MAX_POINTS)
                fft_x = [round(float(value), 6) for value in fft_window["x"].tolist()]
                fft_y = [round(float(value), 6) for value in fft_window["y"].tolist()]
                fft_z = [round(float(value), 6) for value in fft_window["z"].tolist()]
        except KeyError as exc:
            raise ValueError(f"brakuje danych live dla node {node['node_id']} w pliku {file_path.name}") from exc
        except OSError as exc:
            raise RuntimeError(f"nie udało się odczytać pliku HDF5: {exc}") from exc

        node_label = node.get("name") or f"Node {node['node_id']}"
        channel_label = channel.get("label") or channel["name"]
        scale_abs_m_s2 = max(range_g * STANDARD_GRAVITY_M_S2, 1.0)
        configured_window_seconds = max(1, int(config.storage.window_seconds))
        metadata_window_seconds = seconds_between_iso(capture_start_utc, capture_end_utc)
        total_duration_s = total_samples / output_odr_hz if output_odr_hz > 0 else float(configured_window_seconds)
        current_end_utc = add_seconds_to_iso(capture_start_utc, total_samples / output_odr_hz) if output_odr_hz > 0 else capture_end_utc
        return {
            "generated_utc": utc_now_iso(),
            "system_name": config.system.name,
            "status_updated_utc": raw_status.get("updated_utc") if raw_status else None,
            "channel_name": channel["name"],
            "channel_label": channel_label,
            "node_id": int(node["node_id"]),
            "node_label": node_label,
            "file_path": str(file_path),
            "file_name": file_path.name,
            "file_index": file_index,
            "file_count": file_count,
            "previous_file_path": str(previous_file) if previous_file is not None else None,
            "next_file_path": str(next_file) if next_file is not None else None,
            "is_active_file": file_path == active_file,
            "sample_count": len(chart_seq),
            "raw_sample_count": total_samples,
            "total_samples": total_samples,
            "window_start_index": 0,
            "window_end_index": total_samples,
            "is_live_tail": file_path == active_file,
            "capture_start_utc": capture_start_utc,
            "capture_end_utc": capture_end_utc,
            "window_start_utc_estimated": capture_start_utc,
            "window_end_utc_estimated": current_end_utc,
            "file_window_seconds": metadata_window_seconds or float(configured_window_seconds),
            "total_duration_s": total_duration_s,
            "first_sample_seq": sample_seq[0] if sample_seq else None,
            "last_sample_seq": sample_seq[-1] if sample_seq else None,
            "output_odr_hz": output_odr_hz,
            "sensor_odr_hz": sensor_odr_hz,
            "range_g": range_g,
            "accel_unit": accel_unit,
            "scale_abs_m_s2": scale_abs_m_s2,
            "display_decimation": max(1, len(sample_seq) // max(1, len(chart_seq))) if sample_seq else 1,
            "fft_sample_count": len(fft_x),
            "x": chart_x,
            "y": chart_y,
            "z": chart_z,
            "sample_seq": chart_seq,
            "fft_x": fft_x,
            "fft_y": fft_y,
            "fft_z": fft_z,
        }

    def _read_live_snapshot(
        self,
        config: HostSystemConfig,
        raw_status: dict[str, Any] | None,
        channel: dict[str, Any],
        node: dict[str, Any],
        file_path: Path,
        limit: int,
        *,
        end_index: int | None,
    ) -> dict[str, Any]:
        try:
            import h5py  # type: ignore
        except ImportError as exc:
            raise RuntimeError("brakuje biblioteki h5py wymaganej do podglądu live") from exc

        try:
            try:
                handle = h5py.File(file_path, "r", locking=False)
            except TypeError:
                handle = h5py.File(file_path, "r")
            with handle:
                group = handle["nodes"][str(node["node_id"])]
                dataset = group["samples"]
                total_samples = int(dataset.shape[0])
                resolved_end_index = total_samples if end_index is None else max(0, min(total_samples, int(end_index)))
                start = max(0, resolved_end_index - limit)
                window = dataset[start:resolved_end_index]
                output_odr_hz = float(group.attrs.get("output_odr_hz", node.get("output_odr_hz") or 0.0))
                sensor_odr_hz = float(group.attrs.get("sensor_odr_hz", node.get("sensor_odr_hz") or 0.0))
                range_g_attr = group.attrs.get("range_g")
                range_g = float(range_g_attr if range_g_attr is not None else (self._configured_range_g(config, channel["name"], node["node_id"]) or 2))
                accel_unit_raw = group.attrs.get("accel_unit", "m/s^2")
                accel_unit = accel_unit_raw.decode("utf-8") if isinstance(accel_unit_raw, bytes) else str(accel_unit_raw)
                capture_start_raw = (
                    handle.attrs.get("window_start_utc")
                    or handle.attrs.get("file_created_utc")
                    or handle.attrs.get("created_utc")
                )
                capture_end_raw = handle.attrs.get("window_end_utc")
                capture_start_utc = capture_start_raw.decode("utf-8") if isinstance(capture_start_raw, bytes) else (str(capture_start_raw) if capture_start_raw is not None else None)
                capture_end_utc = capture_end_raw.decode("utf-8") if isinstance(capture_end_raw, bytes) else (str(capture_end_raw) if capture_end_raw is not None else None)

                sample_seq = [int(value) for value in window["sample_seq"].tolist()]
                x_values = [round(float(value), 6) for value in window["x"].tolist()]
                y_values = [round(float(value), 6) for value in window["y"].tolist()]
                z_values = [round(float(value), 6) for value in window["z"].tolist()]
        except KeyError as exc:
            raise ValueError(f"brakuje danych live dla node {node['node_id']} w pliku {file_path.name}") from exc
        except OSError as exc:
            raise RuntimeError(f"nie udało się odczytać aktywnego pliku HDF5: {exc}") from exc

        node_label = node.get("name") or f"Node {node['node_id']}"
        channel_label = channel.get("label") or channel["name"]
        scale_abs_m_s2 = max(range_g * STANDARD_GRAVITY_M_S2, 1.0)
        total_duration_s = total_samples / output_odr_hz if output_odr_hz > 0 else 0.0
        window_start_utc_estimated = add_seconds_to_iso(capture_start_utc, start / output_odr_hz) if output_odr_hz > 0 else None
        window_end_utc_estimated = add_seconds_to_iso(capture_start_utc, resolved_end_index / output_odr_hz) if output_odr_hz > 0 else None
        return {
            "generated_utc": utc_now_iso(),
            "system_name": config.system.name,
            "status_updated_utc": raw_status.get("updated_utc") if raw_status else None,
            "channel_name": channel["name"],
            "channel_label": channel_label,
            "node_id": int(node["node_id"]),
            "node_label": node_label,
            "file_path": str(file_path),
            "file_name": file_path.name,
            "sample_count": len(sample_seq),
            "total_samples": total_samples,
            "window_start_index": start,
            "window_end_index": resolved_end_index,
            "is_live_tail": resolved_end_index >= total_samples,
            "capture_start_utc": capture_start_utc,
            "capture_end_utc": capture_end_utc,
            "window_start_utc_estimated": window_start_utc_estimated,
            "window_end_utc_estimated": window_end_utc_estimated,
            "total_duration_s": total_duration_s,
            "first_sample_seq": sample_seq[0] if sample_seq else None,
            "last_sample_seq": sample_seq[-1] if sample_seq else None,
            "output_odr_hz": output_odr_hz,
            "sensor_odr_hz": sensor_odr_hz,
            "range_g": range_g,
            "accel_unit": accel_unit,
            "scale_abs_m_s2": scale_abs_m_s2,
            "x": x_values,
            "y": y_values,
            "z": z_values,
        }

    def _configured_range_g(self, config: HostSystemConfig, channel_name: str, node_id: int) -> int | None:
        for channel in config.channels:
            if channel.name != channel_name:
                continue
            for node in channel.nodes:
                if node.node_id == node_id:
                    return node.range_g
        return None

    def _read_live_file_bounds(self, file_path: Path) -> tuple[datetime | None, datetime | None]:
        cache_key = str(file_path.resolve())
        now = time.monotonic()
        cached = self._live_file_bounds_cache.get(cache_key)
        if cached is not None and cached[0] > now:
            return cached[1]

        start_dt: datetime | None = None
        end_dt: datetime | None = None
        try:
            import h5py  # type: ignore
        except ImportError:
            return None, None
        try:
            try:
                handle = h5py.File(file_path, "r", locking=False)
            except TypeError:
                handle = h5py.File(file_path, "r")
            with handle:
                start_raw = (
                    handle.attrs.get("window_start_utc")
                    or handle.attrs.get("file_created_utc")
                    or handle.attrs.get("created_utc")
                )
                end_raw = handle.attrs.get("window_end_utc")
                start_text = start_raw.decode("utf-8") if isinstance(start_raw, bytes) else (str(start_raw) if start_raw is not None else None)
                end_text = end_raw.decode("utf-8") if isinstance(end_raw, bytes) else (str(end_raw) if end_raw is not None else None)
                start_dt = parse_iso8601(start_text)
                end_dt = parse_iso8601(end_text)
        except OSError:
            start_dt = None
            end_dt = None
        self._live_file_bounds_cache[cache_key] = (
            now + LIVE_PREVIEW_FILE_CACHE_TTL_S,
            (start_dt, end_dt),
        )
        return start_dt, end_dt

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

    def _load_channel_status_nodes(self, status_file: str | None) -> dict[int, dict[str, Any]]:
        if not status_file:
            return {}
        payload = load_json(Path(status_file))
        if payload is None:
            return {}
        nodes: dict[int, dict[str, Any]] = {}
        for raw_node in payload.get("nodes", []):
            try:
                node_id = int(raw_node.get("node_id", 0))
            except (TypeError, ValueError):
                continue
            if node_id <= 0:
                continue
            nodes[node_id] = raw_node
        return nodes

    def _merge_channel(
        self,
        config_channel: Any,
        runtime_channel: dict[str, Any] | None,
        config: HostSystemConfig,
    ) -> dict[str, Any]:
        status_file = runtime_channel.get("status_file") if runtime_channel else str(
            Path(config.supervisor.channel_runtime_dir) / f"{config_channel.name}.status.json"
        )
        process_log = runtime_channel.get("process_log") if runtime_channel else str(
            Path(config.supervisor.channel_runtime_dir) / f"{config_channel.name}.process.log"
        )
        last_rate = load_last_samples_rate(Path(process_log)) if process_log else None
        runtime_nodes = {
            int(node.get("node_id", 0)): node
            for node in runtime_channel.get("nodes", [])
        } if runtime_channel else {}
        channel_status_nodes = self._load_channel_status_nodes(status_file)
        for node_id, channel_status_node in channel_status_nodes.items():
            runtime_node = runtime_nodes.get(node_id)
            if runtime_node is None:
                runtime_nodes[node_id] = channel_status_node
                continue
            if runtime_node.get("firmware_version") in (None, "") and channel_status_node.get("firmware_version"):
                runtime_node = dict(runtime_node)
                runtime_node["firmware_version"] = channel_status_node.get("firmware_version")
                runtime_nodes[node_id] = runtime_node

        nodes = [
            self._merge_node(config_node, runtime_nodes.pop(config_node.node_id, None))
            for config_node in config_channel.nodes
        ]
        for raw_node in runtime_nodes.values():
            nodes.append(self._runtime_only_node(raw_node))
        runtime_rate = aggregate_runtime_rate(nodes)

        attention_count = sum(1 for node in nodes if node["alerts"])
        running = bool(runtime_channel.get("running")) if runtime_channel else False
        enabled = bool(config_channel.enabled)
        desired_running = bool(runtime_channel.get("desired_running", enabled)) if runtime_channel else enabled
        online_nodes = sum(1 for node in nodes if node["online"])
        health = "disabled"
        if enabled:
            if not runtime_channel:
                health = "waiting"
            elif not desired_running:
                health = "stopped"
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
            "desired_running": desired_running,
            "control_state": runtime_channel.get("control_state") if runtime_channel else ("disabled" if not enabled else "waiting"),
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
            "process_log": process_log,
            "last_samples_per_second": last_rate.get("samples_per_second") if last_rate else None,
            "last_samples_rate_line": last_rate.get("line") if last_rate else None,
            "instant_samples_per_second_5s": runtime_rate["instant_samples_per_second_5s"],
            "rate_stability_percent_5s": runtime_rate["rate_stability_percent_5s"],
            "status_file": status_file,
            "event_log": runtime_channel.get("event_log") if runtime_channel else str(
                Path(config.supervisor.channel_runtime_dir) / f"{config_channel.name}.events.jsonl"
            ),
            "nodes": nodes,
        }

    def _runtime_only_channel(self, runtime_channel: dict[str, Any]) -> dict[str, Any]:
        process_log = runtime_channel.get("process_log")
        last_rate = load_last_samples_rate(Path(process_log)) if process_log else None
        status_file = runtime_channel.get("status_file")
        runtime_nodes = {
            int(node.get("node_id", 0)): node
            for node in runtime_channel.get("nodes", [])
        }
        channel_status_nodes = self._load_channel_status_nodes(status_file)
        for node_id, channel_status_node in channel_status_nodes.items():
            runtime_node = runtime_nodes.get(node_id)
            if runtime_node is None:
                runtime_nodes[node_id] = channel_status_node
                continue
            if runtime_node.get("firmware_version") in (None, "") and channel_status_node.get("firmware_version"):
                runtime_node = dict(runtime_node)
                runtime_node["firmware_version"] = channel_status_node.get("firmware_version")
                runtime_nodes[node_id] = runtime_node
        nodes = [self._runtime_only_node(raw_node) for raw_node in runtime_nodes.values()]
        runtime_rate = aggregate_runtime_rate(nodes)
        attention_count = sum(1 for node in nodes if node["alerts"])
        return {
            "name": str(runtime_channel.get("name", "unknown")),
            "label": runtime_channel.get("label"),
            "configured": False,
            "enabled": bool(runtime_channel.get("enabled", True)),
            "desired_running": bool(runtime_channel.get("desired_running", True)),
            "control_state": runtime_channel.get("control_state"),
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
            "process_log": process_log,
            "last_samples_per_second": last_rate.get("samples_per_second") if last_rate else None,
            "last_samples_rate_line": last_rate.get("line") if last_rate else None,
            "instant_samples_per_second_5s": runtime_rate["instant_samples_per_second_5s"],
            "rate_stability_percent_5s": runtime_rate["rate_stability_percent_5s"],
            "status_file": status_file,
            "event_log": runtime_channel.get("event_log"),
            "nodes": nodes,
        }

    def _merge_node(self, config_node: Any, runtime_node: dict[str, Any] | None) -> dict[str, Any]:
        name = config_node.name or (str(runtime_node.get("name")) if runtime_node and runtime_node.get("name") else None)
        online = bool(runtime_node.get("online", False)) if runtime_node else False
        sensor_odr_hz = int(runtime_node.get("sensor_odr_hz", 0)) if runtime_node else 0
        output_odr_hz = float(runtime_node.get("output_odr_hz", 0.0)) if runtime_node else 0.0
        instant_rate = (
            float(runtime_node.get("instant_samples_per_second_5s"))
            if runtime_node and runtime_node.get("instant_samples_per_second_5s") is not None
            else None
        )
        receiving_samples = None if instant_rate is None else instant_rate > 0.0
        sample_flow_state = str(runtime_node.get("sample_flow_state", "unknown")) if runtime_node else "unknown"
        if online and receiving_samples is False:
            sample_flow_state = "stalled"
        elif online and receiving_samples is True:
            sample_flow_state = "flowing"
        alerts: list[str] = []
        if config_node.enabled and runtime_node is None:
            alerts.append("brak runtime")
        elif config_node.enabled and not online:
            alerts.append("offline")
        if config_node.enabled and online and sample_flow_state == "stalled":
            alerts.append("brak próbek")
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
            "firmware_version": str(runtime_node.get("firmware_version")) if runtime_node and runtime_node.get("firmware_version") else None,
            "has_runtime": runtime_node is not None,
            "online": online,
            "sensor_odr_hz": sensor_odr_hz,
            "output_odr_hz": output_odr_hz,
            "samples_written": int(runtime_node.get("samples_written", 0)) if runtime_node else 0,
            "instant_samples_per_second_5s": instant_rate,
            "rate_stability_percent_5s": float(runtime_node.get("rate_stability_percent_5s")) if runtime_node and runtime_node.get("rate_stability_percent_5s") is not None else None,
            "receiving_samples": receiving_samples,
            "sample_flow_state": sample_flow_state,
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
        instant_rate = (
            float(runtime_node.get("instant_samples_per_second_5s"))
            if runtime_node.get("instant_samples_per_second_5s") is not None
            else None
        )
        receiving_samples = None if instant_rate is None else instant_rate > 0.0
        sample_flow_state = str(runtime_node.get("sample_flow_state", "unknown"))
        if runtime_node.get("online", False) and receiving_samples is False:
            sample_flow_state = "stalled"
        elif runtime_node.get("online", False) and receiving_samples is True:
            sample_flow_state = "flowing"
        alerts: list[str] = []
        if not runtime_node.get("online", False):
            alerts.append("offline")
        if runtime_node.get("online", False) and sample_flow_state == "stalled":
            alerts.append("brak próbek")
        return {
            "node_id": int(runtime_node.get("node_id", 0)),
            "name": runtime_node.get("name"),
            "configured": False,
            "enabled": bool(runtime_node.get("enabled", True)),
            "expected_odr_hz": None,
            "firmware_version": str(runtime_node.get("firmware_version")) if runtime_node.get("firmware_version") else None,
            "has_runtime": True,
            "online": bool(runtime_node.get("online", False)),
            "sensor_odr_hz": int(runtime_node.get("sensor_odr_hz", 0)),
            "output_odr_hz": float(runtime_node.get("output_odr_hz", 0.0)),
            "samples_written": int(runtime_node.get("samples_written", 0)),
            "instant_samples_per_second_5s": instant_rate,
            "rate_stability_percent_5s": float(runtime_node.get("rate_stability_percent_5s")) if runtime_node.get("rate_stability_percent_5s") is not None else None,
            "receiving_samples": receiving_samples,
            "sample_flow_state": sample_flow_state,
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
        enabled_nodes = [
            node
            for channel in channels
            if channel["enabled"]
            for node in channel["nodes"]
            if node["enabled"]
        ]
        nodes_total = len(nodes)
        nodes_enabled = len(enabled_nodes)
        nodes_online = sum(1 for node in enabled_nodes if node["online"])
        nodes_receiving_samples = sum(
            1 for node in enabled_nodes
            if node["receiving_samples"] is True
        )
        nodes_without_samples = sum(
            1 for node in enabled_nodes
            if node["online"] and node["receiving_samples"] is False
        )
        samples_written_total = sum(
            node["samples_written"] for node in enabled_nodes
        )
        gaps_detected_total = sum(
            node["gaps_detected"] for node in enabled_nodes
        )
        restart_count_total = sum(channel["restart_count"] for channel in channels)
        attention_count = sum(len(node["alerts"]) for node in enabled_nodes)
        severity_counts = event_severity_counts(events)
        updated = parse_iso8601(raw_status.get("updated_utc")) if raw_status else None
        age_seconds = None
        if updated is not None:
            age_seconds = max(0.0, (datetime.now(timezone.utc) - updated).total_seconds())
        status_stale = raw_status is not None and (age_seconds is None or age_seconds > 5.0)
        storage_total_bytes = int(raw_status.get("storage_total_bytes", 0)) if raw_status else 0
        storage_free_bytes = int(raw_status.get("storage_free_bytes", 0)) if raw_status else 0
        storage_free_percent = (
            100.0 * storage_free_bytes / storage_total_bytes
            if storage_total_bytes > 0
            else None
        )
        storage_low = storage_free_percent is not None and storage_free_percent < 15.0
        if status_stale:
            attention_count += 1
        if storage_low:
            attention_count += 1

        if raw_status is None:
            summary = "Konfiguracja dostępna, oczekiwanie na runtime z supervisora"
        elif status_stale:
            summary = f"Dane runtime są nieaktualne od {int(age_seconds or 0)} s"
        elif storage_low:
            summary = f"Mało wolnego miejsca na dane: {storage_free_percent:.1f}%"
        elif nodes_without_samples > 0:
            summary = f"{nodes_without_samples} węzeł/węzły są online, ale nie dostarczają próbek"
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
            "nodes_receiving_samples": nodes_receiving_samples,
            "nodes_without_samples": nodes_without_samples,
            "samples_written_total": samples_written_total,
            "gaps_detected_total": gaps_detected_total,
            "restart_count_total": restart_count_total,
            "attention_count": attention_count,
            "events_by_severity": severity_counts,
            "status_age_s": age_seconds,
            "status_stale": status_stale,
            "storage_free_percent": storage_free_percent,
            "storage_low": storage_low,
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
        try:
            parsed = urlparse(self.path)
            route = parsed.path
            query = parse_qs(parsed.query)

            if route == "/":
                self._write_response(INDEX_HTML.encode("utf-8"), "text/html; charset=utf-8")
                return

            if route == "/live":
                self._write_response(LIVE_PREVIEW_HTML.encode("utf-8"), "text/html; charset=utf-8")
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

            if route == "/api/logs":
                limit = clamp_limit(query.get("limit", [None])[0], 12)
                channel_name = query.get("channel", [None])[0]
                self._write_json(self.repository.logs_payload(limit=limit, channel_name=channel_name))
                return

            if route == "/api/config":
                self._write_json(self.repository.config_payload())
                return

            if route == "/api/health":
                self._write_json(self.repository.health_payload())
                return

            if route in {"/api/data", "/api/runs"}:
                self._write_json(self.repository.data_payload(query.get("path", [None])[0]))
                return

            if route == "/api/data/search":
                self._write_json(self.repository.data_search_payload(query.get("q", [None])[0]))
                return

            if route in {"/api/data/download", "/api/runs/download"}:
                download = self.repository.data_download(query.get("path", [None])[0])
                self._write_file_response(
                    download,
                    headers={"Content-Disposition": f'attachment; filename="{download.download_name}"'},
                )
                return

            if route == "/api/live/data":
                limit = clamp_live_limit(query.get("limit", [None])[0])
                channel_name = query.get("channel", [""])[0]
                node_id = int(query.get("node", ["0"])[0])
                token = query.get("token", [""])[0]
                file_mode = query.get("file_mode", ["0"])[0] in {"1", "true", "True"}
                selected_file = query.get("selected_file", [None])[0]
                target_utc = query.get("target_utc", [None])[0]
                raw_end_index = query.get("end_index", [None])[0]
                try:
                    end_index = int(raw_end_index) if raw_end_index not in {None, ""} else None
                except ValueError:
                    raise ValueError("end_index must be an integer")
                self._write_json(
                    self.repository.live_preview_data(
                        channel_name,
                        node_id,
                        token=token,
                        limit=limit,
                        end_index=end_index,
                        file_mode=file_mode,
                        selected_file=selected_file,
                        target_utc=target_utc,
                    )
                )
                return

            self._write_json({"error": "not found", "path": route}, status=HTTPStatus.NOT_FOUND)
        except ValueError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.BAD_REQUEST)
        except LivePreviewConflictError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.CONFLICT)
        except FileNotFoundError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.NOT_FOUND)
        except NotADirectoryError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.BAD_REQUEST)

    def do_POST(self) -> None:
        try:
            parsed = urlparse(self.path)
            route = parsed.path
            if route == "/api/live/acquire":
                payload = self._read_json_body()
                channel_name = str(payload.get("channel_name", ""))
                client_id = str(payload.get("client_id", ""))
                file_mode = bool(payload.get("file_mode", False))
                selected_file = str(payload.get("selected_file")) if payload.get("selected_file") is not None else None
                target_utc = str(payload.get("target_utc")) if payload.get("target_utc") is not None else None
                try:
                    node_id = int(payload.get("node_id", 0))
                except (TypeError, ValueError):
                    raise ValueError("node_id must be an integer")
                limit = clamp_live_limit(payload.get("limit"))
                self._write_json(
                    self.repository.live_preview_acquire(
                        channel_name,
                        node_id,
                        client_id,
                        limit=limit,
                        file_mode=file_mode,
                        selected_file=selected_file,
                        target_utc=target_utc,
                    )
                )
                return

            if route == "/api/live/release":
                payload = self._read_json_body()
                token = payload.get("token")
                if token is not None and not isinstance(token, str):
                    raise ValueError("token must be a string")
                self._write_json(self.repository.live_preview_release(token))
                return

            if route.startswith("/api/channels/"):
                parts = [part for part in route.split("/") if part]
                if (
                    len(parts) == 6
                    and parts[0] == "api"
                    and parts[1] == "channels"
                    and parts[3] == "nodes"
                    and parts[5] == "restart-firmware"
                ):
                    channel_name = unquote(parts[2])
                    try:
                        node_id = int(unquote(parts[4]))
                    except ValueError:
                        raise ValueError("node id must be an integer")
                    self._write_json(self.repository.restart_node_firmware(channel_name, node_id))
                    return
                if len(parts) == 4 and parts[0] == "api" and parts[1] == "channels":
                    channel_name = unquote(parts[2])
                    action = unquote(parts[3])
                    self._write_json(self.repository.channel_action(channel_name, action))
                    return

            if route.startswith("/api/supervisor/"):
                parts = [part for part in route.split("/") if part]
                if len(parts) == 3 and parts[0] == "api" and parts[1] == "supervisor":
                    action = unquote(parts[2])
                    self._write_json(self.repository.supervisor_action(action))
                    return

            if route in {"/api/data/download-bundle", "/api/runs/download-bundle"}:
                payload = self._read_json_body()
                raw_paths = payload.get("paths")
                if not isinstance(raw_paths, list) or not all(isinstance(item, str) for item in raw_paths):
                    self._write_json(
                        {"error": "request body must contain 'paths' as an array of strings"},
                        status=HTTPStatus.BAD_REQUEST,
                    )
                    return
                download = self.repository.data_download_bundle(raw_paths)
                self._write_file_response(
                    download,
                    headers={"Content-Disposition": f'attachment; filename="{download.download_name}"'},
                )
                return
            self._write_json({"error": "not found", "path": route}, status=HTTPStatus.NOT_FOUND)
        except ValueError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.BAD_REQUEST)
        except LivePreviewConflictError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.CONFLICT)
        except ChannelControlConflictError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.CONFLICT)
        except FileNotFoundError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.NOT_FOUND)
        except NotADirectoryError as exc:
            self._write_json({"error": str(exc)}, status=HTTPStatus.BAD_REQUEST)

    def _write_json(self, payload: Any, status: HTTPStatus = HTTPStatus.OK) -> None:
        encoded = json.dumps(payload, indent=2, sort_keys=True).encode("utf-8")
        self._write_response(encoded, "application/json; charset=utf-8", status=status)

    def _read_json_body(self) -> dict[str, Any]:
        content_length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(content_length) if content_length > 0 else b"{}"
        payload = json.loads(raw.decode("utf-8"))
        if not isinstance(payload, dict):
            raise ValueError("request body must be a JSON object")
        return payload

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

    def _write_file_response(
        self,
        download: FileDownload,
        *,
        status: HTTPStatus = HTTPStatus.OK,
        headers: dict[str, str] | None = None,
    ) -> None:
        try:
            self.send_response(status)
            self.send_header("Content-Type", download.media_type)
            self.send_header("Content-Length", str(download.path.stat().st_size))
            self.send_header("Cache-Control", "no-store")
            if headers:
                for key, value in headers.items():
                    self.send_header(key, value)
            self.end_headers()
            with download.path.open("rb") as handle:
                while True:
                    chunk = handle.read(64 * 1024)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
        finally:
            if download.cleanup_path is not None:
                download.cleanup_path.unlink(missing_ok=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Serve a web dashboard for the Sensor System host runtime")
    parser.add_argument("--config", default="host/system_config.json")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8090)
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
