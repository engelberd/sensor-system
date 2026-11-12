# rpi-sanok2 — wdrożenie Analysis HDF5

2026-11-11. Źródła zmieniono lokalnie i przesłano wyłącznie na rpi-sanok2.
Application 6, firmware, Capture/Time/Telemetry WAL i binarka recordera
pozostały bez zmian. Kontrakt: [Analysis HDF5 v1](ANALYSIS-HDF5-V1.md).

## Wynik

- Aktywny eksporter: SHA-256
  `364880c7336d62f4399c355100c9ecdee7d4ab9b68b74f095362b39cfba6ec67`.
- Release: końcówka `-hdf5-364880c7336d` w `/opt/sensor-system/releases`.
- Recorder zachował SHA-256
  `ecab5c14b7fd0eecc502df59c2a4c44b1a4bbe58162c732373c1913245489868`.
- Usługa eksportera: `Result=success`; timer: `active`.
- Interpreter: `/home/uigp/sensor-system/host/.venv/bin/python`, h5py 3.16.0.
- Katalog: `var/analysis-protocol6/DD_MM_YYYY/line-{a,b}/*.h5`.
- Docelowy backfill: ostatnie zamknięte okno każdej z czterech zachowanych epok
  dwóch node. Zweryfikowano cztery samodzielne pliki i 160 489 próbek;
  zero zgłoszonych luk i continuity breaks. Kolejny eksport: zero nowych plików,
  zero błędów.

Weryfikator sprawdził odczyt, długości i kolejność kolumn, dokładne timestampy,
liczniki zdarzeń, temperaturę, pełną konfigurację i identyczność osi fizycznych
z przeliczeniem raw XYZ. Dodatkowa próba historyczna objęła 75 plików i
5 641 053 próbek. Próbny pełny backfill zakończono przed aktywacją docelowego
ograniczonego backfillu; jego produkty zastąpiono docelowym zestawem.

## Wycofanie CSV

Zgodnie ze zgodą na usunięcie starych produktów skasowano Analysis CSV,
ich JSON i pozostałości przerwanych eksportów w katalogach `var/analysis`,
`var/analysis-protocol6` oraz dwóch katalogach prób HIL. Łącznie:
23 618 plików / 20 387 126 543 bajty. Kontrola końcowa: zero CSV w obu głównych
katalogach Analysis; wolne miejsce około 39 GiB zamiast 20 GiB.
Usunięto również 12 lokalnych starych plików przykładowych CSV/JSON;
notebook otrzymał ignorowany przykład HDF5 z rpi-sanok2.

Pozostawiono wszystkie źródłowe WAL i sprzętowe BOM. Historyczne 2522 okna
nie zostały zbiorczo przepisane do HDF5: można je ponownie wyeksportować przez
`./hostctl analysis export-new --backfill-all`; nadal obowiązują walidacja
konfiguracji i jawne odrzucanie niepoprawnych historycznych okien.

## Ograniczenie odbioru: recorder był zatrzymany przed zmianą

Już przy pierwszym połączeniu `sensor-host-v2.service` miał stan `failed`.
Log wskazuje `Bootstrap(NodeDeadline)` na kanale 1 / node 1, szesnaście
transakcji bez odpowiedzi. `doctor`: 12 OK, dwa ostrzeżenia — nieaktywna usługa
oraz brak runtime statusu; zero błędów konfiguracji/storage. Stan ten pozostał
bez zmian. Nie potwierdzono nowych okien z żywej akwizycji; odbiór HDF5 opiera
się na rzeczywistych zachowanych WAL. Po przywróceniu komunikacji z node A
należy uruchomić recorder i sprawdzić pierwszy nowy plik HDF5.

## Testy i ponowienie

Lokalnie: 465 testów Rust workspace, 186 testów hosta i dziewięć testów Python
narzędzi danych — wszystkie przeszły. Po końcowej poprawce wyboru konfiguracji
powtórzono testy eksportera. Build ARM64 release wykonano na rpi-sanok2.

Instalator ograniczony do tej instalacji:
`host/runtime/tools/install_analysis_hdf5_sanok2.sh`.
Walidacja: `host/.venv/bin/python host/runtime/analysis/verify_analysis_directory.py var/analysis-protocol6`.

Rollback do CSV wymaga zatrzymania timera, przywrócenia pierwotnego release
`/opt/sensor-system/releases/sensor-host-0.5.0-dev-framing-3-application-6-ecab5c14b7fd-c49bba95f9b9-7e4337688a7e`
jako `current` i uruchomienia timera. Nie przywraca to skasowanych produktów
CSV; mogą powstać ponownie wyłącznie przez eksport źródłowych WAL.


Późniejsza poprawka tego samego dnia przywróciła nagrywanie B mimo nieobecnej A:
[odbiór częściowego startu](RPI-SANOK2-PARTIAL-START-2026-11-11.md).
Opis niesprawnego recordera powyżej dokumentuje stan przed tą poprawką.


Później tego samego dnia użytkownik zlecił usunięcie starych pomiarów i powrót
do standardowych ścieżek: [czysta struktura i proste nazwy HDF5](RPI-SANOK2-CLEAN-LAYOUT-2026-11-11.md).
Opisane wyżej pliki pomiarowe i ścieżki są historyczne.
