# rpi-sanok2 — izolacja niesprawnego kanału podczas startu

2026-11-11. Decyzja: [ADR-0015](../adr/0015-partial-channel-startup.md).
Zmiany przygotowano lokalnie i wdrożono wyłącznie na rpi-sanok2.

## Problem i zmiana

Node na linii A nie odpowiadał, a produkcyjny bootstrap przerywał start całego
recordera po 16 timeoutach. Linia B odpowiadała na poprawny handshake.
Bootstrap teraz waliduje kanały oddzielnie i zachowuje ich jawne błędy.
Zdrowe kanały przechodzą admission/recovery i recording; brakujące są faulted,
cały host degraded. Profile nadal zawierają obie włączone linie.

W statusie niesprawnego kanału pozostają `channel_name`, `phase=faulted`
i `startup_error`. Nie powstają dla niego niepotwierdzone identity,
NodeSession, cursor ani strumień Time WAL. Brak wszystkich dopuszczonych
kanałów nadal kończy startup błędem; read-only discovery zachowuje ścisły kontrakt.

## Wdrożenie i odbiór

Recorder SHA-256:
`f542de662139ad35bfd413ff2921a46161b21d2cf896fbefc0ddc729dd3d96e9`.
Release kończy się `-partial-f542de662139`.
Dotychczasowy eksporter HDF5 pozostawiono identyczny bajtowo.
Firmware, konfiguracja node i profile instalacji pozostały bez zmian.

1. Start 16:37:41 CET: host po bounded bootstrapie rozpoczął pracę z jednym
   workerem B i jednym jawnym startup fault A. Operations IPC: B `recording`.
2. Potwierdzono wzrost durable cursora B do 145 i świeże TimeSync bez luk.
3. Kontrolowany restart 16:38:24 CET: shutdown `clean=true`, zero błędów
   protocol/persistence i poprawne zamknięcie wspólnego Time/Telemetry storage.
4. Nowe rzeczywiste okno B wyeksportowano do HDF5: 281 próbek, zero luk,
   jeden jawny continuity break; pełna walidacja osi raw/fizycznych, czasu
   i metadanych przeszła. Plik:
   `var/analysis-protocol6/11_11_2026/line-b/line_b-node_1-16_37-11_11_2026-32d32d8f5b796a99-e2010923965271505690-w0.h5`.
5. Po restarcie B ponownie `recording`, durable cursor wzrósł do 522
   (240 nowych próbek w procesie), zero timeoutów, decode errors, retry,
   braków i storage failures. A nadal faulted, host degraded.

`doctor`: 17 OK, dwa ostrzeżenia degraded i dwa błędy odnoszące się wyłącznie
do brakującej linii A/node A. To jawna niesprawność A przy działającym B,
a nie globalny błąd procesu. ODR B odczytane z node: 7.8125 Hz, revision 13;
nie zmieniano go na wartość zapisaną w profilu.

## Weryfikacja i granice

Pełny workspace Rust: 467 testów zaliczonych. Dodatkowo powtórzono testy
Runtime Status po dodaniu regresji nazwy/przyczyny nieobecnego kanału.
Testy hosta Python: 186 zaliczonych. Clippy workspace/all-targets bez ostrzeżeń.
Test integracyjny potwierdza recording i operacje zdrowych kanałów przy
startup fault, ich clean shutdown i nadal globalną reakcję na błąd storage.
Testy bootstrapu obejmują brak A, nieprawidłową tożsamość i brak wszystkich linii.

Kanał nieobecny przy bootstrapie nie ma jeszcze automatycznego dołączania.
Po przywróceniu A wymagany jest kontrolowany restart hosta; nie trzeba edytować
profilu. Izolacja dotyczy całego kanału, nie podzbioru node tej samej magistrali.
Zachowanie działających workerów przy późniejszej utracie node pozostaje
bez zmian (istniejące retry/offline recovery).

Rollback: zatrzymać recorder, przełączyć `/opt/sensor-system/current` na cel
`/opt/sensor-system/previous` i uruchomić usługę. Poprzednia wersja zachowuje
HDF5, ale ponownie wymaga wszystkich kanałów przy starcie.


Później tego samego dnia użytkownik zlecił usunięcie starych pomiarów i powrót
do standardowych ścieżek: [czysta struktura i proste nazwy HDF5](RPI-SANOK2-CLEAN-LAYOUT-2026-11-11.md).
Opisane wyżej pliki pomiarowe i ścieżki są historyczne.
