# Audyt gotowości rdzenia host v2 i node v2

- Status: obowiązujący przegląd ryzyk i parametrów
- Ostatnia aktualizacja: 2026-10-15
- Zakres: firmware node protocol 5, host v2 1–16 kanałów i RS485 115200 bit/s

## Ocena

Rdzeń protokołu, tożsamości, lifecycle, journala, retransmisji i trwałego
potwierdzania ma poprawne granice odpowiedzialności. Fizyczny pion jednej linii
potwierdził update A/B, rollback, trial confirm i dziesięciominutowy cykl
Measurement bez gap. `sensor-host` składa produkcyjny proces 1–16 kanałów:
waliduje profil, wykonuje fail-closed discovery i admission, odzyskuje Capture
oraz Time WAL, uruchamia niezależne workery i zamyka je kontrolowanie po
`SIGINT`/`SIGTERM`. Symulacja 1/4/8 przechodzi; następną bramką jest fizyczna
macierz na Raspberry Pi. Capabilities schema 2 odpowiada faktycznym output
rates i rodzajowi obrazu. Configuration, Commissioning i RestartNode mają już
zgodne codeki, bounded wykonanie oraz publiczną ścieżkę Operations API przez
jedynego właściciela RS485. HIL na Raspberry Pi z firmware 0.5.8 potwierdził
Get/Apply/Reset Configuration, pusty ScanUnassigned, RestartNode, zmianę
`boot_epoch`, rollover Capture + Time i powrót do Recording. AssignAddress nadal
wymaga rzeczywistego nieprzypisanego node.

## Blokery rdzenia

1. Host ma wewnętrzny bounded command path, idempotentny ledger w pamięci,
   snapshoty statusu per channel i wykonawcę kontrolowanego
   Recording → Maintenance oraz Resume także po zmianie `boot_epoch`.
   Capture i Time są otwierane przez swoich właścicieli, a bounded broker
   koordynatora blokuje wymianę sesji i Start do czasu obu trwałych wyników.
   Bounded lokalne IPC wystawia już status, Maintenance i Resume przez
   `hostctl` bez otwierania RS485. Get/Validate/Apply/Reset Configuration są
   osiągalne przez Operations API; mutacje aktualizują admission, scheduler i
   trwałą historię konfiguracji. Commissioning i RestartNode są osiągalne tym
   samym IPC. Każde przejście operacji jest zapisywane w osobnym per-channel
   WAL z CRC przed wykonaniem i po wyniku.
2. Configuration i ratunkowy restart przeszły fizyczny HIL firmware 0.5.8.
   Commissioning ma potwierdzony pusty skan; nadal wymaga HIL discovery,
   przypisania adresu i końcowego handshake z rzeczywistym nieprzypisanym node.
   Test restartu zanotował pojedynczy timeout i decode error w czasie zmiany
   epoki, po czym kanał ustabilizował się bez gapów i kolejnych błędów. Ten
   kontrolowany transient trzeba jeszcze objąć testem powtarzalnym.
3. Production/Diagnostic zmienia dziś stan lifecycle, lecz nie steruje jeszcze
   kompletną, budżetowaną telemetrią po RS485. Diagnostics jest read-only;
   osobna polityka kompaktowego Health nadal wymaga implementacji.
4. Fizycznie potwierdzono jeden node na jednym kanale. Nadal trzeba zmierzyć
   wiele node na kanale, macierz 1/4/8 portów, długi przewód, zakłócenia,
   restart procesu i utratę zasilania.
5. MVP używa kompaktowego Capture WAL i gotowego strumieniowego eksportu CSV.
   Trzeba potwierdzić eksport na długim nagraniu oraz wykonać benchmark na
   docelowym SSD. Archive, kompresja i uploader nie blokują MVP.

Rollout Raspberry Pi 2026-10-15 uruchomił B/C/D/G/H na firmware 0.5.8 i hoście
z trwałym audytem. Fizyczny gate pięciu kanałów miał 34 wyniki Doctor OK oraz
zero timeoutów, decode errors, retry i storage failures. A pozostaje na 0.5.6
w `Faulted`; ta wersja powstała przed ratunkowym PrepareUpdate/Restart z tego
stanu i wymaga jednorazowego restartu zasilania przed aktualizacją po RS485.
Lokalny override wyłącza wyłącznie A. E i F nie są fizycznie podłączone.

## Przepustowość nowego Measurement Service

Przy maksymalnym chunku 48 próbek request ma do 42 B na przewodzie, a response
do 516 B razem z COBS i delimiterem. Z początkowym turnaround 1 ms transakcja
zajmuje około 49,4 ms. Absolutny limit wynosi około 971 próbek/s, bez timeoutów,
sync, health, filesystemu i zapasu recovery.

| Chunk | Czas transakcji z 1 ms turnaround | Teoretyczna przepustowość |
|---:|---:|---:|
| 8 | 18,0 ms | 444 próbek/s |
| 16 | 24,3 ms | 659 próbek/s |
| 32 | 36,9 ms | 868 próbek/s |
| 48 | 49,4 ms | 971 próbek/s |

Duży chunk wyraźnie zmniejsza narzut, ale zwiększa czas jednego slotu. Przy
polityce 60% jedna linia ma około 583 próbek/s bezpiecznego budżetu danych;
przy twardym limicie 70% około 680 próbek/s. Przykładowo 4 × 125 = 500 próbek/s
mieści się w celu, 5 × 125 = 625 przekracza cel 60%, a 3 × 250 = 750 przekracza
limit 70%. Wyniki muszą zostać zastąpione pomiarem RTT na docelowym przewodzie.

## Miejsca optymalizacji

### Node RAM

`SampleRecord` ma 32 B, choć payload XYZ zajmuje 9 B. Timestamp, config revision,
segment i flags są powtarzane przy każdej próbce. Obecne 2048 rekordów zużywa
około 64 KiB samych próbek i daje 8,2 s przy 250 próbkach/s. Przed prostym
zwiększeniem journala trzeba zbadać rozdzielenie danych per-sample od metadanych
segmentu. Celem jest większa retencja bez utraty dowodów czasu.

Dodatkową pamięć zajmują niezależnie acquisition buffer, ingress, journal,
potrójny USB snapshot i 512-wpisowy `DiagnosticStore`. Każdy ma uzasadnioną
granicę, ale profil Production nie powinien płacić pełnego kosztu diagnostyki,
jeśli nie jest ona dostępna lub używana.

### Node CPU i latency

RS485 TX jest obecnie blokujący. Maksymalna odpowiedź zajmuje około 44,8 ms na
UART przy 115200. Core 0 nadal pobiera dane, lecz core 1 nie drenuje wtedy
ingress ani nie obsługuje observera. Jest to poprawny wariant referencyjny dla
niskich ODR; powrót do DMA wymaga osobnego HIL, pomiaru zajęcia ingress i testu
deadline/abort.

### Host storage

Produkcyjny Capture używa append-only WAL, recovery, ograniczonej kolejki i
group commit. Node zwalnia dane wyłącznie do durable cursor. Parametry batcha i
opóźnienia są typowane w profilu; wymagają benchmarku na docelowym Raspberry Pi
z M.2 SSD, aby ograniczyć latency i zużycie nośnika bez osłabienia durability.

### Host wykonanie

Blokujący timeout portu wynosi 10 ms. Host używa jednego ograniczonego workera
na kanał, jednego właściciela half-duplex I/O i wspólnego bounded Time WAL;
Capture ma osobne bounded executory per strumień. Snapshoty metryk są lock-free
i nie logują każdej próbki. Nie wprowadzamy runtime async bez wyniku pomiaru,
który uzasadniałby dodatkową złożoność.

## Parametry i ich właściciele

### Runtime instalacji

- port i baudrate channel;
- lista node, adresy i oczekiwane hardware ID/board revision;
- sensor ODR, decimation i wynikowy output rate;
- range, HPF, filtr, offsets i FIFO watermark;
- tryb Production/Diagnostic/Maintenance;
- długość okna Capture, katalog storage i polityka durability;
- docelowy limit wykorzystania channel oraz budżety sync/health/diagnostics.

Te wartości wymagają walidacji i desired/observed state. Nie powinny być
rozrzucone jako stałe w `main`.

### Polityka hosta

- transaction timeout, retry delay i late-response quarantine;
- retry budget, offline probe interval i recovery backoff;
- chunk 1–48, maksymalny czas slotu i weighting node;
- próg urgency według backlog/time-to-overflow;
- 60% target i 70% hard channel utilization;
- storage group-commit bytes/time oraz limit oczekujących durable zapisów;
- częstotliwość time sync i health;
- pojemności bounded command/event queues.

### Profil kompilacji firmware

- acquisition 256 próbek;
- ingress 128 zdarzeń;
- journal 2048 próbek;
- gap i continuity po 64 wpisy;
- chunk 48;
- command/result mailbox po 4;
- lifecycle cache 8;
- response outbox 2;
- ingress drain 16 na poll;
- core 1 stack 8 KiB;
- USB snapshot 32 i diagnostics ring 512.

Profil jest jednym miejscem strojenia pojemności, ale USB snapshot, diagnostics,
stack i część timeoutów nadal są w `main_v2.cpp` lub klasach adapterów i powinny
trafić do nazwanych profili kompozycji.

### Stałe protokołu lub produktu

- RS485 115200 8N1 dla obecnego wdrożenia;
- framing 3, application 5, CRC32C i host-initiated request/response;
- payload maksymalnie 512 B;
- XYZ signed-24, 9 B na próbkę;
- dokładnie jeden aktywny request na channel;
- `released <= durable <= received`;
- brak cichej utraty i brak interpolacji w Capture.

Zmiana tych wartości wymaga nowej wersji kontraktu albo jawnej decyzji produktu,
a nie strojenia instalacji.

## Kolejność dalszej pracy

1. Wykonać HIL Scan/Assign z rzeczywistym nieprzypisanym node.
2. Usunąć flash write z pozostałych hot path.
3. Wykonać powtarzalny HIL restartu, restart po zatrzymaniu heartbeat core 1,
   wielu node, długiego przewodu, zakłóceń, wolnego storage i soak.
4. Dodać kompaktową politykę Health z osobnym budżetem Production.
