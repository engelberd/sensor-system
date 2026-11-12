# rpi-sanok2 — natywne atrybuty HDF5

2026-11-11. Bieżący kontrakt: [Analysis HDF5 v3](ANALYSIS-HDF5-V3.md).
Zmiany przygotowano lokalnie, wdrożono wyłącznie na rpi-sanok2.

## Porównanie z plikiem referencyjnym

Przykład `line-acm0_2026-06-16_16-30-00.h5` odczytano bez zmiany zawartości.
Metadane nagrania były atrybutami pliku (kanał, host, port, baudrate,
czas okna, strefa), a ustawienia node atrybutami `/nodes/1` (ODR, range,
FIFO, filtry i offsety). Dane znajdowały się w tabelach samples/gaps/temperature.

Zastosowano natywne atrybuty także w bieżącym formacie, zachowując jeden node
na plik i pojedynczą tabelę raw `samples` z indywidualnymi czasami uint64.

## Wynik

Usunięto dataset `metadata_json`. Plik zawiera wyłącznie dataset `samples`
i grupy danych `events`, `telemetry`. Nie dodano grupy ani tabeli metadanych.
Pełne metadane są w nazwanych atrybutach głównych pliku, np.:

- `channel_name`, `hardware_id`, `boot_epoch`, `window_ordinal`;
- `configured_odr_hz`, `sample_count`, `first_sample_time_local`;
- `configuration_revision`, `configuration_range_g`, `configuration_offset_x`;
- `telemetry_record_count`, `telemetry_status`.

Teksty to natywne UTF-8, liczby uint64/int64/float64, listy nazw kolumn to
natywne tablice atrybutów. Opcjonalny brak wartości oznacza brak atrybutu,
nie zero. Konfiguracja ma prefiks `configuration_`. Samples zachowuje też
atrybuty `axis_unit=raw`, `time_unit=us`.

## Wdrożenie i odbiór

Wymieniono wyłącznie eksporter. Recorder PID 13604 nie zmienił się i przez
całe wdrożenie nagrywał B. A nadal faulted, host degraded. Trzy pliki przebudowano
atomowo z zachowanych WAL pod dotychczasowymi nazwami:

| Początek | Próbki | Telemetria |
| --- | ---: | ---: |
| 16:50 | 247 | 0 (krótkie okno przed pierwszym odczytem) |
| 16:51 | 4056 | 9 |
| 17:00 | 4730 | 10 |

Łącznie 9033 próbek, zero braków, jeden jawny ProcessingReset w pierwszym
oknie. Każdy plik ma 49 natywnych atrybutów i nie ma `metadata_json`.
Pełny odczyt i walidacja danych przeszły; lokalny przykład notebooka także
zastąpiono wersją v3.

Release kończy się `-1ad870ca750b-532f04fec012-7e4337688a7e`.
Testy: 468 Rust, 186 host Python, 13 narzędzi Python; Clippy poprawne.
Regresje obejmują brak datasetu JSON, odczyt konfiguracji z atrybutów,
ujemne offsety, uint64 do 2^64−1, listy tekstowe i nieznane wartości opcjonalne.
