# Analysis HDF5 v5 — format wycofany

Bieżący kontrakt: [Analysis HDF5 v6](ANALYSIS-HDF5-V6.md). V6 dodaje jedną
tabelę `time_sync` z surowymi obserwacjami `t1..t4`; próbki, zdarzenia i
telemetria pozostają zgodne znaczeniowo.

Od 2026-11-11 jedynym automatycznym produktem analitycznym jest samodzielny
plik `.h5` na zapieczętowane okno Capture. Zastępuje Analysis CSV v2/v3 wraz
z towarzyszącymi JSON, events i telemetry. Decyzja: [ADR-0014](../adr/0014-analysis-hdf5.md).

## Zawartość

Schema: `sensor-system-analysis-hdf5-v5` w atrybucie `schema` pliku.

| Ścieżka | Typ / znaczenie |
| --- | --- |
| `/samples` | jedna tabela HDF5 (compound): `sample_sequence` uint64, `czas_us` uint64, `x`, `y`, `z` int32 raw |
| `/events` | jedna tabela HDF5 (compound): `row_index` uint64, `source_sample_number` uint64, `event` UTF-8, `count` uint64, `reason` UTF-8 |
| `/telemetry` | jedna tabela HDF5 (compound): `time_us` uint64, `temperature_c` float64 (°C) |
| atrybuty główne pliku | natywne metadane: identyfikacja, konfiguracja, ODR, jednostki raw, zakresy, kotwica UTC, niepewność i metoda czasu |

Metadane nie są datasetem ani tekstem JSON. Nagłówek pliku zawiera nazwane
atrybuty HDF5, np. `channel_name`, `hardware_id`, `boot_epoch`, `window_ordinal`,
`configured_odr_hz`, `first_sample_time_utc`, `sample_count` i `telemetry_status`.
Pełna konfiguracja ma prefiks `configuration_`: `configuration_revision`,
`configuration_range_g`, `configuration_offset_x` itd. Bez dodatkowej grupy
lub tabeli metadanych. Liczby są uint64/int64/float64, teksty UTF-8, listy nazw
kolumn natywnymi tablicami atrybutów. Nieznane wartości opcjonalne (np.
niepewność czasu) nie mają atrybutu; brak nie oznacza zera.
Dataset `samples` ma też własne atrybuty `axis_unit=raw` i `time_unit=us`.
Dataset `telemetry` ma `time_unit=us`, `temperature_unit=degC`.
W telemetrii nie przechowujemy raw kodu temperatury ani czasu względem
pierwszej próbki; `time_us` jest oryginalnym czasem monotonicznym node.

`samples`, `events` i `telemetry` są trzema datasetami o nazwanych polach.
Nie zapisujemy wartości m/s² ani drugiej kopii osi. Pusta tabela events oraz
pusta tabela telemetry mają zero wierszy i zachowują dokładnie swój dtype.
Temperatura obejmuje zamknięty
zakres czasu pierwszej i ostatniej próbki; brak odczytów nie oznacza błędu.
UTC jest opisane kotwicą pierwszej próbki i jej niepewnością; nie dopisujemy
pozornie dokładnych, niezależnych timestampów UTC do każdej próbki.

Dane liczbowe używają bezstratnego gzip, shuffle i Fletcher32. Tabela zdarzeń
używa UTF-8, gzip i shuffle (HDF5 nie obsługuje Fletcher32 dla zmiennych tekstów).
Eksporter przetwarza dane w ograniczonych partiach 8192 rekordów. Rust waliduje
WAL i tworzy tymczasowe strumienie JSONL; wbudowany w binarium skrypt Python
z h5py zamienia je w HDF5. To proces offline, poza recorderem i RS485.

## Nazwy i publikacja

Wszystkie wyniki leżą bezpośrednio w `var/analysis/` (lub skonfigurowanym
`analysis_v2.root_dir`), bez katalogów dni i kanałów.
Format: `line-b_2026-11-11_16-38.h5`, czyli
`CHANNEL_YYYY-MM-DD_HH-MM.h5`. Czas to pierwsza próbka,
w strefie instalacji, z zamrożonego kalendarza WAL.
Hardware ID, epoka i ordinal pozostają wyłącznie wewnątrz HDF5. Eksporter
odczytuje ten zestaw z atrybutów pliku i według niego rozpoznaje ukończone okna.
Jeżeli dwa różne okna zaczynają się w tej samej minucie (restart lub zmiana
konfiguracji), kolejne dostaje minimalny sufiks `_2`, `_3`, …; żadne dane nie
są nadpisywane. Ponowienie tego samego okna rozpoznaje też plik z sufiksem.

Eksporter blokuje katalog przez flock. Zapisuje ukryty plik tymczasowy,
zamyka HDF5, wykonuje fsync i atomowy rename, a następnie fsync katalogu.
Wyłącznie finalny `.h5` oznacza ukończony produkt. Ponowienie nie duplikuje
okna; przerwany zapis jest sprzątany przy kolejnej próbie tego samego okna.
Capture i jego retencja nie zależą od formatu Analysis.

`./hostctl analysis export-new` obejmuje wszystkie znane epoki włączonych node.
Pierwszy przebieg ma `initial_backfill_windows` na epokę; kolejne uzupełniają
luki od pierwszego istniejącego HDF5. `--backfill-all` obejmuje wszystkie
zapieczętowane okna. Odrzucone okna raportują przyczynę i niezerowy exit status;
nie blokują eksportowania pozostałych. Mieszane konfiguracje i brak wymaganych
dowodów nadal powodują jawne odrzucenie okna.

## Odczyt i wdrożenie

Python: `host/.venv/bin/python host/runtime/analysis/load_analysis_window.py PLIK.h5`.
Notebook: `host/runtime/analysis/analysis_window.ipynb`.
MATLAB: `samples = h5read(path, '/samples')`; pola `samples.czas_us`,
`samples.sample_sequence`, `samples.x`, `samples.y`, `samples.z`;
metadane: `h5readatt(path, '/', 'configured_odr_hz')`,
`h5readatt(path, '/', 'configuration_range_g')` itd.
Zachowaj uint64 do chwili odjęcia kotwicy; float64 nie przechowuje dokładnie
wszystkich czasów powyżej 2^53 µs. Loader udostępnia dokładne czasy temperatury
osobno w `telemetry_device_time_us`.

Wymagane `numpy` i `h5py` w interpreterze `SENSOR_ANALYSIS_PYTHON` (domyślnie
`python3`). Instalator systemd sprawdza zależności i wpisuje jawny interpreter
`--python` do usługi eksportera. Wdrożenie tej zmiany ograniczamy do rpi-sanok2;
pozostałe hosty nie są aktualizowane. Starsze produkty CSV można usunąć;
nie usuwamy Capture WAL ani sprzętowych plików CSV/BOM.


## Telemetria i migracja do v4

`telemetry_status` ma wartość `present` albo `no_observations_in_window`.
Puste telemetry oznacza, że żaden zapisany snapshot nie ma czasu wewnątrz
zakresu próbek tego okna. Nie zastępujemy go odczytem z przyszłości ani zerami.
Pierwsze odpytanie po starcie jest planowane po pełnym interwale (na rpi-sanok2
60 s); krótkie okno zakończone wcześniej może więc nie mieć temperatury.
Następne okna zawierają rzeczywiste obserwacje z Telemetry WAL.

Poprzedni v2 zawierał raw samples, ale metadane w dataset `metadata_json`.
V3 przenosi je do atrybutów. V1 miał dodatkowo grupę kolumn samples i m/s².
V3 zachował telemetrię jako grupę czterech kolumn; v4 zapisuje jedną tabelę
z czasem i temperaturą °C. V5 zapisuje także events jako jedną tabelę zamiast
grupy pięciu osobnych kolumn. Czytnik obsługuje wyłącznie bieżący v5.
Indeks eksportera traktuje v1/v2/v3/v4 jako
produkty wymagające odtworzenia: `--backfill-all` przebudowuje je z zachowanych
WAL atomowo pod tymi samymi nazwami. Nie trzeba zatrzymywać recordera.
