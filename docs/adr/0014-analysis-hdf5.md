# ADR-0014: Jeden samodzielny HDF5 na okno Analysis

- Data: 2026-11-11
- Status: zaakceptowane na żądanie użytkownika

Analysis CSV rozdziela jedno okno na dane, zdarzenia, temperaturę i JSON.
Przenoszenie zestawu i dobieranie companion files utrudnia analizę.

Zastępujemy automatyczny Analysis CSV przez Analysis HDF5. Jeden plik
zawiera wszystkie potrzebne metadane, surowe XYZ i pełną
konfigurację pomiaru. Indywidualne czasy i sekwencje pozostają uint64.

Capture WAL, kalendarz okien, źródła czasu i semantyka luk nie zmieniają się.
Wdrożenie dotyczy na razie wyłącznie rpi-sanok2. Wycofujemy czytnik i publikację
Analysis CSV; dotychczasowe produkty można skasować bez usuwania Capture.
Diagnostyczny `capture-export` i sprzętowe BOM nie są produktami Analysis.

HDF5 powstaje offline przez h5py w jawnie wybranym interpreterze, z ograniczoną
pamięcią i atomowym rename. Binarium zawiera skrypt piszący, więc wdrożenie
samego eksportera nie wymaga dopasowywania osobnej wersji skryptu.

Kontrakt bieżący: [ANALYSIS-HDF5-V6](../architecture/ANALYSIS-HDF5-V6.md).
Decyzja zastępuje format Analysis z ADR-0012/0013; pozostałe decyzje tych ADR
pozostają w mocy.


Doprecyzowanie nazwy i układu na żądanie użytkownika 2026-11-11: wyniki
w płaskim `var/analysis/`, nazwa `line-b_node-1_16-38_11-11-2026.h5`.
Techniczne identyfikatory pozostają w metadanych; kolizje tej samej minuty
rozróżnia wyłącznie minimalny sufiks `_2`, `_3`, …, bez nadpisywania okna.
Zastępuje to dzienne katalogi Analysis z ADR-0013; kalendarz Capture nie zmienia się.


Kolejne doprecyzowanie użytkownika 2026-11-11: `samples` jest jedną tabelą
compound z `sample_sequence`, `czas_us`, `x`, `y`, `z`. Zapisujemy tylko raw.
Zmiana struktury wymaga schema HDF5 v2; dotychczasowe pliki można odtworzyć
atomowo z zachowanych WAL. Telemetry pozostaje osobnym strumieniem w tym samym
pliku, z jawną informacją o braku obserwacji w krótkim oknie.


Po porównaniu ze starym HDF5 użytkownik 2026-11-11 wymaga natywnych nagłówków.
Schema v3 usuwa `metadata_json`: wszystkie metadane są atrybutami HDF5,
konfiguracja ma prefiks `configuration_`. Tabele próbek, zdarzenia i telemetry
zachowują znaczenie. Nieznane wartości opcjonalne reprezentuje brak atrybutu.


Kolejne doprecyzowanie 2026-11-11: w schema v4 `telemetry` jest jedną tabelą
`time_us` uint64 i `temperature_c` float64. Usunięto raw kod temperatury i czas
względem pierwszej próbki. Znaczenie snapshotu i polityka odpytania pozostają
bez zmian; źródłowy Telemetry WAL nadal umożliwia odtworzenie danych.


Ostateczne doprecyzowanie nazwy 2026-11-11: `CHANNEL_YYYY-MM-DD_HH-MM.h5`,
np. `line-b_2026-11-11_16-20.h5`. Numer node pozostaje atrybutem pliku.
Kanał ma nazwę zgodną z profilem instalacji; nie przemianowujemy `line-b`
na historyczne `line-acm0`. Sufiks kolizji `_2` nadal chroni przed nadpisaniem
różnych node lub okien tej samej minuty. Schema danych nie zmienia się.


Doprecyzowanie użytkownika 2026-11-12: w schema v5 `events` jest jedną tabelą
compound z polami `row_index`, `source_sample_number`, `event`, `count`,
`reason`. Usunięto grupę pięciu osobnych datasetów. Znaczenie zdarzeń nie
zmienia się; produkty v4 można atomowo odtworzyć z Capture WAL.


Doprecyzowanie użytkownika 2026-11-12: schema v6 dodaje jedną tabelę
`time_sync` z surowymi obserwacjami `t1..t4` i identyfikatorem zegara hosta.
Nie zastępujemy dowodów czasu gotowym współczynnikiem dopasowania. Produkty v5
można atomowo odtworzyć z Capture, Time i Telemetry WAL.
