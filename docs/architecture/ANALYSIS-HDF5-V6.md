# Analysis HDF5 v6

Od 2026-11-12 samodzielny produkt Analysis zawiera próbki, zdarzenia,
telemetrię i surowe obserwacje TimeSync w jednym pliku. V6 rozszerza v5 o
dataset `time_sync`; pozostałe tabele i czytelne nazwy plików nie zmieniają się.

Schema: `sensor-system-analysis-hdf5-v6` w atrybucie `schema` pliku.

## Zawartość

| Ścieżka | Typ / znaczenie |
| --- | --- |
| `/samples` | jedna tabela compound: `sample_sequence` uint64, `czas_us` uint64, `x`, `y`, `z` int32 raw |
| `/events` | jedna tabela compound: `row_index`, `source_sample_number`, `event`, `count`, `reason` |
| `/telemetry` | jedna tabela compound: `time_us` uint64, `temperature_c` float64 |
| `/time_sync` | jedna tabela compound z identyfikatorem zegara hosta, surowymi `t1..t4`, opóźnieniem ścieżki i niepewnością |
| atrybuty pliku | identyfikacja strumienia, konfiguracja, ODR, granice czasu, statusy i liczby rekordów |

Metadane są natywnymi atrybutami HDF5. Pełna konfiguracja ma prefiks
`configuration_`. Nie ma tabeli JSON ani osobnego pliku metadanych.

## Tabela `time_sync`

| Pole | Typ | Domena |
| --- | --- | --- |
| `host_clock_id` | stały ASCII[32] | hex identyfikatora bootu Linuksa, bez myślników |
| `t1_host_us` | uint64 | host `CLOCK_BOOTTIME`, kompletny request opuścił transmitter |
| `t2_node_us` | uint64 | node przyjął kompletny request |
| `t3_node_us` | uint64 | node rozpoczął response |
| `t4_host_us` | uint64 | host przyjął kompletny response |
| `path_delay_us` | uint64 | `(t4-t1) - (t3-t2)` |
| `uncertainty_us` | uint64 | `ceil(path_delay_us / 2)` |

`t1/t4` wolno porównywać wyłącznie w obrębie tego samego `host_clock_id`.
`t2/t3` należą do monotonicznego zegara RP2350 wskazanego przez atrybut
`boot_epoch`. Restart hosta nie tworzy nowej epoki node, dlatego identyfikator
zegara hosta występuje w każdym wierszu, a jeden HDF5 może zawierać więcej niż
jedną domenę hosta.

Eksporter zachowuje wszystkie obserwacje, których node midpoint leży w zakresie
próbek okna, oraz do dwóch obserwacji przed i po tym zakresie dla każdego
zegara hosta. Punkty brzegowe pozwalają później interpolować albo jawnie
ekstrapolować model bez otwierania Time WAL. Nie zapisujemy tylko gotowego
współczynnika: surowe dowody `t1..t4` pozostają dostępne do ponownego fitowania.

Gdy nie ma pasującego Time WAL lub obserwacji, `/time_sync` jest pustą tabelą o
pełnym dtype, `time_sync_record_count=0`, a
`time_sync_status=no_observations_available`. Brak nie jest zastępowany zerami.
Uszkodzony istniejący Time WAL albo niezgodna tożsamość zatrzymują eksport
danego okna fail-closed.

## Czas próbek i temperatura

`samples.czas_us` pozostaje oryginalnym czasem monotonicznym node związanym z
DRDY/FIR center sample. Nie jest interpolowany z TimeSync. UTC pierwszej próbki
i jego niepewność pozostają widokiem w atrybutach; `time_sync` dostarcza dane
wejściowe do dokładniejszego modelu offline.

`telemetry.time_us` używa tej samej domeny node co próbki, a
`temperature_c` pozostaje temperaturą wewnętrzną ADXL355 w stopniach Celsjusza.
Pusta telemetria ma jawny status `no_observations_in_window`.

## Publikacja i migracja

Produkty są grupowane według lokalnej daty pierwszej próbki i channel:
`analysis_v2.root_dir/YYYY-MM-DD/CHANNEL/`. Pełna przykładowa ścieżka to
`var/analysis/2026-11-13/line-b/line-b_2026-11-13_16-38.h5`. Nazwy plików
pozostają w formacie `CHANNEL_YYYY-MM-DD_HH-MM.h5`. Tożsamość produktu pochodzi
z atrybutów, nie z nazwy. Eksporter używa blokady katalogu channel, ukrytego
pliku tymczasowego, `fsync` i atomowego `rename`.

V1–v5 są formatami wycofanymi. Indeks eksportera rozpoznaje ich tożsamość, ale
nie uznaje ich za ukończony produkt bieżący. `./hostctl analysis export-new
--backfill-all` odtwarza v6 z zachowanych Capture, Time i Telemetry WAL pod tą
samą czytelną nazwą. Recorder nie musi być zatrzymany.

Istniejące płaskie produkty v6 można przenieść bez przebudowy danych:

```text
host/.venv/bin/python host/runtime/analysis/reorganize_analysis_layout.py var/analysis
host/.venv/bin/python host/runtime/analysis/reorganize_analysis_layout.py var/analysis --apply
```

Pierwsze polecenie jest dry-run. Migrator wyznacza datę i channel z atrybutów
HDF5, nie nadpisuje istniejącego celu i przenosi plik atomowo w obrębie tego
samego filesystemu.

## Odczyt

Python:

```text
host/.venv/bin/python host/runtime/analysis/load_analysis_window.py PLIK.h5
```

Loader zachowuje wszystkie czasy jako `uint64`, sprawdza kolejność `t1..t4`,
przelicza `path_delay_us` i `uncertainty_us` oraz odrzuca niezgodne rekordy.
Nie należy konwertować bezwzględnych mikrosekund do `float64` przed odjęciem
lokalnej kotwicy.

MATLAB odczytuje tabele przez `h5read(path, '/samples')`,
`h5read(path, '/telemetry')` i `h5read(path, '/time_sync')`.
