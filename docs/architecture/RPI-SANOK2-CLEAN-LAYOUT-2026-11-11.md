# rpi-sanok2 — czysta struktura i proste nazwy HDF5

2026-11-11. Na wyraźne żądanie użytkownika usunięto stare pomiary na rpi-sanok2.
Nie jest to zwykły krok aktualizacji. Zmiany kodu przygotowano lokalnie;
wdrożono tylko tę instalację.

## Układ jak po nowej instalacji

Lokalny, ignorowany `host/system_config.json` zawiera wyłącznie
`{"extends":"configs/systems/rpi-sanok2.json"}`. Ścieżki wynikają z common:

```text
var/
  README.md
  recordings/  # Capture, Time, Telemetry, historia konfiguracji
  analysis/    # samodzielne pliki HDF5, bez podkatalogów dni/kanałów
  archive/
  diagnostics/
  log/
  tmp/
```

Koordynacja usług pozostaje w `/run/sensor-system`. Usunięto stare katalogi
`*-protocol6`, próby HIL, pomiary legacy, tymczasowe źródła i diagnostykę
w `var/tmp`, `var/run`, stary target/debug, node/build i nieśledzony plik
`tc --version`. Źródła, Git, hardware i środowisko Python usług zachowano.

Przy zatrzymanym recorderze i eksporterze usunięto 15 832 stare pliki danych
z `var` o sumie rozmiarów 6 534 594 854 B. Wolne miejsce po sprzątaniu wynosi
około 46 GiB. Stare Capture/Time/Telemetry i Analysis zostały skasowane;
wcześniejsze raporty o możliwości ich ponownego eksportowania są historyczne.

Wersjonowany szablon dodaje `var/analysis/.gitkeep`, usuwa `var/run/.gitkeep`
i zachowuje ignorowanie pomiarów. `setup_host.sh` instaluje teraz numpy i h5py
z `host/requirements-v2.txt`, więc zwykłe przygotowanie świeżego checkoutu
zapewnia zależności eksportera.

## Nazwy i odbiór

Wymagany format: `line-b_node-1_16-38_11-11-2026.h5`.
Identyfikatory hardware/epoka/ordinal pozostają w HDF5; idempotency odczytuje
atrybuty pliku, a nie jego nazwę. Jedynie kolizja dwóch różnych okien tej samej
minuty używa sufiksu `_2`, `_3`, …; plik nie jest nadpisywany.

Po standardowym `hostctl install-services` usługi uruchomiono z uprawnieniami
administratora (`sudo systemctl start sensor-host-v2 sensor-analysis-export-v2.timer`).
Nowe rzeczywiste okno:
`var/analysis/line-b_node-1_16-50_11-11-2026.h5`.

Walidacja: 247 próbek, zero missing samples, jeden jawny `ProcessingReset`.
Pełne metadane, raw XYZ, osie fizyczne, czas i struktura HDF5 przeszły kontrolę.
Ponowny eksport: zero nowych plików i zero błędów. Po kontrolowanym restarcie
B ponownie recording; odnotowano 465 nowych próbek i zero braków. A nadal
faulted, host degraded — zachowano izolację kanałów z ADR-0015.
`doctor`: 17 OK, dwa ostrzeżenia degraded i dwa błędy dotyczące wyłącznie A.

## Binaria i testy

- Recorder SHA-256: `1ad870ca750b9e73c0f9bfc5f79b35e1fa32b35105d7dc2046d8cb81534eba47`.
- Eksporter SHA-256: `3b4ff79a6421e31bb2429e4ef8838105df0180c67298de3e77296aae0c3d8da4`.
- Zwykły release: `sensor-host-0.5.0-dev-framing-3-application-6-1ad870ca750b-3b4ff79a6421-7e4337688a7e`.
- Szablony systemd wygenerowano normalnym instalatorem, bez tymczasowego
  drop-in HDF5. Zachowano adres dashboardu 100.88.203.10:8090.
- Rust workspace: 468 testów; host Python: 186; narzędzia Python: 10 — zaliczone.
- Clippy workspace/all-targets, format zmienionych plików i diff check: poprawne.

Testy obejmują dokładną nazwę, płaski katalog, kolizje dwóch okien w tej samej
minucie, ponowienie, identyfikację po metadanych niezależnie od nazwy,
indywidualne czasy uint64, luki oraz temperaturę.


## Późniejsze ostateczne nazwy

Nazwy zmieniono na `CHANNEL_YYYY-MM-DD_HH-MM.h5`, np.
`line-b_2026-11-11_16-51.h5`. Przemianowano cztery pliki (16:50, 16:51,
17:00, 17:10), sprawdzając SHA-256 wszystkich datasetów, pozostałe metadane
i inode. Łącznie 13 763 próbek, zero braków, jeden dawny ProcessingReset.
Recorder PID 13604 nie zmienił się; B recording przez całe wdrożenie.
Release kończy się `-1ad870ca750b-94fb31cf1d8a-7e4337688a7e`.
Schema v4 pozostaje bez zmian. Numer node pozostaje w atrybutach;
sufiks `_2` chroni przed kolizją różnych okien/node tej samej minuty.
Testy eksportera i indeksu, 186 host Python, 14 narzędzi Python oraz Clippy
przeszły. Zaktualizowano także lokalny przykład notebooka.
