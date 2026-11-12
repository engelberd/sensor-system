# rpi-sanok2 — jedna tabela events w Analysis HDF5 v5

Data wdrożenia: 2026-11-12. Zakres: wyłącznie `rpi-sanok2`.

## Wynik

- `/events` jest jednym compound datasetem z polami `row_index` uint64,
  `source_sample_number` uint64, `event` UTF-8, `count` uint64 i `reason` UTF-8;
- `/samples` i `/telemetry` pozostają pojedynczymi tabelami bez zmiany pól;
- schema produktu to `sensor-system-analysis-hdf5-v5`;
- 141 istniejących produktów odtworzono atomowo z Capture WAL pod tymi samymi
  nazwami; kolejny produkt utworzył się automatycznie przez aktualny eksporter;
- walidacja 142 plików potwierdziła wyłącznie schema v5 i poprawny dtype events;
- pełna walidacja po migracji objęła 14 645 528 próbek, 0 brakujących próbek
  i 2 zachowane continuity break;
- `sensor-host-v2` zachował PID 13604 przez całe wdrożenie, a linia B nadal
  zapisywała próbki bez błędów wire, timeoutów i błędów storage;
- timer `sensor-analysis-export-v2.timer` jest aktywny.

Przykład `line-b_2026-11-12_15-20.h5` ma dokładnie trzy obiekty najwyższego
poziomu: datasety `events`, `samples`, `telemetry`. Puste `events` jest tabelą
zero-wierszową z zachowanym pełnym dtype, a nie grupą pustych kolumn.

## Weryfikacja

- 14 testów narzędzi Analysis Python;
- 4 testy eksportera Rust;
- 186 testów hosta legacy wymaganych przez repozytorium;
- `cargo fmt --check`, Clippy workspace z `-D warnings`, `sgit diff --check`;
- `verify_analysis_directory.py var/analysis` na rpi-sanok2.

`hostctl doctor` potwierdził porty, storage, usługę i synchronizację zegara,
ale zgłosił zastaną rozbieżność fingerprintu konfiguracji między działającym
procesem a aktualnym plikiem profilu. Proces recordera nie był restartowany,
żeby wdrożenie samego eksportera nie przerywało linii B.
