# Time Service i model zegara

- Status: model domenowy, wire, endpointy, scheduler, compact Time WAL,
  repozytorium, bounded worker/recovery, threaded executor, startup i
  konfiguracja instalacji, health persistence, deterministyczna symulacja i
  podstawowy HIL jednego node gotowe; przeplatany HIL, długi pomiar dryfu i UTC
  otwarte
- Ostatnia aktualizacja: 2026-10-10

## Cel

Time Service ma powiązać czas próbek z zegarem hosta wraz z jawną
niepewnością. Nie ustawia zegara node i nie zapisuje UTC przy każdej próbce.
Surowe obserwacje pozostają źródłem prawdy, dzięki czemu dokładniejszy model
może zostać zastosowany offline bez ponownego wykonania pomiaru.

Model ma trzy warstwy:

```text
sample sequence -> node monotonic (Measurement timing evidence)
node monotonic   -> host monotonic (Time Service t1..t4)
host monotonic   -> UTC + uncertainty (platform clock observations)
```

Restart node tworzy nową `boot_epoch` i bezwarunkowo nowy model. Skok UTC na
hoście nie może zmienić relacji node-monotonic ↔ host-monotonic.

## Granice timestampów

Jeden exchange zachowuje:

- `t1`: host monotonic po opuszczeniu transmittera przez kompletny request;
- `t2`: node monotonic po odebraniu i zaakceptowaniu kompletnego requestu;
- `t3`: node monotonic przy rozpoczęciu transmisji response;
- `t4`: host monotonic po odebraniu i zaakceptowaniu kompletnego response.

`transaction_id` koreluje próbę. Retry otrzymuje nowy identyfikator i nowe
`t1`; opóźniona odpowiedź starej próby nie może zostać użyta jako obserwacja.
Node zwraca `boot_epoch`, `t2` i `t3`. Host dołącza zachowane lokalnie `t1` i
`t4` dopiero po przyjęciu dokładnie pasującej odpowiedzi.

Wyliczenia są odpowiednikiem podstawowego czterotimestampowego pomiaru NTP:

```text
path_delay = (t4 - t1) - (t3 - t2)
host_midpoint = midpoint(t1, t4)
node_midpoint = midpoint(t2, t3)
observation_uncertainty = ceil(path_delay / 2)
```

To nie jest implementacja NTP ani mechanizm dyscyplinowania zegara. Korzystamy
z tej samej geometrii pomiaru, a następnie budujemy model dla lokalnej,
half-duplex magistrali. Założenie symetrycznej ścieżki jest jawne; asymetria
mieści się w niepewności dopiero po dodaniu wyników pomiarów HIL.

Podstawą wzorów jest RFC 5905, sekcja obliczania offsetu i round-trip delay:
<https://www.rfc-editor.org/rfc/rfc5905.html>. Uzasadnienie wyboru obserwacji o
małym delay opisuje dokumentacja clock filter NTP:
<https://www.ntp.org/documentation/4.2.8-series/filter/>.

## Model online

`sensor-core::ClockModel` jest ograniczony do dwóch anchorów. Nie alokuje
kolejki i służy do decyzji runtime:

- `Acquiring`: nie ma jeszcze dwóch wystarczająco odległych obserwacji;
- `Locked`: dwie obserwacje przeszły limity niepewności, kolejności i dryfu;
- `Holdover`: estymacja leży poza anchorami, ale nie przekroczyła limitu wieku.

Polityka podaje maksymalną niepewność obserwacji, minimalny odstęp anchorów,
maksymalny dryf w ppm i maksymalny holdover. Wartości nie mają jeszcze
produkcyjnych defaultów. Muszą wynikać z symulacji oraz pomiarów Raspberry Pi,
konwertera, przewodu i RP2350.

## Punkt wyjścia dla częstotliwości synchronizacji

Pico 2 korzysta z 12 MHz XOSC. Dokumentacja RP2350 podaje dla zalecanego
Abracon ABM8-272-T3 tolerancję częstotliwości ±30 ppm przy 25°C, stabilność
temperaturową ±30 ppm w zakresie -40..85°C oraz starzenie ±5 ppm w pierwszym
roku. Są to granice kwarcu, a nie gwarantowany dryf kompletnego urządzenia ani
gotowy interwał synchronizacji:

- [RP2350 Datasheet, rozdział XOSC](https://datasheets.raspberrypi.com/rp2350/rp2350-datasheet.pdf)
- [Abracon ABM8-272-T3 Source Control Drawing](https://abracon.com/datasheets/ABM8-272-T3.pdf)

Konserwatywna suma samych składowych node daje 65 ppm w pierwszym roku. Trzeba
do niej dodać błąd zegara hosta, wpływ kompletnego obwodu i margines uzyskany z
HIL. PLL nie usuwa błędu źródła XOSC; mnoży częstotliwość odniesienia.

Jedna ppm oznacza przyrost błędu o 1 µs na sekundę. Górne ograniczenie
interwału można więc wyznaczyć z budżetu:

```text
interval_s <= (allowed_growth_us / relative_drift_ppm)
relative_drift_ppm = node_bound + host_bound + measured_margin
```

Przykład służący wyłącznie do kontroli jednostek: przy 100 ppm względnego
dryfu budżet wzrostu 1000 µs wyczerpie się po 10 s. Produkcyjne wartości nie
zostają zakodowane z tego przykładu.

`TimeSyncScheduleConfig` utrzymuje `interval`, `estimated_slot` i `retry_delay`
oddzielnie dla node. Docelowy profil instalacji ma je podawać konfiguracyjnie.
Pierwsze deadline pozostają fazowane, a Measurement może wyprzeć TimeSync przy
ryzyku przepełnienia journala. Po symulacji i HIL możemy dodać wolniejszy
interwał dla modelu `Locked`; nie jest to wymagane do poprawnego pierwszego
pionu.

Odrzucona obserwacja nie zmienia modelu. Model odrzuca:

- inną albo zerową `boot_epoch`;
- cofnięcie dowolnego zegara;
- czas przetwarzania node większy niż cały zmierzony exchange;
- zbyt duży delay/uncertainty;
- niemonotoniczne anchory;
- dryf przekraczający politykę;
- estymację po wygaśnięciu holdover.

Niepewność estymacji jest co najmniej maksimum niepewności anchorów. Poza ich
zakresem rośnie konserwatywnie wraz z maksymalnym dozwolonym dryfem.

## Startup i konfiguracja instalacji

`TimePersistenceInstallationProfile` opisuje wyłącznie politykę storage:
katalog główny, wspólną ograniczoną pojemność kolejki oraz nazwę workera.
`TimeSyncInstallationPolicy` opisuje wyłącznie politykę ruchu na RS485:
interwał, przewidywany czas slotu i opóźnienie ponowienia. Rozdział zapobiega
uzależnieniu formatu WAL i recovery od częstotliwości synchronizacji.

Identyfikator epoki monotonicznego zegara hosta jest czytany z linuksowego
`/proc/sys/kernel/random/boot_id`. Parser wymaga kanonicznego UUID i odrzuca
wartość zerową. Nazwa pliku zawiera ten identyfikator dla wygody operatora,
ale źródłem tożsamości pozostaje wersjonowany nagłówek WAL.

Po discovery profil łączy `channel_name`, `hardware_id`, adres i `boot_epoch`
z `host_clock_id`, sprawdza duplikaty i buduje stałą topologię. `TimeRuntime`
najpierw otwiera wszystkie WAL-e i zachowuje wynik recovery, a dopiero potem
uruchamia executor i udostępnia klonowalne submittery kanałom. Nie ma okresu,
w którym RS485 może przyjąć obserwację przed zakończeniem recovery.

`TimePersistenceHealth` konsumuje completion wewnątrz `TimeRuntime`. Błąd
storage przechodzi do `RecoveryRequired` i zachowuje dokładny niepewny rekord.
Po reopen stan przechodzi do `Recovering`; wraca do `Healthy` dopiero po
końcowym durable receipt tego samego rekordu z kolejnym licznikiem. Nieznany
strumień, zły receipt lub skok licznika tworzy trwały `IntegrityFault`.

Tracked profil ma obecnie wartości startowe 10 s interwału, 20 ms slotu i
250 ms retry. Są jawnie konfigurowalne i nie stanowią wartości produkcyjnych;
symulacja potwierdza mechanikę, a HIL ma je zatwierdzić lub zmienić.

## Brama symulacji

`SimulatedRs485Bus` obsługuje Time Service przez ten sam framing,
`ChannelPort`, `ChannelAgent` i `TransactionCore` co adapter produkcyjny. Model
node ma konfigurowalny offset, podpisany drift ppm oraz osobne opóźnienie
request/response. Dodatkowe opóźnienie pojedynczej odpowiedzi modeluje
deterministyczny jitter. Restart zeruje monotoniczny zegar node i tworzy nową
`boot_epoch`.

Powtarzalne testy potwierdzają:

- model przy `+65 ppm` oraz asymetrii 400/120 µs obejmuje prawdziwy czas swoją
  wyliczoną niepewnością;
- duży jitter pozostaje widoczny w `t1..t4` i obserwacja przekraczająca limit
  jakości nie zmienia modelu;
- odpowiedź gotowa po deadline jest klasyfikowana jako `LateResponse` i nigdy
  nie powstaje z niej `ClockObservation`;
- odpowiedź po restarcie z inną `boot_epoch` nie może wejść do starego modelu.

Symulator uwzględnia 8-N-1, baudrate, długość ramek, turnaround i jawne
opóźnienia ścieżek. Nie modeluje jakości elektrycznej przewodu, opóźnień
sterownika USB/serial ani schedulera Linuksa. Te składniki oraz produkcyjne
progi pozostają bramą HIL.

## Trwałość i następny pion

Model dwupunktowy nie zastępuje historii. Capture musi dostać osobny,
wersjonowany rekord surowej obserwacji `t1..t4`, node identity i `boot_epoch`.
Archive zachowa te obserwacje albo ich zweryfikowany blok źródłowy. Dopiero
narzędzia offline mogą użyć większego okna, odrzucania outlierów i ponownego
fitowania.

Kolejność implementacji:

1. **Gotowe w `ChannelRuntime`:** kandydat `TimeSync` ma osobny stan i
   konfigurację, ale konkuruje z Measurement w tym samym schedulerze deadline.
   Retry zwalnia kanał, stare completion nie może zmienić nowego work itemu,
   a stary interfejs measurement-only odmawia pracy po włączeniu TimeSync;
2. **Gotowe:** unified dispatch jest podłączony do obu wariantów produkcyjnej
   pętli kanału. Każdy node może dostać osobny pierwszy deadline, więc
   synchronizacje są wykonywane pojedynczo i fazowane. Sukces, timeout, `Busy`
   i błąd transmisji rozliczają dokładnie właściwy work item;
3. **Gotowa granica channel → storage:** zweryfikowany rekord zawiera
   `hardware_id`, adres node, `boot_epoch` i surowe `t1..t4`. Pętla kanału zna
   wyłącznie nieblokujące `try_submit` do bounded queue. `QueueFull` oraz
   `Disconnected` zwracają własność rekordu, tworzą jawny settlement
   `Backpressured` i planują retry bez zatrzymania Measurement. Pozostają
   worker i completion trwałości;
4. **Gotowy format Time WAL:** stały rekord 36 B zawiera wyłącznie `t1..t4`
   oraz CRC32C. Tożsamość node/boot i host clock epoch występuje raz w nagłówku.
   Writer obsługuje batch z jednym `sync_data`, recovery niepełnego ogona i
   fail-closed CRC. Szczegóły opisuje [TIME-WAL-V1.md](TIME-WAL-V1.md);
5. **Gotowy router, worker, executor i startup:** `TimeRepository` ma stałą topologię
   strumieni, a `TimePersistenceWorker` ogranicza wejście/completion, faultuje
   po pierwszym błędzie i przez record count rozróżnia retry od już trwałego
   appendu. `TimePersistenceExecutor` wykonuje I/O na osobnym wątku, współdzieli
   bounded submittery pomiędzy kanały i ma kontrolowany shutdown.
   `TimePersistenceInstallationProfile` tworzy topologię dopiero z discovery,
   a `TimeRuntime` kończy recovery przed udostępnieniem submittera. Polityka
   synchronizacji pozostaje osobnym obiektem. `TimePersistenceHealth` rzutuje
   completion i recovery na jawny stan `Healthy`, `RecoveryRequired`,
   `Recovering` albo `IntegrityFault`;
6. **Gotowe:** zasymulowane są jitter, asymetria, dryf, timeout, późna
   odpowiedź i restart przez rzeczywisty hostowy framing i transaction core;
7. **Gotowy podstawowy HIL jednego node:** firmware 0.5.6 i host wykonały osiem
   exchange przy 115200 bit/s bez timeoutów i błędów dekodowania. Pozostają
   dłuższy pomiar dryfu, HIL z TimeSync przeplatanym z Measurement, wiele node
   na kanale i dopiero potem dobór interwałów oraz progów.

Warstwa host-monotonic ↔ UTC powstanie osobno. Do tego czasu model nie może
produkować `TrustedUnixSeconds` ani dzielić Archive na godziny UTC.
