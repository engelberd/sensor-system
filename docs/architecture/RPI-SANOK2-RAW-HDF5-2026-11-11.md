# rpi-sanok2 — jedna tabela raw i telemetria w HDF5

2026-11-11. Zmiany przygotowano lokalnie i wdrożono wyłącznie na rpi-sanok2.
Bieżący kontrakt: [Analysis HDF5 v2](ANALYSIS-HDF5-V2.md).

## Samples

`/samples` jest pojedynczym datasetem HDF5 typu compound o N wierszach:

| Pole | Typ | Znaczenie |
| --- | --- | --- |
| sample_sequence | uint64 | oryginalna sekwencja |
| czas_us | uint64 | indywidualny czas node w µs |
| x | int32 | surowa oś X |
| y | int32 | surowa oś Y |
| z | int32 | surowa oś Z |

Usunięto kolumny m/s² i ich przeliczanie w eksporterze. Metadane nadal zawierają
pełną konfigurację, ODR, zakres, identyfikację i kotwicę czasu. Nazwy plików
oraz płaski katalog `var/analysis` pozostają bez zmian.
Dwa istniejące pliki v1 atomowo odtworzono z WAL jako v2 pod tymi samymi nazwami.
Nie usuwano pomiarów, nie restartowano recordera (PID pozostał 13604).

## Dlaczego w pierwszym pliku nie było telemetrii

`line-b_node-1_16-50_11-11-2026.h5` jest krótkim oknem testowym: 247 próbek,
około 31 sekund, zakończonym przed pierwszym planowanym zapytaniem Telemetry.
Profil node B ma interwał 60 s, a pierwsze odpytanie następuje po pełnym
interwale od startu. Nie było więc snapshotu do umieszczenia w tym oknie.
Telemetry WAL i Runtime Status potwierdziły, że dalsze odczyty działają;
przy diagnozie odczytano około 27.65 °C.

Kolejny plik `line-b_node-1_16-51_11-11-2026.h5` zawiera 4056 próbek i dziewięć
rzeczywistych odczytów temperatury (około 27.54–27.76 °C), bez brakujących próbek.
W obu plikach tabela raw i pozostałe metadane przeszły weryfikację.

Metadane jawnie rozróżniają `telemetry_status=present` oraz
`no_observations_in_window`. Nie podstawiamy przyszłych odczytów temperatury
do zakończonego wcześniej okna. Polityka interwału nie została zmieniona.
Lokalny przykład notebooka zastąpiono dłuższym oknem z telemetrią.

## Wdrożenie i weryfikacja

Zbudowano i wymieniono wyłącznie eksporter. Zwykły instalator zachował binarium
recordera SHA-256 `1ad870ca750b9e73c0f9bfc5f79b35e1fa32b35105d7dc2046d8cb81534eba47`.
Nowy release kończy się `-1ad870ca750b-f3ae39fc5d9a-7e4337688a7e`.
Recorder przez cały rollout nagrywał B; A nadal faulted, host degraded.

Testy: 468 Rust workspace, 186 host Python, 12 narzędzi Python; Clippy
workspace/all-targets i diff check poprawne. Dodatkowo test eksportera sprawdza
zgodność raw XYZ z oryginalnymi kodami WAL; testy Python sprawdzają jedną
tabelę o dokładnym dtype, precyzję powyżej 2^53 µs, pustą telemetrię krótkiego
okna, obecne odczyty oraz przebudowę v1 pod tą samą nazwą.
