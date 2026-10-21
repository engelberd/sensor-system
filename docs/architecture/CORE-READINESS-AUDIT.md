# Audyt gotowości rdzenia host v2 i node v2

- Status: kandydat MVP stanowiskowego; otwarte bramki kwalifikacji produktu
- Ostatnia aktualizacja: 2026-10-21
- Zakres: firmware node protocol 5, host v2 1–16 kanałów i RS485 115200 bit/s

## Ocena

Architektura i implementacja tworzą spójny recorder v2. Droga próbki ma jednego
właściciela RS485, bounded kolejki, trwały Capture przed `Release`, dokładne
gapy i recovery po restarcie. TimeSync, Telemetry i operacje korzystają z tego
samego schedulera kanału, więc nie tworzą bocznych właścicieli magistrali.
Konfiguracja, commissioning, restart node, dashboard, runtime status, Doctor,
systemd i strumieniowy Analysis CSV są podłączone do tego samego modelu.

Stanowisko Raspberry Pi z pięcioma aktywnymi liniami B/C/D/G/H i firmware
`0.5.10` jest kandydatem do długiego testu MVP przy B/D/G = 125 Hz oraz C/H =
250 Hz. Doctor zwrócił 34 OK, zero ostrzeżeń i błędów. Wszystkie kanały były
`running`, a liczniki timeoutów, błędów dekodowania, disconnectów, wire
failures, retry, backpressure i błędów storage pozostały równe zero.
Automatyczny eksporter publikuje okna razem z JSON, Events CSV i Telemetry CSV.

To wystarcza do MVP stanowiskowego z jednym node na każdej z pięciu linii.
Nie zamyka jeszcze docelowego MVP produktu opisanego przez wymagania: nie ma
fizycznego dowodu wielu node na jednej magistrali, pracy na przewodzie
400–800 m, recovery po nagłej utracie zasilania ani commissioning
`AssignAddress` rzeczywistego niezarejestrowanego node. Archive pozostaje poza
tym audytem zgodnie z decyzją produktową.

## Wynik audytu 2026-10-21

| Obszar | Stan | Wniosek |
| --- | --- | --- |
| Framing, protocol 5, identity i capabilities | gotowe | wersje są sprawdzane fail-closed; wszystkie aktywne node mają 0.5.10 |
| Measurement i retransmisja | gotowe | małe błędy mają bounded retry; utrata z retencji daje jawny gap |
| Capture i recovery | gotowe funkcjonalnie | WAL, CRC, manifest, kursory i rollover są połączone; pozostaje kwalifikacja power-loss |
| TimeSync | poprawione po audycie | pojedyncza fizycznie błędna obserwacja jest odrzucana i planowana ponownie, bez kończenia workera |
| Telemetry | gotowe dla MVP | temperatura ma walidację w node, cache, rzadki harmonogram, WAL, status, dashboard i CSV |
| Operations | gotowe dla znanych node | Maintenance, Resume, konfiguracja i restart działają przez właściciela kanału |
| Commissioning | częściowe HIL | skan pustej magistrali działa; `AssignAddress` wymaga fizycznego testu |
| Dashboard i systemd | gotowe dla stanowiska | wszystkie cztery usługi są włączone i aktywne; dashboard pozostaje narzędziem zaufanej sieci |
| Analysis CSV | gotowe tymczasowo | eksport jest automatyczny i niezależny od hot path; brak automatycznej retencji CSV |
| Skala fizyczna | niezamknięta | pięć równoległych kanałów działa; wiele node na jednym kanale i 8/16 kanałów nie mają fizycznego gate |

Audyt znalazł realny błąd w przebiegu z 2026-10-20: odpowiedź Time Service z
`ProcessingExceedsRoundTrip` kończyła worker linii B, a kontrolowany shutdown
kończył cały proces kodem 1. Błąd nie dotyczył próbek ani storage. Poprawka
klasyfikuje taki wynik jako `TimeSyncInvalidObservation`, zwalnia właściciela
transakcji i stosuje istniejący bounded retry. Test regresyjny, pełne testy
workspace, `clippy -D warnings` i bramka symulacyjna przechodzą.

Drugim ryzykiem operacyjnym jest pojemność. Bieżące katalogi na karcie SD mają
około 6,2 GB Capture i 13 GB Analysis przy około 18 GB wolnego miejsca. Host
pilnuje rezerwy i zatrzyma się bezpiecznie zamiast pisać w pełny filesystem,
ale bez Archive albo jawnej polityki wynoszenia/usuwania danych nie jest to
instalacja bezobsługowa na dowolnie długi czas. Docelowy SSD zwiększy horyzont,
lecz nadal wymaga monitorowania pojemności.

Audyt obciążenia wykazał również, że 500 Hz na linii H nie jest stabilne z
obecną kartą SD i polityką trwałości. Przed kontrolą H miał pełny backlog 2048
oraz 112881 jawnie brakujących próbek. Pełna walidacja 6,2 GB Capture wykonana
równolegle z recording zwiększyła presję I/O; po jej przerwaniu 500 Hz nadal
nie odrabiało ogona, a licznik wzrósł do 153033. Po kontrolowanym Apply do
250 Hz host wydrenował backlog do zera, TimeSync wrócił do około 3–8 sekund, a
liczniki strat przestały rosnąć. Pełnej walidacji całego drzewa nie wolno
uruchamiać online na tym nośniku; Doctor powinien sprawdzać tylko ograniczony
ogon, a pełny audyt wykonywać w Maintenance albo na kopii danych.

## Bramka finalnego MVP produktu

1. Wdrożyć poprawkę odrzucania błędnej obserwacji czasu i wykonać kontrolowany
   restart z potwierdzeniem `clean=true` oraz ponownego takeover wszystkich
   aktywnych linii.
2. Zakończyć co najmniej 24-godzinny soak pięciu linii i zweryfikować wszystkie
   zamknięte Capture: zero ukrytych dziur, poprawne jawne eventy i świeże Time
   oraz Telemetry.
3. Wykonać restart procesu oraz zimny restart Raspberry Pi. Test odcięcia
   zasilania wymaga kontrolowanego stanowiska i późniejszej walidacji WAL.
4. Podłączyć co najmniej dwa node do jednej magistrali i potwierdzić fairness,
   recovery jednego node oraz brak wpływu na drugi.
5. Wykonać Scan/Assign/handshake na rzeczywistym nieprzypisanym node.
6. Zmierzyć docelowy przewód lub reprezentatywny tor zakłóceń przy 115200 i
   ustalić dopuszczalne ODR na kanał z fizycznych wyników.
7. Ustalić dla okresu bez Archive prostą procedurę monitorowania i wynoszenia
   Capture/Analysis przed osiągnięciem rezerwy filesystemu.
8. Dodać do admission profil zmierzonej przepustowości persistence albo
   zoptymalizować group commit; 500 Hz pozostaje niedopuszczone na bieżącej
   karcie SD mimo wystarczającego teoretycznego budżetu RS485.

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

1. Wykonać powtarzalny HIL takeover po restarcie procesu, kill/power-loss oraz
   wielogodzinny soak z kontrolą gapów, TimeSync i cursorów.
2. Wykonać HIL Scan/Assign z rzeczywistym nieprzypisanym node.
3. Usunąć flash write z pozostałych hot path.
4. Wykonać HIL restart po zatrzymaniu heartbeat core 1, wielu node na kanale,
   długiego przewodu, zakłóceń i wolnego storage.
5. Dodać kompaktową politykę Health z osobnym budżetem Production.
