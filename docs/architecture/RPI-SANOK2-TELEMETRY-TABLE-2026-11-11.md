# rpi-sanok2 — jedna tabela telemetrii

2026-11-11. Kontrakt: [Analysis HDF5 v4](ANALYSIS-HDF5-V4.md).
Zmiany lokalne wdrożono wyłącznie na rpi-sanok2.

`/telemetry` jest jednym datasetem HDF5 typu compound:

| Pole | Typ | Znaczenie |
| --- | --- | --- |
| time_us | uint64 | oryginalny monotoniczny czas node w µs |
| temperature_c | float64 | temperatura wewnętrzna sensora w °C |

Usunięto raw kod temperatury i czas od pierwszej próbki. Dataset ma natywne
atrybuty `time_unit=us`, `temperature_unit=degC`. Pusta telemetria pozostaje
tą samą tabelą z zerową liczbą wierszy. Samples raw, metadane w atrybutach,
nazwy plików, ścieżki i polityka odpytania nie zmieniły się.

Trzy istniejące HDF5 przebudowano atomowo z zachowanych WAL, pod tymi samymi
nazwami. Łącznie zachowano 9033 próbek i 19 temperatur (0, 9 i 10 w kolejnych
plikach 16:50, 16:51, 17:00). Wszystkie pliki przeszły walidację dtype, czasu,
zakresów temperatur, liczników i danych raw. Brak missing samples; jeden
historyczny ProcessingReset w pierwszym oknie.

Recorder PID 13604 nie zmienił się, B cały czas recording. Wymieniono wyłącznie
eksporter; zwykły release kończy się `-1ad870ca750b-19919cb41285-7e4337688a7e`.
Lokalny przykład oraz notebook zaktualizowano do dwóch nazwanych pól telemetrii.

Testy: 468 Rust workspace, 186 host Python, 14 narzędzi Python — zaliczone.
Clippy workspace/all-targets, format i diff check poprawne. Regresje obejmują
strukturę pojedynczego datasetu i dokładne dwie kolumny, uint64 powyżej 2^53,
Celsius bez raw, puste okno, granice czasu, niefizyczną/NaN temperaturę oraz
bezstratny zapis większy niż jedna partia 8192 rekordów.
