# Host v2 i firmware node — plan implementacji

- Status dokumentu: aktywny plan wykonawczy
- Ostatnia aktualizacja: 2026-10-21
- Baseline: `architecture-v1`

Ten dokument odpowiada na cztery pytania: co już mamy, czego brakuje, co robimy
dalej i po czym poznajemy, że etap jest skończony. Nie zastępuje kontraktów
produktowych ani ADR-ów.

Krytyczny przegląd bieżącego rdzenia i mapa parametrów znajdują się w
[CORE-READINESS-AUDIT.md](CORE-READINESS-AUDIT.md).

## 1. Cel systemu

System zbiera bez utraty surowe próbki XYZ z wielu node, zachowuje dowody czasu
i jawnie opisuje każdą lukę. Jeden host obsługuje 1–16 niezależnych channels.
Obecną implementacją channel jest half-duplex RS485 115200 bit/s na przewodzie
około 400–800 m; jeden channel może mieć wiele node.

Host odpytuje node. Node zachowuje niepotwierdzone próbki w ograniczonym
`MeasurementJournal`. Host potwierdza zakres dopiero po trwałym zapisie. Krótkie
zakłócenia naprawia retransmisja, a utrata po przekroczeniu retencji jest jawnym
zdarzeniem jakościowym.

System ma trzy tryby:

- `Production` — maksymalnie przewidywalny transfer danych, tylko ograniczona
  diagnostyka potrzebna do health i recovery;
- `Diagnostic` — dodatkowa telemetria i narzędzia kosztem jawnie przydzielonego
  budżetu;
- `Maintenance` — konfiguracja, commissioning i update; pomiary mogą być
  zatrzymane.

## 2. Obowiązujący podział technologiczny

| Obszar | Technologia | Odpowiedzialność |
| --- | --- | --- |
| Firmware node | C++17 + Pico SDK | sensor, journal, protokół, bootloader |
| Runtime hosta | stable Rust, ARM64 Linux | protokół, scheduler, sesje, zapis, recovery, control plane |
| Narzędzia danych | Python | odczyt, walidacja, migracja i analiza offline |
| UI | cienki klient Operations API | prezentacja i składanie commands |

Pierwszy runtime hosta jest jednym procesem `sensor-host`. Każdy channel ma
własny logiczny `ChannelAgent`, ograniczone kolejki i jednego właściciela I/O.
Izolacja do osobnych procesów może później objąć cały `TransportDevice`, jeśli
uzasadnią ją pomiary lub testy awarii.

## 3. Stan implementacji

### Gotowe w kodzie v2

- workspace Rust z crate'ami `sensor-core`, `sensor-protocol`, `sensor-host`;
- zakaz `unsafe` w obecnych bibliotekach;
- typy `HardwareId`, `BootEpoch`, `SampleSequence`, `SampleIdentity`;
- osobny natywny `Adxl355SampleCode` signed-20 oraz `SampleAxis` signed-24 dla
  dokładnego wyniku potoku node i zwartego wire;
- dokładny `SampleRate` w nanohertz i profil wszystkich 11 ustawień ODR ADXL355;
- jawny `Gap` z zakresem, przyczyną i ochroną przed overflow;
- `SequenceCursor` jako granica wyłączna oraz trzy chronione cursory delivery:
  received, durable i released;
- semantyczne kontrakty `PULL_SAMPLES`, `GET_STREAM_STATE` i
  `RELEASE_SAMPLES` oraz zamrożony wire Measurement Service;
- wersje framing `3`, application `5`, host `0.5.0-dev`;
- fail-closed przy niezgodnej wersji protokołu;
- minimalna binarka hosta raportująca wersję oraz `sensor-hil` wykonujący
  fail-closed handshake przez produkcyjny adapter portu;
- pełny zestaw testów workspace Rust, czysty Clippy i release build workspace;
- zaakceptowany ADR-0008 i normatywna specyfikacja framing protocol 3;
- bounded encoder/decoder COBS + CRC-32C w Rust i C++ z limitem payloadu 512 B;
- wspólne binarne golden vectors potwierdzające identyczny wire Rust ↔ C++;
- host-side Transaction Core z deadline, quarantine oraz rozróżnieniem
  `RetrySafe`/`ReconcileState`;
- kontrakty i codeki Identity/Capabilities po obu stronach;
- bounded dispatcher node obsługujący pełną transakcję Identity przez framing v3;
- fail-closed hostowa maszyna handshake z wymaganiami instalacji i Transaction Core;
- event-driven `NodeSession` oraz bounded `SimulatedNodeLink` sprawdzające
  retransmisję, trwałość, restart i overflow retencji bez I/O;
- zamrożony wire Measurement Service, niezależne golden vectors oraz fail-closed
  codeki Rust/C++ dla requestów, wyników, exact gap, timing evidence i signed
  XYZ24 LE.
- stałopojemnościowy firmware `MeasurementJournal` bez alokacji, łączący zwarty
  sample ring z bounded `GapLedger`, widokami bez kopiowania oraz testami wrap,
  overflow, retransmisji i mieszanego modelu stanów.
- niezależny od wire `DeliverySession` z jednorazowym tokenem
  prepare/complete/abort, ograniczeniem chunku, fail-closed timing evidence oraz
  typowanymi wynikami dla wszystkich ścieżek Measurement pull/release.
- `MeasurementServiceHandler` obsługujący trzy messages, pełne mapowanie błędów
  oraz encoding bez pośredniej tablicy próbek przez ograniczony view journala.
- routing Measurement w `ApplicationDispatcher` oraz stałopojemnościowy
  `ResponseOutbox`, który atomowo rozstrzyga delivery token przy enqueue i nie
  pozostawia sesji w `Busy`, gdy kolejka jest pełna.
- niezależny `ProtocolV3TransportEndpoint` z ograniczonym budżetem RX, jednym
  aktywnym TX, retry startu, deadline/abort oraz licznikami diagnostycznymi;
  fake-port tests i pełny build board rev 2 przechodzą.
- zaakceptowany ADR-0009, niezależny od sensora `SourceSequencer` oraz bounded
  `GapLedger` z dokładnymi zakresami i scalaniem.
- docelowy `MeasurementJournal`, który łączy zwarty sample ring i `GapLedger` w
  jeden cursor space, zwraca sample albo exact gap i fail-closed skraca retencję
  po przepełnieniu; test modelowy obejmuje 50 000 mieszanych zdarzeń.
- bounded `SampleIngress` core 0 → core 1 z lock-free SPSC, źródłowym sequence,
  pending quality backlogiem bez wyprzedzania oraz testem 200 000 zdarzeń pod
  ThreadSanitizer.
- bounded mailbox command/result core 1 ↔ core 0 oraz executor z pojedynczym
  pending result; backpressure wyniku nie wykonuje operacji drugi raz, a każdy
  `poll` ma ograniczony koszt.
- przyjęty ADR-0010 rozdzielający obowiązkowe ordered quality events,
  stałokosztową telemetrię Production i bounded diagnostykę serwisową; wykryty
  niebezpieczny synchroniczny zapis flash z legacy hot path.
- semantyczny `ContinuityBreak` w `sensor-core`/`sensor-protocol`, durable gating
  w `NodeSession` oraz firmware `ContinuityLedger`, `MeasurementJournal` i
  `DeliverySession`; break zajmuje marker cursora bez fałszywego missing count.
- wire `CONTINUITY_BREAK` result 6 w Rust/C++, wspólny 35-bajtowy golden vector
  i firmware handler z tym samym atomowym delivery tokenem co sample/gap.
- `SampleIngress` przenosi sample, exact gap i continuity break; bounded pending
  quality backlog zachowuje kolejność przy pełnej kolejce i przechodzi
  fail-closed po przepełnieniu zamiast ukryć zdarzenie.
- deterministyczny `Core1ProtocolRuntime`, `node_v2::DataPlane` i jeden
  composition root łączący ingress, journal, protocol endpoint, lifecycle oraz
  system operations;
- osobny obraz `sensor_system_node_v2_integration` dla board rev 2, budujący
  prawdziwe sterowniki z application protocol 5;
- hostowy `ChannelAgent`, `ChannelPort` i `ChannelEndpoint`: jedna transakcja na
  linię, stałe bufory wire, częściowe write, `tcdrain`, timeout po zakończeniu
  TX, handshake, Lifecycle i Measurement w jednym arbitrze;
- `MeasurementTransfer` zachowujący pełny `SampleChunk` z timing evidence i
  `config_revision` oraz `DurableMeasurementSink`, który nie pozwala podnieść
  release przed trwałym potwierdzeniem;
- wykonywalne testy host-side C++ obejmujące protokół, runtime i kompozycję
  firmware;
- produkcyjne obrazy slotów A/B budowane z `main_v2`, paczka update protocol 5,
  zdalne `ConfirmTrialBoot` oraz automatyczny restart do rollbacku po 30 s bez
  potwierdzenia.
- fizyczny update A/B wyłącznie przez RS485, wymuszony rollback bez confirm,
  poprawny confirm obrazów 0.5.4 i 0.5.5 oraz pion Start → durable `fsync` →
  Release → Stop → Maintenance;
- naprawiony adapter hosta, który po timeoutcie odczytu nie odtwarza już
  wcześniej zużytego fragmentu bufora wejściowego.
- capabilities schema 2: typowana maska output ODR po decymacji, zgodna z
  rzeczywistym zakresem `2000..62.5 Hz`, oraz feature bits zależne od rodzaju
  obrazu A/B;
- kontrolowane Recording → Maintenance → Recording dla niezmienionego
  `boot_epoch`: świeży handshake, `GetState`, `GetStreamState`, odbudowa sesji
  wyłącznie z durable Capture, ponowna admission i sekwencyjny Start;
- pełny Resume po zmianie `boot_epoch`: idempotentny rollover Capture i Time,
  bounded broker worker → koordynator, wymiana `NodeSession` i kursora
  schedulera dopiero po obu wynikach durable, a następnie sekwencyjny Start.
  Stare WAL-e pozostają dowodem; nieudana próba zostawia kanał w Maintenance,
  więc można ją bezpiecznie ponowić zdalnie;
- lokalne Operations IPC schema 1 przez bounded Unix datagram oraz natywny
  adapter `hostctl channel`; status kanału, Maintenance i Resume docierają do
  istniejącego worker-owned ledgera bez otwierania RS485 przez klienta;
- bootstrap aktywnego kanału traktuje zapisany profil instalacji jako trwały
  desired state: node pozostawiony w Maintenance przez przerwany proces jest
  po pełnym handshake i rekonstrukcji cursorów jawnie przeprowadzany przez
  ExitMaintenance do Recording. Worker nie znika już z Operations API w stanie,
  z którego jedyną drogą odzyskania był restart zasilania;
- Get/Validate/Apply/Reset Configuration per node przez ten sam control plane;
  mutacje mają optimistic revision, preflight admission całego kanału,
  rekonfigurację quiescent schedulera i trwały WAL znaczenia `config_revision`;
- bounded walidacja Capture i strumieniowy `capture-export`, generujący
  `samples.csv`, `events.csv` oraz `windows.csv` bez ładowania nagrania do RAM;
- automatyczny, idempotentny `Analysis CSV v2` per zamknięte okno Capture:
  tylko osie XYZ w `m/s²`, uproszczona nazwa z czasem pierwszej próbki,
  ustawiony i wyznaczony ODR oraz pełne znaczenie czasu i konwersji w JSON,
  osobne zdarzenia jakości, rzadki `telemetry.csv` dopasowany czasem urządzenia
  oraz timer systemd poza gorącą ścieżką RS485;
  pierwszy przebieg ma ograniczony backfill i nie rozwija całej historycznej
  bazy do tekstu;

### Gotowe jako decyzja lub kontrakt

- model Host → TransportDevice → Channel → Node → Sensor;
- jeden właściciel każdego zmiennego stanu;
- dane źródłowe: raw XYZ, sequence, boot epoch, gaps, konfiguracja i dowody czasu;
- ruchome, konfigurowalne okna Capture, domyślnie około 600 s od pierwszej
  rzeczywistej próbki;
- brak interpolacji w Capture i Archive;
- rozdzielenie Capture, Archive i Derived;
- niezależny profil Production/Diagnostic oraz lifecycle
  Idle/Recording/Stopping/Maintenance/Faulted;
- lokalny Operations API jako jedyna droga sterowania runtime;
- ręczny wybór ODR z admission control; bez automatycznej zmiany ODR w pierwszej
  wersji;
- świadome zerwanie zgodności ze starym application protocol 4/framing 2.

### Nadal działa wyłącznie w systemie legacy

- obecny grant/burst/commit;
- timing DRDY i model synchronizacji;
- zapis HDF5 pozostaje w legacy; dashboard v2 i Configuration korzystają już z
  produkcyjnego Capture, Runtime Status i Operations API.

Interfejs operatorski nie wystawia już tych implementacji jako poleceń
produkcyjnych. `hostctl` obsługuje wyłącznie V2; zakres i jawne braki opisuje
[HOSTCTL-V2.md](HOSTCTL-V2.md). Produkcyjny Capture V2 ma własną walidację i
eksport. Configuration, Commissioning i RestartNode protocol 5 są podłączone do
Operations API i `hostctl` przez jedynego właściciela RS485. Pięć aktywnych
node działa na firmware 0.5.10. HIL potwierdził Configuration, pusty skan,
restart i Resume; panel Commissioning jest podłączony, a do zamknięcia
pozostaje HIL Scan/Assign z nieprzypisanym node. Telemetry temperatury ma pełny
pion node → scheduler → WAL → status/dashboard → Analysis CSV. Archive,
kompresja i uploader są świadomie odłożone poza MVP. `Analysis CSV v2` jest
obecnym formatem gotowym do podstawowej analizy drgań; Capture WAL pozostaje
źródłem prawdy.

Nowy host ma konkretny adapter serial/RS485 i zaliczony pion HIL jednej linii.
Nie oznacza to jeszcze gotowości produkcyjnej dla wielu node, 1–16 channels,
długich przewodów ani awarii storage.

Te elementy są źródłem doświadczeń i scenariuszy testowych. Nie importujemy ich
bezpośrednio do `host_v2` i nie kopiujemy ich struktury klas.

## 4. Granice modułów

### `sensor-core`

Zawiera znaczenie danych i polityki produktu. Obecnie są to identity, sample,
gap, continuity i cursory. Docelowo trafią tu polityki schedulera, trybów,
recovery i decyzji commit. Nie zna bajtów wire, serial, filesystemu, HDF5, HTTP
ani konfiguracji JSON.

### `sensor-protocol`

Zawiera wyłącznie kontrakt wire: framing, CRC, ograniczony decoder strumienia,
typowane messages, wersje i capabilities. Nie decyduje o retry, trwałości,
harmonogramie ani trybie pracy.

### `sensor-host`

Składa runtime i adaptery: RS485, storage, clock, status/control IPC oraz
konfigurację instalacji. `main.rs` pozostaje cienki.

### Firmware node

Docelowo rozdziela `SensorRuntime`, akwizycję i numerowanie źródłowe,
`MeasurementJournal`, `DeliverySession`, codec protokołu oraz
bounded mailbox danych i operacji sensora. Core 0 jest jedynym wykonawcą
operacji ADXL; core 1 jest właścicielem journala i transportu. Każda próbka
otrzymuje sequence na core 0, przed pierwszą granicą, na której może zostać
utracona.

### Python offline

Czyta zapieczętowane produkty danych, waliduje je i tworzy Archive/Derived.
Nie jest ładowany przez `sensor-host` i nie uczestniczy w durable commit.

## 5. Krytyczny przepływ produkcyjny

```text
ADXL -> source sequence -> bounded sample/gap ingress -> MeasurementJournal node
     -> odpowiedź na pull albo dokładna informacja o luce
     -> decoder hosta -> kontrola identity/sequence
     -> durable append Capture -> DurableReceipt
     -> release do node -> zwolnienie retencji
```

Zakazane skróty:

- commit przed `DurableReceipt`;
- ciche pominięcie błędnej ramki lub luki;
- nieograniczona kolejka;
- retry sterowane przez codec;
- bezpośrednie otwieranie portu przez UI lub konfigurator;
- uznawanie czasu odbioru ramki za czas pobrania próbki.

## 6. Etapy implementacji i bramki

### Granica MVP 24/7

MVP jest autonomicznym lokalnym recorderem, a nie kompletną platformą danych.
Raspberry Pi ma uruchamiać się sam, nagrywać na SSD, obsługiwać wiele kanałów,
odzyskiwać się po typowych awariach i pozwalać na zdalne operacje serwisowe
wyłącznie przez RS485.

Do bramki MVP należą:

1. rollover Capture i Time po zmianie `boot_epoch`, bez restartu procesu hosta;
2. minimalne Operations IPC i `hostctl`: status, Maintenance, Resume, restart
   node oraz odczyt/walidacja/zapis konfiguracji;
3. częściowy start instalacji, ponowne dołączenie kanału oraz powrót node po
   odłączeniu bez zatrzymania zdrowych kanałów;
4. instalacja systemd, automatyczny start, kontrolowany shutdown, runtime status
   i `hostctl doctor` na Raspberry Pi;
5. fizyczna kwalifikacja 1/4/8 channels, wielu node na jednej magistrali,
   restartu procesu/Pi, utraty zasilania i krótkich błędów łącza;
6. bounded walidacja Capture i prosty eksport do CSV dla MATLAB/Pythona.

CSV jest formatem wymiany generowanym z zapieczętowanego i zweryfikowanego
Capture. Nie zastępuje kompaktowego WAL na gorącej ścieżce, ponieważ tekstowy
zapis każdej próbki zwiększyłby rozmiar, liczbę konwersji i obciążenie storage.
Istniejący `capture-export` zapisuje strumieniowo `samples.csv`, `events.csv` i
`windows.csv` ze stałym użyciem pamięci. `analysis-export` publikuje dodatkowo
osobny, atomowy CSV dla każdego zamkniętego około dziesięciominutowego okna.

Poza MVP pozostają Archive, kompresja, publication outbox, uploader,
automatyczny dobór ODR i fizyczna kwalifikacja 16 kanałów. Dashboard oraz
prosty commissioning jednego nowego node są zaimplementowane; wymagają jeszcze
końcowego HIL.
Te elementy nie blokują pierwszego recorderowego wdrożenia 24/7.

Numery oznaczają obszary i bramki produktu. Fundament, runtime node i pion HIL
jednej linii są zamknięte. Bieżąca ścieżka zależności prowadzi przez scheduler
wielu node w etapie 4 → produkcyjny Capture w etapie 7 → czas, koordynację
1–16 channels i Operations. Stan na dziś:

| Etap | Stan | Co realnie pozostaje |
| --- | --- | --- |
| 0. Fundament | ukończony | utrzymanie dokumentacji i reguł zależności |
| 1. Framing v3 | codeki, integracja i HIL jednej linii gotowe | benchmark RP2350, szum i soak |
| 2. Discovery | runtime oraz fizyczny fail-closed handshake gotowe | inventory i testy wielu node |
| 3. Transfer/`NodeSession` | fizyczny wire-to-durable pion oraz produkcyjny Capture gotowe | recovery po utracie procesu/zasilania w HIL |
| 4. Scheduler/`ChannelRuntime` | pełny pion Measurement, TimeSync, Telemetry i symulacja wielu node gotowe | HIL wielu node i strojenie fizycznych budżetów |
| 5. Firmware protocol 5 | Configuration, restart, telemetria i pusty skan potwierdzone HIL 0.5.10; Commissioning, obrazy A/B, rollback, migracja i confirm gotowe w kodzie | HIL AssignAddress, soak i pomiary granic pojemności |
| 6. Adapter RS485/HIL | handshake, update i pełny transfer jednej linii gotowe | wiele node, długi przewód, zakłócenia i restart hosta |
| 7. Capture | WAL, recovery, segmenty, bounded handoff, group commit, fault injection, walidacja, CSV i rollover podłączony do Resume gotowe | power-loss matrix i benchmark docelowego SSD |
| 8. Czas | model, wire, endpoint, scheduler, persistence, startup/config/health, symulacja, HIL jednego node i koordynowany rollover gotowe | długi pomiar dryfu, wiele node i fizyczna kwalifikacja odrzucania outlierów |
| 9. Skala 1–16 | runtime i symulacja 1/4/8 gotowe | fizyczny test macierzy i wiele node na kanał |
| 10. Operations | bounded command path, IPC, trwały audit oraz wykonanie Maintenance/Resume/Configuration/Commissioning/Restart gotowe | operacje grupowe i HIL AssignAddress |
| 11. Migracja | eksport CSV gotowy | canary, rollback, procedura operacyjna i usunięcie legacy |

### Etap 0 — fundament projektu — ukończony

Rezultat: workspace, kierunek zależności, podstawowe value types, wersje i
dokumentacja źródła prawdy.

### Etap 1 — framing protocol 3 — implementacja w toku

Format zamrażają [ADR-0008](../adr/0008-framing-v3.md) i
[FRAMING-V3-SPEC.md](../protocol/FRAMING-V3-SPEC.md). Codecs Rust i C++ oraz
golden vectors są gotowe. Do zamknięcia etapu pozostaje benchmark na RP2350 i
HIL warstwy RS485; nie blokuje to implementacji messages na tych codecach.

Zakres:

- spisać format envelope bajt po bajcie;
- ustalić sync marker, length, adresowanie, message kind, request/operation ID,
  wersję i CRC;
- określić maksymalny rozmiar ramki oraz limity resynchronizacji;
- zaimplementować encoder i inkrementalny bounded decoder w Rust;
- przygotować poprawne i uszkodzone golden vectors niezależne od języka;
- zaimplementować ten sam codec w małym module C++ host-test dla firmware.

Brama ukończenia:

- Rust i C++ kodują identyczne bajty;
- oba dekodery odrzucają te same błędne ramki;
- testy obejmują fragmentację, sklejenie ramek, szum, zły length, zły CRC i
  powrót do synchronizacji w ograniczonej liczbie bajtów;
- parser nie alokuje według niezaufanego `length`.

### Etap 2 — discovery i capabilities — jedna linia sprawdzona fizycznie

Zakres: `Hello`, tożsamość sprzętu, boot epoch, adres node, wersje, obsługiwane
messages, maksymalny payload, profile ODR i możliwości journala.

Przed messages obowiązuje wspólny host-side Transaction Core opisany w
[TRANSACTION-POLICY.md](../protocol/TRANSACTION-POLICY.md): jedna próba na
channel, nowy transaction ID dla retry, deadline, quarantine po timeout oraz
rozdzielenie `RetrySafe` od `ReconcileState`.

Brama: handshake przed recording jest fail-closed, a niezgodność wskazuje
konkretne pole; identity nie zależy od portu USB ani etykiety operatora.

### Etap 3 — strumień transferu i czysta `NodeSession` — logika podstawowa gotowa

Zakres: continuous segment, transfer chunk, trzy jawne cursory, duplikaty,
luki, retry, durable write i niejednoznaczny wynik release. Najpierw `SimulatedNodeLink`
oraz pamięciowe repozytorium, bez serial i filesystemu.

Semantykę opisuje
[MEASUREMENT-SERVICE.md](../protocol/MEASUREMENT-SERVICE.md), a układ bajtów
[MEASUREMENT-WIRE-V5.md](../protocol/MEASUREMENT-WIRE-V5.md). `NodeSession`,
symulator, golden vectors i codeki Rust/C++ są gotowe. `ChannelPort` realizuje
serial, a `DurableMeasurementSink` zamraża granicę storage bez wyboru formatu.
Wire opisuje niedostępny prefiks retencji, exact `GAP` i `CONTINUITY_BREAK`;
konkretny Capture sink i recovery pozostają etapem 7.

Brama: deterministyczne testy dowodzą poprawności po timeout, duplikacie,
restarcie node, restarcie hosta, błędzie storage i odpowiedzi utraconej po
commit. Żaden commit nie wyprzedza trwałego cursora.

### Etap 4 — scheduler wielu node i `ChannelRuntime` — pion Measurement zasymulowany

Zakres: rotacja pull, deadline, retry budget, time-sync budget, trzy tryby,
backpressure i admission control dla 115200 bit/s. Obowiązujący model transakcji
i klas ruchu opisuje
[COMMUNICATION-MODEL.md](../protocol/COMMUNICATION-MODEL.md); dokładne wagi,
chunk size i interwały pozostają wynikiem pomiarów.

Brama: symulacja wielu node nie głodzi żadnego zdrowego node, diagnostyka nie
narusza budżetu Production, a konfiguracja niemieszcząca się w paśmie jest
odrzucana przed startem.

`ChannelScheduler` wybiera deterministycznie jedną gotową pracę według ryzyka
overflow, deadline zakończenia, czasu oczekiwania i profilu obserwowalności.
Waliduje maksymalny czas slotu, respektuje retry `ready_at`, nie dopuszcza
diagnostyki w Production ani ponad przekazany budżet. `ChannelAdmission` liczy
konserwatywny koszt measurement, time sync i health z osobną rezerwą recovery;
czasy transakcji pozostają wejściem z HIL, a nie ukrytą stałą protokołu. Oba
moduły są czystą polityką i mają deterministyczne testy.

`ChannelRuntime` posiada bounded rejestr node jednej linii, wylicza
`time_to_overflow` z output ODR bez float, starzeje ostatnią obserwację
producenta, pilnuje jednej pracy `in_flight` na całym half-duplex channel i
odrzuca stare completion przez `work_id`. Błąd dostaje wykładniczy bounded
backoff, a po limicie przechodzi w wolne próby offline. Regresja strumienia
wyłącza node z rotacji do jawnego recovery. Testy potwierdzają rotację równych
node, brak równoległych dispatchy, ustępowanie magistrali po błędzie i ochronę
przed starym wynikiem.

`MeasurementCoordinator` domyka dispatch → `MeasurementTransfer` →
`ChannelAgent` → durable sink → progress runtime. Przenosi jedyną instancję
`NodeSession`, więc nie istnieją równoległe kopie cursorów. Błąd storage
zachowuje ten sam pending transfer i ponawia tylko persistence. Zweryfikowane
`produced` z odpowiedzi aktualizuje estymację backlogu dopiero po trwałym
handoff. `ChannelAgent` wykonuje mały, konfigurowalny budżet prób w jednym
slocie; po jego wyczerpaniu zwraca transfer do runtime, który stosuje backoff i
pozwala pracować pozostałym node.

`ChannelRecordingLoop` składa ten pion z `ChannelEndpoint` bez ukrytego wątku i
wykonuje najwyżej jedną akcję wire albo jedno rozliczenie persistence na krok.
Po odpowiedzi runtime wyznacza `ready_at` z output ODR, zweryfikowanego backlogu,
docelowego wypełnienia chunku i deadline obsługi. Zapobiega to ciasnej pętli
`NoData`, a zaległy strumień jest pobierany bez sztucznego oczekiwania.

Deterministyczna symulacja bajtowej magistrali obejmuje cztery node 125 Hz przez
pełny framing v3 i Measurement protocol 5. Potwierdza brak głodzenia zdrowych
node, brak overflow, odzyskanie pojedynczej utraconej odpowiedzi bez luki,
odrzucenie odpowiedzi przychodzącej po timeoutcie, ponowienie wyłącznie
persistence po błędzie storage oraz wolne próby node offline. Zakres i
ograniczenia zapisuje
[HOST-V2-SIMULATION.md](HOST-V2-SIMULATION.md).

Wykonawca wire Time i wspólne księgowanie jego budżetu są gotowe. Pozostają
wykonawcy Health/Diagnostic oraz przeplatany HIL wielu node. FIFO agenta
pozostaje bounded mechanizmem wykonawczym dla jawnie przekazanej pracy, a nie
źródłem kolejności produkcyjnej.

### Etap 5 — firmware node protocol 5 — pion A/B zaliczony

Zakres: `MeasurementJournal`, `DeliverySession`, nowe messages, release ograniczony do
trwale zapisanego i faktycznie dostarczonego zakresu, źródłowe sequence na core
0, bounded ingress sample/gap core 0 → core 1, mailbox command/result core 1 ↔
core 0 oraz zachowanie obecnego sterownika ADXL i bootloadera. Utrata na wejściu
mailboxa musi utworzyć dokładny zakres luki; późniejsza próbka nie może dostać
ponownie numeru utraconej próbki. Trial boot nie może zostać automatycznie
potwierdzony tylko dlatego, że aplikacja wystartowała. Wymaga lokalnego health i
jawnego potwierdzenia hosta przez nowy protokół; brak potwierdzenia kończy się
restartem i rollbackiem do starego slotu.

Brama: testy host-side C++, wspólne golden vectors, testy overflow journala i
retransmisji; oba buildy slotów i profile płytki przechodzą. Bez wdrożenia na
sprzęt przed walidacją board revision. Migracja node bez USB przechodzi pełną
sekwencję framing 2 → bootloader protocol 2 → trial framing 3 → host confirm,
łącznie z wymuszonym brakiem confirm i automatycznym rollbackiem.

`main_v2.cpp` jest źródłem obrazów slot A/B i paczki update. Nie potwierdza
startu automatycznie. Po poprawnej inicjalizacji wystawia capability trial
confirm, a `ConfirmTrialBoot` zapisuje metadane wyłącznie dla aktualnie
wykonywanego, uzbrojonego i już uruchomionego slotu. Zapis jest idempotentny po
utracie odpowiedzi. Brak confirm przez 30 s albo błąd lokalnego startu powoduje
restart, aby bootloader wykonał rollback. Logika i pełne buildy przechodzą.
Fizyczne update przez RS485 sprawdziły zapis nieaktywnego slotu, rollback bez
confirm, promocję dokładnie dopasowanego obrazu 0.5.4 oraz kolejną migrację do
0.5.5 przez Lifecycle Service i ograniczony mostek capabilities schema 1.

### Etap 6 — adapter RS485 i test HIL jednej linii

Zakres: jedyny właściciel portu, timeouty, turnaround half-duplex, stabilne
bindingi urządzeń, jedna i wiele node na linii.

Brama: soak, szum/CRC, odłączenie i powrót node, restart procesu oraz pomiar
rzeczywistego wykorzystania łącza na długim przewodzie.

`ChannelPort` otwiera konkretny port 8N1, używa stałych buforów i drenuje TX.
`ChannelEndpoint` składa go z agentem i zegarem. `sensor-hil` realizuje
fail-closed handshake, trial confirm oraz scenariusz Start → pull → durable
`fsync` → release → Stop → Maintenance. Test 0.5.5 zapisał 256 próbek bez gap,
rozliczył końcowy `ProcessingReset` i zrównał cursory received, durable i released
na 257. Pozostają testy długiego przewodu, wielu node, zakłóceń i soak.
`sensor-hil capture-cycle` oraz ręczny skrypt HIL zapisują ten sam pion przez
produkcyjny `CaptureSegmentStream`, wymuszają wiele krótkich okien i niezależnie
walidują każdy WAL z manifestem. Pozwala to uruchomić test bez czekania na
domyślne dziesięciominutowe okno; polityka produkcyjna nie jest zmieniana.
Fizyczny przebieg zapisał 2505 nowych próbek w 10 oknach, bez gap, błędów decode
i timeoutów, zrównał cursory na 5273 i zakończył w Maintenance. Runner odczytuje
stan strumienia przed Start, więc kolejne ręczne sesje nie tworzą fałszywego
`JournalOverflow` od cursora zero.

### Etap 7 — trwały Capture i recovery

Najpierw benchmarkujemy prosty append-only journal/WAL oraz HDF5. Wybór formatu
nie jest jeszcze zamrożony. Kryteria to trwałość, recovery po utracie zasilania,
koszt flush/fsync, rozmiar i łatwość walidacji.

Brama: fault injection przed i po append/flush/commit; po restarcie cursor nie
wyprzedza danych; okno zaczyna się na prawdziwej próbce i zachowuje gaps/timing.

`CaptureWindowLedger` implementuje niezależną od formatu część tej bramki.
Waliduje jeden wspólny cursor space próbek, `Gap` i `ContinuityBreak`, planuje
rotację bez dzielenia wire chunku i udostępnia manifest dopiero przy commit
planu po durable append. Manifesty mają jawne zakresy `[first_cursor,
end_exclusive)` oraz ordinal; verifier odrzuca ukrytą dziurę, overlap i zamianę
sensorów. Odrzucony zapis może porzucić plan bez przesunięcia ledgera.

`CaptureWalSink` implementuje wersjonowany nagłówek, packed-int24 records,
CRC32C, `sync_data`, trwały seal i odtwarzanie ledgera ze stałą pamięcią.
Niepełny fizyczny ogon jest obcinany, natomiast pełny rekord z błędnym CRC
zatrzymuje recovery fail-closed. Writer zostaje zatruty po niepewnym błędzie I/O
i wymaga ponownego otwarcia, co zapobiega podwójnemu appendowi. Bounded
`CaptureSegmentStream` wymusza jeden WAL na okno, publikuje manifest atomowym
rename po synchronizacji i odtwarza go po crashu. `CaptureRepository` rozdziela
skonfigurowane identity/boot epoch wielu node. `CaptureWalReader` i
`validate_capture_segment` czytają blokowo, odtwarzają ledger i wymagają pełnej
zgodności WAL z manifestem. Szczegóły i pozostałą bramkę opisuje
[CAPTURE-WAL-V1.md](CAPTURE-WAL-V1.md).

`CaptureWalBatch` rozdziela przygotowanie rekordów od bariery trwałości dla
jednego fizycznego WAL. Batch nie zmienia durable cursora przed `sync_data`,
odrzuca stary stan i kończy się na granicy okna. `CaptureRepositoryBatch`
wiąże tę operację z jednym skonfigurowanym strumieniem. Executor celowo nadal
używa grup jednoelementowych, dopóki recovery nie sklasyfikuje wszystkich
`WriteJobId` niepewnej grupy.
`classify_group_recovery` potrafi już rozdzielić odzyskany pełny prefiks od
suffixu wymagającego ponowienia i zatrzymuje się fail-closed na cursorze
wewnątrz rekordu lub nieciągłych zakresach.
`GroupedPersistenceWorker` zachowuje wszystkie joby niepewnej grupy, potwierdza
odzyskany prefiks i ponawia tylko suffix. Grupowy kontrakt storage pozostaje
niezależny od WAL, a `CaptureRepository` jest jego pierwszym adapterem.
`GroupedPersistenceExecutor` dodaje wątek, wspólny limit rekordów w locie,
konfigurowalne `max_records`/`max_wait` i jawny wynik operacji recovery.
`RecordingPersistence` odcina pętlę kanału od konkretnego executora, a
`CaptureRuntimeConfig` wymaga jawnej polityki i uruchamia wariant grupowy.

`ArchiveCoverageIndex` opisuje relację many-to-many pomiędzy segmentami Capture
i produktami Archive. `CaptureCoverageCheckpointStore` utrwala pełny dowód
pokrycia, wszystkie tożsamości produktów i deadline 48 h od najpóźniejszego
acknowledgement. Brak dowodu, luka zakresu, niespójna tożsamość, overflow albo
uszkodzony stan blokują usunięcie. Szczegóły opisują
[ARCHIVE-COVERAGE-INDEX.md](ARCHIVE-COVERAGE-INDEX.md) i
[RETENTION-CHECKPOINT.md](RETENTION-CHECKPOINT.md). Cleanup Capture wykonuje
unlink WAL → unlink manifestu → sync katalogu → update checkpointu, uzgadnia
crash i chroni aktywne okno writera. Osobny checkpoint Archive utrwala
tożsamość produktu, deadline i pełny zbiór zależnych segmentów, a cleanup wymaga
trwałego `deleted_at` każdego Capture przed usunięciem danych i coverage index.
Pozostają wpięcie procesu compactora/uploadera, testy na rzeczywistych danych i
rozszerzony fault injection procesu.

`BoundedPersistenceWorker` zamraża ograniczoną własność rekordów i completion,
backpressure, korelację `WriteJobId` oraz fail-closed po błędzie sinka.
`PersistenceExecutor` przenosi sink na osobny wątek, a `CaptureRuntimeConfig`
wykonuje recovery i zachowuje snapshot cursorów przed przekazaniem własności.
Niejednoznaczny błąd I/O jest rozstrzygany przez porównanie zakresu pending
jobu z odzyskanym durable cursorem; rekord już obecny nie jest dopisywany drugi
raz.
Pozostałą integrację opisują
[BOUNDED-PERSISTENCE-WRITER.md](BOUNDED-PERSISTENCE-WRITER.md) oraz
[CAPTURE-RUNTIME-STARTUP.md](CAPTURE-RUNTIME-STARTUP.md).

Pierwszy powtarzalny benchmark syntetyczny opisuje
[STORAGE-FORMAT-BENCHMARK.md](STORAGE-FORMAT-BENCHMARK.md). Wskazał też źródło
problemu RAM starego compactora: zbiór jednej tuple tożsamości na każdą próbkę
całej doby. Roboczym kierunkiem jest nieskompresowany packed-int24 WAL dla
Capture oraz blokowy int32 byte-shuffle + zstd dla Archive. Archive pozostaje
decyzją warunkową do testu na prawdziwych danych; fizyczny nośnik Raspberry Pi
musi wyznaczyć politykę group commit i liczbę workerów.

### Etap 8 — synchronizacja czasu

Zakres: źródłowe obserwacje node monotonic ↔ host monotonic ↔ UTC, niepewność,
holdover, timing anchors i effective ODR. Zachowujemy sprawdzone idee DRDY bez
kopiowania starego formatu protokołu.

Pierwszy model domenowy waliduje surowe `t1..t4`, odejmuje czas przetwarzania
node, wyznacza konserwatywną niepewność, pilnuje `boot_epoch` i buduje bounded
model `Acquiring/Locked/Holdover` z limitem dryfu. Nie tworzy UTC i nie ukrywa
historii obserwacji. Kontrakt oraz kolejność następnego pionu opisuje
[TIME-SERVICE.md](TIME-SERVICE.md).

Firmware 0.5.6 chwyta `t2` przy przyjęciu kompletnego requestu i uzupełnia `t3`
dopiero bezpośrednio przed `start_tx`. Hostowy `ChannelAgent` zachowuje `t1` po
pełnym drain TX, dołącza `t4` po przyjęciu skorelowanej odpowiedzi i zwraca
typowany `ClockObservation`. Time, Measurement i Lifecycle korzystają z jednego
`TransactionCore`, więc nie wykonują równoległych transakcji na magistrali.

`ChannelRuntime` utrzymuje osobną konfigurację i stan deadline TimeSync dla
każdego node, ale przekazuje kandydatów do tego samego `ChannelScheduler`, co
Measurement. Dzięki temu nie istnieje drugi arbiter magistrali. Zagrożenie
overflow próbek zachowuje pierwszeństwo, synchronizacja wykonuje się przed
upływem własnego deadline, a błąd próby zwalnia kanał i stosuje bounded retry.
Pierwsze deadline mogą być rozłożone pomiędzy node, więc start systemu nie
tworzy burstu synchronizacji. Produkcyjny koordynator i oba warianty pętli
recording wykonują unified dispatch i rozliczają końcowe eventy TimeSync.
Nieblokująca granica persistence jest gotowa jako `TimeObservationSubmitter`
i bounded queue. Kanał
wykonuje wyłącznie `try_submit`; nie zna filesystemu, `flush` ani `fsync`.
Pełna lub rozłączona kolejka daje jawny `TimeSyncSettlement::Backpressured` i
krótki retry TimeSync, ale nie zatrzymuje Measurement. Synchronizacja danego
node jest również wyłączona, gdy jego `NodeSession` należy do oczekującego
handoff do storage. Compact `TimeWalSink` zapisuje stałe rekordy 36 B
(`t1..t4` + CRC32C), trzyma identity i oba clock epoch raz w nagłówku, grupuje
barierę `sync_data` i odzyskuje wyłącznie niepełny ogon. Pełne uszkodzenie CRC
jest fail-closed. Format opisuje [TIME-WAL-V1.md](TIME-WAL-V1.md).
`TimeRepository` utrzymuje stałą topologię WAL-i jednego host clock epoch, a
`TimePersistenceWorker` wykonuje bounded storage handoff i zatrzymuje się na
pierwszym błędzie. Recovery porównuje record count sprzed próby z ponownie
otwartym WAL-em, więc potwierdza już trwały rekord albo ponawia wyłącznie rekord
nieobecny. `TimePersistenceExecutor` przenosi całe I/O na nazwany wątek,
udostępnia klonowalne submittery kanałom, przenosi bounded completion i ma
kontrolowany shutdown. `TimePersistenceInstallationProfile` buduje stałą
topologię ze zweryfikowanego discovery i epoki zegara hosta, a `TimeRuntime`
kończy recovery wszystkich WAL-i przed udostępnieniem submittera. Konfiguracja
storage i harmonogramu synchronizacji pozostaje rozdzielona.
`TimePersistenceHealth` utrzymuje dokładny niepewny rekord
przez `RecoveryRequired/Recovering` i wraca do `Healthy` dopiero po sekwencyjnym
durable receipt; niespójność przechodzi fail-closed do `IntegrityFault`.
Przyjęcie przez RAM queue samo nie jest durable.

Brama symulacyjna drift/jitter/asymetria/timeout/późna odpowiedź/restart jest
gotowa i przechodzi przez rzeczywisty framing, `ChannelPort`, `ChannelAgent`
oraz `TransactionCore`. HIL jednego node na firmware 0.5.6 potwierdził 8/8
exchange, stały `boot_epoch`, brak decode errors i timeoutów oraz path delay
12,7–17,8 ms przy 115200 bit/s. Następna analiza ma odtworzyć czas i niepewność
bez używania nazw plików ani przypadkowego czasu odbioru pakietu.

Wielokanałowy restart procesu na Raspberry Pi ujawnił szczególny stan
startowy: node C 250 Hz pozostawał w `Recording` z pełną retencją, więc
Measurement miał stale priorytet ochrony przed overflow i TimeSync nie dostał
slotu. Rozwiązanie znajduje się w kompozycji kanału, a nie w wyjątku
schedulera. Takeover zastanego `Recording` wykonuje odtworzenie durable sesji,
`Stop`, drain zatrzymanego ogona, `Start` i dopiero potem normalne planowanie.
HIL po wdrożeniu potwierdził świeży TimeSync na wszystkich pięciu liniach,
zerowy backlog C i brak nowych gapów. Scheduler zachowuje więc niezmiennik, że
TimeSync nie wymusza transakcji kosztem próbki przy zerowej retencji.

### Etap 9 — `HostCoordinator` i skala 1–16 channels

Zakres: konfiguracja topologii, globalny budżet CPU/RAM/storage, staggered start,
health, kontrolowany shutdown i rozpoznawanie wspólnej awarii TransportDevice.

Brama: macierz 1/4/8/16 channels, bounded RSS, brak wspólnej nieograniczonej
kolejki, awaria jednej linii nie zatrzymuje pozostałych, a globalny błąd storage
zatrzymuje commit bez cichej utraty.

Stan implementacji: moduły pojedynczego kanału, Capture i Time mają gotowe
granice oraz testy. `HostInstallationProfile` składa z jednego scalonego
dokumentu osobną politykę storage i typowaną topologię maksymalnie 16 kanałów.
Przed I/O odrzuca duplikaty channel ID, endpointów, adresów i hardware ID,
nieznane transporty, inną prędkość niż 115200, brak board revision oraz output
ODR spoza pełnego zbioru ADXL355. `InstallationDiscoveryPlan` wybiera wyłącznie
aktywne channel/node i wymaga Measurement, Lifecycle, Time, journala oraz timing
DRDY. Wykonawca odpytuje node jednego half-duplex channel kolejno, a niezależne
kanały uruchamia równolegle. Każdy node ma bounded deadline; brak odpowiedzi,
rozłączenie lub błąd jednej linii nie może zwrócić częściowego inventory jako
sukcesu. Collector koreluje wynik z `(channel_id, node_address)` i ponownie
sprawdza identity oraz capabilities.

`ChannelStartupPlan` przekształca kompletne inventory w jawne
`NodeScheduleConfig` i raport admission per kanał. Używa output ODR, pojemności
journala i maksymalnego chunku zgłoszonych przez node, ale czasy transakcji,
rezerwę recovery i limity schedulera przyjmuje jako obowiązkowy profil z
pomiarów. Kanał przekraczający budżet jest odrzucany przed startem runtime.

`GET_STREAM_STATE` jest pełnoprawnym retry-safe zadaniem tego samego
`ChannelAgent`, który posiada handshake, Lifecycle, Measurement i Time. Nie
otwiera drugiego właściciela portu. `stream_recovery` uzgadnia zweryfikowane
identity/capabilities, stan journala node oraz durable cursor odzyskany z
Capture. Odrzuca zmianę epoki lub pojemności, Capture wyprzedzający node i
przede wszystkim `released > durable`. Gdy mały fragment został już nadpisany,
nie przesuwa kursora w pamięci: pozostawia zwykłemu pierwszemu pullowi zapisanie
dokładnej luki `JournalOverflow`. Nowe nagranie dostaje jawny pre-Start boundary
równy rzeczywistemu `produced`, więc wcześniejsze próbki nie są fałszywie
oznaczane jako utracone.

Produkcyjny `ChannelBootstrap` różni się celowo od read-only polecenia
`discover`: zachowuje jeden żywy `ChannelEndpoint` i ten sam port przez
handshake, `GetState` oraz `GET_STREAM_STATE` wszystkich node na linii. Wyniki
mają wspólny bounded deadline per node, a rozbieżna epoka lub pojemność journala
kończy startup fail-closed. `channel_recovery` przygotowuje origin nowego albo
istniejącego WAL, ale tworzy `NodeSession` dopiero z durable snapshotu zwróconego
po pełnym replayu `CaptureRuntime`. Snapshoty są łączone po
`(hardware_id, boot_epoch)`, nigdy według kolejności adresów.
`RecordingActivation` przyjmuje dopiero odzyskane sesje: zachowuje node już w
`Recording + Production`, wysyła `Start` wyłącznie dla `Idle`, a stany
Maintenance/Stopping/Faulted wymagają jawnej operacji. Aktywacja nie może
zmienić żadnego cursora. Końcowy assembler rejestruje w
`ChannelWorkCoordinator` dokładnie cursor należący do `NodeSession` i fazuje
pierwsze deadline TimeSync; nie przyjmuje osobnej wartości kursora.
`compose_channel_recording` spina te granice dla jednego fizycznego kanału:
żywy endpoint → origin/replay Capture → reconciliation → aktywacja Recording →
assembler → `ThreadedChannelRecordingLoop`. Jeśli którakolwiek faza po starcie
storage zawiedzie, executor jest jawnie zamykany, a błąd shutdownu pozostaje w
wyniku. Test pionowy potwierdza, że `produced=140` wybrane jako origin nowego
WAL wraca po replayu jako durable i bez zmiany trafia do schedulera.

`installation_bootstrap` wykonuje produkcyjny bootstrap wszystkich aktywnych
kanałów równolegle i nie zamyka ich portów po zebraniu danych. Dopiero kompletny
wynik jest ponownie sprawdzany przez `DiscoveryInventoryCollector`; błąd jednej
linii odrzuca całą próbę startupu i nie publikuje częściowego inventory.
`HostStartupBlueprint` łączy następnie każdy zachowany endpoint z jego planem
admission po `channel_id`, buduje jedną topologię globalnego Time WAL i kończy
całą walidację przed otwarciem Capture albo wysłaniem `Start`. Dzięki temu
dalszy coordinator nie musi łączyć kanałów według pozycji w wektorze ani
ponownie otwierać portów.

`ChannelSupervisorSet` uruchamia maksymalnie 16 niezależnych, nazwanych
workerów z osobnymi mailboxami sterowania o pojemności jeden i stałym snapshotem
stanu. Próbki ani zdarzenia ramek nie przechodzą przez globalną kolejkę.
Awaria jednego tasku zachowuje jego błąd i dowód shutdownu, ale nie zatrzymuje
pozostałych kanałów; błąd tworzenia późniejszego wątku zatrzymuje i dołącza już
uruchomione workery. Test ośmiu kanałów potwierdza tę izolację.

`start_host_runtime` wykonuje produkcyjny blueprint. Uruchamia dokładnie jeden
globalny `TimeRuntime`, daje każdemu kanałowi bounded submitter, składa jego
Capture i `ProductionChannelTask`, a workery startuje dopiero po przygotowaniu
całego zestawu. Błąd po częściowym przygotowaniu wykonuje kontrolowany shutdown
gotowych kanałów i zamyka Time WAL. Błąd po częściowej aktywacji kanału wykonuje
rollback Lifecycle do `Idle`; wynik rollbacku i zamknięcia Capture pozostają w
błędzie startupu. Globalny shutdown najpierw zatrzymuje i dołącza wszystkie
workery, a dopiero potem zamyka Time WAL.

Test kompozycyjny uruchamia osiem niezależnych kanałów przez prawdziwy framing
Measurement i Lifecycle na deterministycznych magistralach RS485. Każdy worker
wykonuje pracę i pełny protokół Stop/drain/Capture shutdown, a końcowy wynik
potwierdza osiem poprawnie zatrzymanych kanałów oraz jedno repozytorium Time z
ośmioma strumieniami. Jest to bramka programowa przed testem procesu `run` i
pomiarami na Raspberry Pi; nie zastępuje sterownika USB ani fizycznego I/O.

`HostHealthSnapshot` oddziela lifecycle koordynatora od health. Fault jednego
channel daje `Degraded` i pozostawia pozostałe linie aktywne. Time WAL po
pierwszym błędzie wykonuje dokładnie jedną próbę uzgodnienia niepewnego rekordu;
błąd odczytu health, recovery albo integralności rozsyła globalny Stop.
`StorageHealthProbe` używa `statvfs` bez tworzenia plików, sprawdza flagę
read-only oraz konfigurowalną rezerwę `min_free_bytes`. Potwierdzony fault
wspólnego filesystemu zatrzymuje wszystkie linie, natomiast lokalny błąd pliku
jednego kanału pozostaje faultem tego kanału. Pierwsza przyczyna globalna jest
zachowywana i nie może zostać nadpisana późniejszym raportem.

Coordinator pomiarów ma jawny stan `quiescing`: blokuje nowe Measurement i
TimeSync, zachowując możliwość rozliczenia transakcji oraz trwałych zapisów już
będących w locie. Retry-safe `stop_recording_nodes` używa tego samego endpointu
i operation ID aż node osiągnie `Idle`. `ProductionChannelTask` wykonuje pełny
kontrolowany shutdown: bariera istniejącego work, Stop wszystkich node,
wymuszony Measurement Pull każdego zamrożonego journala aż do `NoData`, durable
settlement i dopiero shutdown Capture executor. Drain omija zwykłe oczekiwanie
na wypełnienie chunku, ale zachowuje retry backoff. Osobne deadline bariery,
Lifecycle i drain uniemożliwiają nieskończone blokowanie workera. Test pionowy
zapisuje końcowy chunk `140..141`, potwierdza durable cursor `141`, a następnie
zamyka drain dopiero po `NoData`.

Binarny `sensor-host validate --config ... --repository-root ...` wykonuje
walidację bez otwierania portów. `sensor-host discover` wykonuje równoległy,
read-only handshake wszystkich aktywnych linii i wypisuje zweryfikowane
inventory. `sensor-host run` składa ten sam walidowany profil w produkcyjny
proces, wykonuje preflight storage przed otwarciem RS485, okresowy health check
oraz kontrolowany shutdown po `SIGINT`/`SIGTERM` z raportem per kanał. Biblioteka ma już side-effect-free planowanie oraz produkcyjne
uruchamianie i kontrolowany shutdown 1–16 pętli recording ze wspólnym Time WAL.
Polityka reakcji na globalny fault storage jest podłączona do pętli procesu.
Tracked `runtime_v2` utrzymuje parametry wire, admission, schedulera i shutdownu
w jednym fail-closed kontrakcie. Szablon jednostki systemd v2 jest gotowy, ale
nie jest instalowany ani aktywowany z repozytorium. Następną bramką pozostaje
powtarzalna macierz testów 1/4/8 na Raspberry Pi.

Minimalna kolejność do pierwszego testu Raspberry Pi 1/4/8 channels:

1. **Gotowe:** typowany `HostInstallationProfile` z maksymalnie 16 kanałami,
   stabilnym bindingiem portu, oczekiwanym `hardware_id`, board revision,
   adresem node i wymaganym output ODR;
2. **Gotowe:** read-only discovery oraz równoległy produkcyjny bootstrap całej
   instalacji, zachowujący po jednym właścicielu portu przez
   handshake/`GetState`/`GET_STREAM_STATE`, kompletne inventory i channel
   admission. Profil tuningu jest typowany i walidowany; jego wartości pozostają
   do kalibracji z HIL;
3. **Gotowe:** przygotowanie origin Capture bez zgadywania kursora,
   fail-closed połączenie replayowanego durable snapshotu z node, utworzenie
   `NodeSession` dokładnie na tym cursorze, aktywacja `Idle`/`Recording` i
   złożenie `CaptureRuntime`, coordinatora i `ThreadedChannelRecordingLoop` bez
   ponownego podawania cursora oraz dostarczenie submittera jednego globalnego
   `TimeRuntime` każdemu kanałowi;
4. **Gotowe planowanie i produkcyjna kompozycja nadzoru:** sparowanie żywych endpointów z
   planami kanałów i zbudowanie jednej topologii Time WAL przed Capture/Start;
   niezależne workery mają bounded control/status, izolację awarii, produkcyjne
   pętle oraz wspólny `TimeRuntime`. Start i TimeSync są fazowane per node, a
   Capture i Time zachowują własne bounded group commit;
5. **Gotowe per channel:** shutdown drain obejmujący barierę nowych zadań,
   rozliczenie aktywnego work, Stop do `Idle`, pobranie zamrożonego ogona,
   durable completion i shutdown storage. Globalny coordinator zachowuje tę
   kolejność dla wszystkich kanałów i zamyka Time na końcu. Sonda braku miejsca
   i read-only uruchamia globalny Stop. `SIGINT`/`SIGTERM` używają tej samej
   ścieżki;
6. **Gotowe:** `sensor-host validate`, `discover`, `run`, szablon systemd,
   powtarzalna bramka symulacyjna macierzy 1/4/8 oraz lekkie, lock-free
   snapshoty metryk każdego kanału;
7. **Przygotowane, wymaga sprzętu:** skryptowana bramka i checklista testu na Pi
   kolejno 1, 4 i 8 kanałów z pomiarem RSS, CPU, I/O wait, fsync latency,
   backlogu, retry, luk i niepewności TimeSync.

### Etap 10 — Operations API i Maintenance

Zakres: command/result/progress z `operation_id`, idempotency, audit, start/stop,
config transaction, commissioning i firmware update. UI i CLI są klientami API.

Brama: równoległe/conflicting commands mają deterministyczny wynik, UI nie
otwiera portów, a przerwany update pozostawia bootowalny slot.

Bieżąca kolejność implementacji:

1. **Gotowa domena i executor per node:** bounded command mailbox, idempotentny
   ledger, `SafeStateConverger`, bezpieczne porzucenie control work dopiero po
   kwarantannie wire oraz nieblokujący `SafeStateWireOperation` z weryfikacją
   nowego `boot_epoch` po restarcie. `SafeStateGroupOperation` wykonuje node
   sekwencyjnie przez jednego arbitra half-duplex. Plan startowy zachowuje pełny
   kontrakt handshake dla późniejszego recovery. `MaintenanceEntryPlan` jest
   osobną polityką kolejności quiesce → Idle → reconcile → drain → Maintenance
   oraz awaryjnego powrotu do stabilnego Idle;
2. **Gotowe:** inbox, idempotentny ledger i snapshot statusu są
   podłączone do produkcyjnego workera przez osobny bounded uchwyt per channel;
   `ReadStatus` przechodzi cały pion, błędny routing jest odrzucany przed
   kolejką, a `SafeStateGroupOperation` korzysta z tego samego workera i arbitra
   RS485. Nieobsługiwana jeszcze mutacja daje jawne `ExecutorUnavailable` bez
   zatrzymania Recording;
3. **Wejście i szybki powrót gotowe:** `EnterMaintenance` wykonuje bounded
   quiesce → Idle → kontrolę epoki → drain → Maintenance. Osiągalny node, który
   stale odrzuca stan docelowy, ma ograniczoną liczbę pełnych cykli restartu i
   wraca do stabilnego Idle; nie może zapętlić workera. `ResumeRecordingPlan`
   wymusza kolejność pełna weryfikacja node → opcjonalne stream recovery →
   admission → aktywacja → wznowienie schedulera i nie pozwala ominąć recovery
   po zmianie `boot_epoch`. Nieblokujący wykonawca jest podłączony do workera:
   sekwencyjnie wykonuje pełny handshake, `GetState` i `GetStreamState`,
   odtwarza sesję z trwałego cursora Capture, egzekwuje niezmieniony kontrakt
   admission, aktywuje node i
   dopiero wtedy otwiera scheduler. Pion Recording → Maintenance → Recording,
   również po zmianie epoki i rolloverze Capture/Time, przechodzi przez
   prawdziwy codec w symulacji 1/4/8 channels;
4. odczyt oraz mutacje Configuration per node;
5. wyłączne okno Commissioning per channel i handshake przydzielonego node;
6. **Gotowe:** bounded lokalne IPC, adapter `hostctl`, operacje per node oraz
   trwały audit obejmują Status, Maintenance, Resume, Configuration,
   Commissioning i RestartNode;
7. **Gotowe:** bounded Operator HTTP API i dashboard są cienkimi klientami tego
   samego IPC. UI obsługuje Maintenance, Resume, restart node i kompletny
   formularz Configuration. Configuration z Recording jest worker-owned
   workflow Maintenance → operacja → Resume, a capabilities ograniczają ODR;
8. **Gotowe dla lifecycle usługi:** osobny root-owned helper z allowlistą
   steruje start/stop/restart recordera i timera eksportu. Instalator publikuje
   checksummowany helper oraz automatyczne jednostki helpera i dashboardu;
9. **Gotowe lokalnie:** prosty panel Commissioning w karcie kanału przyjmuje
   nazwę i adres, wykonuje Maintenance/Scan/Assign, atomowy zapis profilu oraz
   restart hosta; pozostaje HIL z nieprzypisanym node,
   asynchroniczny wynik `doctor` oraz operacje grupowe z wynikiem per target.

### Etap 11 — prosty eksport i migracja produkcyjna

Zakres MVP: walidacja sealed Capture, automatyczny eksport okien do prostego
CSV, porównanie ze
starym recorderem, canary i kontrolowany rollback. Godzinne Archive, kompresja,
publication outbox, uploader i Derived są odłożone poza MVP. Ich wcześniejsze
kontrakty pozostają dokumentacją możliwego rozszerzenia, a nie bieżącą bramką.

Brama: wielogodzinny soak, test utraty zasilania, zgodność liczby próbek/luk,
poprawny eksport CSV i zatwierdzona procedura operacyjna. Stary host usuwamy
dopiero po tej bramie.

## 7. Otwarte decyzje

Nie wolno rozstrzygać ich przypadkową stałą w kodzie:

- pojemność sample ingress oraz mailboxów command/result;
- pojemność `GapLedger` i próg kontrolowanego collapse prefiksu retencji;
- rozmiar journala node oraz gwarantowany czas retencji;
- algorytm i wagi schedulera;
- częstotliwość time sync i timing anchors;
- format Capture, chunking, kompresja i polityka fsync;
- docelowe prawa dostępu i gateway zdalny dla lokalnego Unix Operations IPC;
- kryterium wydzielenia TransportDevice do osobnego procesu;
- profile admission control dla różnych Pi, SD/SSD i topologii;
- fizyczny format Archive, poziom kompresji, parametry retencji i replikacji.

Każda decyzja musi mieć właściciela, pomiar albo test oraz miejsce konfiguracji.
Zmiana niezmiennika wymaga ADR.

## 8. Reguły pracy dla następnej osoby

1. Przeczytaj `docs/README.md`, baseline i ten dokument.
2. Sprawdź realny kod; roadmapa opisuje stan, ale kompilator i testy rozstrzygają
   szczegóły implementacji.
3. Podejmij pierwszą nieukończoną bramkę zależności. Nie twórz modułów z
   późniejszych etapów „na zapas”.
4. Dodaj trait tylko dla rzeczywistego portu I/O, drugiego adaptera albo fake'a
   potrzebnego w teście zachowania.
5. Każdy bufor ma limit, przepełnienie i obserwowalny skutek.
6. Każdy stan i cursor ma jednego właściciela.
7. Zewnętrzny błąd jest typowany; nie sterujemy domeną przez tekst logu.
8. Po zmianach Rust uruchom:

```bash
cargo fmt --all --manifest-path host_v2/Cargo.toml --check
cargo clippy --workspace --all-targets --manifest-path host_v2/Cargo.toml -- -D warnings
cargo test --workspace --manifest-path host_v2/Cargo.toml
```

9. Po zmianach starego hosta uruchom:

```bash
host/.venv/bin/python -m unittest discover -s host/tests
```

10. Po zmianach firmware uruchom:

```bash
bash node/tests/run_host_tests.sh
```

11. Nie wdrażaj firmware z pominięciem walidacji board revision.
12. Aktualizuj sekcję „Stan implementacji” i status etapu w tym dokumencie, ale
    nie przepisuj historii ADR.

## 9. Następna konkretna praca

### Audyt odpowiedzialności 2026-10-06

Kierunek zależności i właściciele stanu pozostają poprawne. `sensor-core` nie
zna I/O, `sensor-protocol` zależy tylko od typów wartości `sensor-core`, a
`sensor-host` składa oba z adapterami. Firmware zachowuje jednego właściciela
sensora na core 0 oraz journala i protokołu na core 1. Nie znaleziono drugiego
właściciela cursora, sensora ani aktywnej transakcji.

Rozmiar pliku nie jest sam w sobie powodem do tworzenia kolejnej klasy.
`ChannelAgent` ma 552 linie kodu produkcyjnego, a dalszą część stanowią testy;
`NodeSession` ma jedną spójną odpowiedzialność: pilnuje cursorów, trwałości i
recovery jednego node. Oba pozostają na miejscu.

Obowiązują natomiast dwie granice rozbudowy:

- przed dodaniem Time Service, Health albo Configuration do arbitra powstaje
  `ChannelScheduler`, który posiada kolejki, priorytety, fairness, retry budget
  i admission. Wykonawca kanału nadal posiada dokładnie jeden
  `TransactionCore` i wykonuje jedną wybraną pracę;
- `LifecycleServiceHandler` może nadal wspólnie obsługiwać korelację,
  idempotency i bounded cache dla obecnych dwóch operacji systemowych. Przed
  dodaniem następnej operacji systemowej routing oraz jej polityka
  post-transmit muszą przejść do osobnego handlera usług systemowych.

Bufory ramek i firmware są statycznie ograniczone. Hostowe `VecDeque` i
dekodowane `Vec<SampleAxes>` korzystają z heapu, lecz ich rozmiary są ograniczone
przez politykę i kontrakt wire. Host v2 nie jest więc całkowicie allocation-free;
na Raspberry Pi jest to świadoma własność, którą trzeba zmierzyć w teście
obciążeniowym zamiast opisywać jako gwarancję pamięci statycznej.

Nazwy `Start`/`Stop` w bieżącym Lifecycle oznaczają start/stop akwizycji node.
Nie są tym samym co przyszłe `start_recording`/`stop_recording` hosta. Przed
wystawieniem tych operacji node przez publiczne API nazwy typów domenowych
trzeba ujednoznacznić do
`StartAcquisition`/`StopAcquisition`; numery messages na wire mogą pozostać bez
zmian.

`MeasurementJournal`, ścieżka delivery i izolowany endpoint transportu są gotowe
oraz opisane w
[MEASUREMENT-JOURNAL.md](MEASUREMENT-JOURNAL.md),
[DELIVERY-SESSION.md](DELIVERY-SESSION.md) oraz
[MEASUREMENT-HANDLER.md](MEASUREMENT-HANDLER.md) i
[PROTOCOL-V3-TRANSPORT-ENDPOINT.md](PROTOCOL-V3-TRANSPORT-ENDPOINT.md).

ADR-0009, `MeasurementJournal`, typowany wynik `GAP` w Rust/C++ oraz
`DeliverySession` domykają źródłową sekwencję, dokładne luki, retransmisję i
kontrolowane przepełnienie retencji. Stary ciągły `SampleJournal` został usunięty.
Bounded sample/gap ingress core 0 → core 1 oraz mailbox command/result core 1 ↔
core 0 są gotowe jako czysta logika. Mailbox opisuje
[CORE-OPERATION-MAILBOX.md](CORE-OPERATION-MAILBOX.md). Czysta kompozycja
`SensorRuntime`, mailboxa, ingress i właściciela journala jest gotowa oraz
opisana w
[NODE-V2-RUNTIME-COMPOSITION.md](NODE-V2-RUNTIME-COMPOSITION.md). Pozostałe
operacje są zapisane jako braki w dokumencie mailboxa i będą dodawane pionowymi
fragmentami. Nie adaptujemy starego grant/burst/commit.

Brak opisany w
[ACQUISITION-QUALITY-AND-DIAGNOSTICS.md](ACQUISITION-QUALITY-AND-DIAGNOSTICS.md)
jest domknięty w czystej logice. `FifoReadOutcome` wydziela wynik pojedynczego
serwisu FIFO zamiast odczytywania późniejszego snapshotu counters.
`AcquisitionEngine` emituje `SensorOverflow`, `SensorReadUncertain` i
`InvalidTiming` przez `MeasurementIngressSink`. `SensorRuntime` z lifecycle
`Idle`, `Recording`, `Stopping`, `Maintenance`, `Faulted` oraz niezależnym profilem
`Production`/`Diagnostic` jest gotowy i przetestowany razem z mailboxem. Jest
używany przez obrazy A/B i integracyjny `main_v2`; legacy `main.cpp` nadal
korzysta ze starej ścieżki.

Lifecycle Service ma zgodne kodeki C++/Rust, routing przez dispatcher, jedną
aktywną operację i bounded cache terminalnych wyników. Ponowienie tego samego
`operation_id` nie duplikuje wykonania, konflikt treści jest odrzucany, a
`Stop/InProgress` jest uzgadniany aż do wyniku terminalnego.

`Core1ProtocolRuntime`, `node_v2::DataPlane` i `node_v2::RuntimeComposition` są
gotowe oraz przetestowane. Kompozycja ma jeden jawny composition root, a
tymczasowy `RuntimeCapacityProfile` centralizuje wartości potrzebne do builda i
pilnuje budżetu ingress/journala przez `static_assert`.

Odroczone przejście do bootloadera i zdalny restart po RS485 są gotowe.
`PrepareFirmwareUpdate` jest dozwolone z `Maintenance` oraz `Faulted`.
`RestartNode` w firmware 0.5.8 jest ratunkowo dostępny w każdym stanie, również
przy zakleszczonym `Stopping` lub fault handlera Lifecycle. Akcja następuje
dopiero po zakończeniu transmisji odpowiedzi. Zwykłe operacje nadal dochodzą do
`Idle` i `Maintenance` bez restartu; eskalacja ma jawnie zakończyć boot epoch.

Ogólny kontrakt post-transmit jest już gotowy w dispatcherze, response outbox i
`TransportEndpoint`. Zachowuje token do zakończenia TX, zatwierdza go po
`tx_completed` i anuluje po błędzie kodowania lub timeout.

`PrepareFirmwareUpdate` i `RestartNode` są osobnymi operacjami systemowymi.
Timeout odpowiedzi nie ustawia akcji, a potwierdzony TX uzbraja dokładnie jedno
`EnterBootloader` albo `RestartNode`. Osobny obraz
`sensor_system_node_v2_integration` pozostaje diagnostycznym obrazem bez A/B.
Produkcyjne obrazy slotów A/B składają ten sam runtime protocol 5 i są źródłem
paczki update.

Aktualny build integracyjny board rev 2 ma 128404 B BSS wobec 289772 B starego
obrazu direct. To punkt pomiarowy, a nie zamrożony budżet.

Fizyczne scenariusze migracji wykonane wyłącznie przez RS485 potwierdziły wejście
z aplikacji legacy `0.4.1` do bootloadera protocol 2, zapis całego obrazu do
nieaktywnego slotu, automatyczny rollback po braku potwierdzenia oraz poprawną
promocję obrazu protocol 5 `0.5.4`. Host zatwierdził trial dopiero po zgodności
hardware ID, board revision, protokołu, capability i wersji firmware. Runtime v2
używa jawnego stosu core 1 o rozmiarze 8 KiB w głównym SRAM.

Kolejna aktualizacja `0.5.4` → `0.5.5` potwierdziła wejście do bootloadera przez
Lifecycle Service protocol 5, pełny zapis 153088 B z kontrolą CRC, handshake
capabilities schema 2 i trial confirm. USB zapewniało jedynie zasilanie.

Klient Lifecycle Service jest gotowy w czystej logice. Zachowuje jeden
`operation_id` przez wiele wire `transaction_id`, odróżnia application `Busy`
od lifecycle `InProgress`, respektuje quarantine po timeout i fail-closed
odrzuca odpowiedź z innym ID. Profil recording handshake wymaga Lifecycle.
Pierwszy pion `ChannelAgent` jest gotowy w czystej logice. Ma jedną kolejkę
ograniczaną przez politykę oraz jeden `TransactionCore`; wykonuje Identity/Capabilities
handshake i Lifecycle dla wielu node, obraca node bez nakładania transakcji i
zachowuje retry przez timeout/quarantine. `ChannelPort` i `ChannelEndpoint`
składają stałe bufory, stream decoder, częściowy write, drain TX i zegar timeoutu.
Adapter przeszedł fizyczny HIL jednej linii. Naprawa `ChannelPort` eliminuje
ponowne dekodowanie zużytych bajtów po timeoutcie pomiędzy fragmentami ramki.

Measurement Service jest włączony do tego samego arbitra. `MeasurementTransfer`
zachowuje pełny `SampleChunk`, w tym `config_revision` i timing evidence.
`DurableMeasurementSink` blokuje przesunięcie durable/release przy błędzie
storage. Capture WAL, recovery, segmentacja i walidacja są gotowe; checkpoint
retencji, bounded writer, handoff coordinatora, threaded executor i
parser-independent runtime startup oraz tracked parser profilu są gotowe.

Następne fragmenty mają stałą kolejność zależności:

1. **Gotowe:** proces `sensor-host run` wokół `HostCoordinator`, sygnały,
   okresowy health, globalna reakcja storage i raport końcowego shutdownu.
2. **Gotowe w symulacji i wystawione przez IPC:** Resume po nowym `boot_epoch`; worker przygotowuje
   granicę Capture, koordynator rejestruje Time WAL, a dopiero po obu durable
   wynikach wymieniane są `NodeSession` i cursory schedulera. Maintenance i
   Resume są dostępne przez `hostctl channel` bez dostępu klienta do RS485.
3. **Gotowe dla procesu zastającego Recording:** takeover zatrzymuje node,
   drenuje zatrzymany ogon do trwałego cursora, ponownie uruchamia akwizycję i
   dopiero wtedy wznawia Measurement/TimeSync.
4. Rozszerzyć gotowy szczęśliwy pion symulacyjny ośmiu niezależnych kanałów o
   awarię jednej linii, opóźniony storage oraz globalny storage fault.
5. Częściowo gotowe: Raspberry Pi z pięcioma aktywnymi liniami potwierdził
   czysty restart i automatyczny eksport. Dokończyć jawne gate 1/4/8 oraz
   zmierzyć RTT, turnaround,
   decode errors, retry, RSS, CPU, I/O wait, fsync latency, backlog i TimeSync.
6. Powtórzyć HIL z wieloma node na jednym kanale, długim przewodem,
   kontrolowanymi zakłóceniami, restartem procesu hosta i utratą zasilania.
7. **Gotowe na danych Raspberry Pi:** strumieniowy eksport Capture do CSV
   zachowuje próbki, jawne luki, continuity breaks i ODR; potwierdzić jeszcze
   otwieranie wyniku w MATLAB/Python. Archive, kompresję i uploader wykonać
   dopiero po MVP, jeśli będą potrzebne.
8. Na końcu dostroić wyłącznie profile pojemności na podstawie pomiarów; nie
   zmieniać przy tym domeny ani formatu Capture.

Pierwszy fizycznie zatwierdzony obraz protocol 5 miał wersję `0.5.6`,
capabilities schema 2 i reklamował Time Service. Migracja `0.5.4` → `0.5.5`
przeszła w całości po RS485: fail-closed identity,
ograniczony do dokładnej wersji mostek schema 1, Idle → Maintenance, wejście do
bootloadera, zapis i CRC 153088 B, start trial oraz confirm po pełnym handshake.
Następnie cykl Measurement zebrał 256 kolejnych próbek bez gap i zakończył się
w Maintenance. Każda następna paczka trial musi otrzymać unikalną wersję, aby
host nie mógł pomylić nowego obrazu z rollbackiem do poprzedniego slotu.
Aktualizacja `0.5.5` → `0.5.6` również przeszła po RS485 przez Lifecycle
`prepare-update`, zapis nieaktywnego slotu, trial i exact-version confirm.

### Raspberry Pi — pierwszy wielokanałowy gate

Na Raspberry Pi 5 wykonano migrację sześciu fizycznych node board rev 2 ze
starego firmware do `0.5.6` wyłącznie przez RS485. Każda linia przeszła osobno
pełny upload A/B, odczyt Identity/Capabilities i exact-version trial confirm.
Aktywne były linie A, B, C, D, G i H; E oraz F nie były fizycznie podłączone.

Pierwszy jednokanałowy gate ujawnił po kolejnym uruchomieniu procesu, że
`SystemMonotonicClock` liczył czas od startu procesu, chociaż Time WAL wiąże oś
czasu z linuxowym `boot_id`. Powodowało to prawidłowe fail-closed odrzucenie
niemonotonicznej obserwacji po restarcie hosta. Zegar produkcyjny używa teraz
linuxowego `CLOCK_BOOTTIME`; bootstrap sprawdza jego dostępność przed otwarciem
runtime, a test regresji potwierdza, że utworzenie nowej instancji zegara nie
zeruje wartości.

Po poprawce zaliczono 60-sekundowy gate sześciu kanałów i kolejny 30-sekundowy
gate restartu na tych samych Capture i Time WAL. Linia C pracowała z output ODR
250 Hz, pozostałe z 125 Hz. Oba przebiegi zakończyły się kontrolowanym Stop i
pełną walidacją wszystkich strumieni: zero gapów, timeoutów, decode errors,
retry, storage failures oraz backpressure. Dłuższy przebieg zużył maksymalnie
2592 KiB RSS, 0.40 s łącznego czasu CPU przez 60.24 s i nie użył swapu.
Artefakty pozostają lokalnie poza Git. To jest baseline dla sześciu fizycznych
linii, nie końcowa kwalifikacja 1/4/8 ani test długotrwały.

Późniejszy rollout firmware 0.5.8 i automatycznego eksportu CSV pracuje na
liniach B/C/D/G/H; A jest lokalnie wyłączony do jednorazowego restartu
zasilania, a E/F nie są podłączone. Restart procesu 2026-10-16 ujawnił, że
bezpośrednie przejęcie node C nagrywającego przy 250 Hz i pełnym journalu może
utrzymywać Measurement w recovery priority bez slotu TimeSync. Okno 66
zachowało 114181 jawnych braków `JournalOverflow`, więc wynik nie został
uznany za stabilny. Po dodaniu automatycznego `Stop → drain → Start` dla
zastanego `Recording` powtórny restart dał 34 Doctor OK, zero ostrzeżeń i
świeży TimeSync na wszystkich pięciu node. Okno 67 zamknięte przy tym restarcie
ma 141965 próbek, zero braków i jeden oczekiwany `ProcessingReset`.

Ostateczny profil pojemności wymaga pomiarów HIL przy docelowym ODR, obciążeniu
core 1, retransmisjach i najdłuższej przerwie w odpytywaniu node. Wartości
startowe pozostaną stałymi kompozycji, aby strojenie nie przebudowywało domeny.

Przejście od tego baseline do automatycznej pracy na Raspberry Pi, natywnego
`hostctl doctor`, Operations IPC i testów 24/7 opisuje
[HOST-V2-USER-ADOPTION-ROADMAP.md](HOST-V2-USER-ADOPTION-ROADMAP.md). Jego Etap 1,
wersjonowany snapshot statusu, jest gotowy. Rollover Capture + Time po restarcie
node przechodzi pełną symulację, a podstawowy pion Operations IPC jest gotowy.
Pion Configuration per node jest podłączony przez ten sam control plane:
Get/Validate/Apply/Reset przechodzą od `hostctl`, przez ograniczone IPC i
worker-owned ledger, do jedynego `ChannelAgent` i wracają jako typowane wyniki.
Apply/Reset wymagają Maintenance i revision, przed wire przeliczają admission
całego kanału, po sukcesie zapisują trwały snapshot konfiguracji, rekonfigurują
quiescent scheduler i zastępują plan używany przez Resume. Bootstrap odczytuje
rzeczywistą konfigurację node, więc restart hosta rozstrzyga przerwę pomiędzy
trwałą zmianą node i odpowiedzią hosta. Commissioning i RestartNode są
podłączone przez ten sam bounded control plane. Fizyczny HIL firmware 0.5.8 na
kanale B potwierdził Get/Apply/Reset, pusty ScanUnassigned, RestartNode, zmianę
epoki, rollover i ResumeRecording. Trwały per-channel audit zapisuje każde
przejście operacji przed wykonaniem i po wyniku. Bieżącą bramką MVP jest
kwalifikacja pięciu linii: przebieg 1 h, następnie 6 h i 24 h, wraz z testami
kill procesu, restartu Raspberry Pi, odłączenia node i utraty konwertera.
Scan/Assign z rzeczywistym nieprzypisanym node pozostaje osobnym gate
commissioning i nie blokuje testu obecnej topologii. Dashboard pozostaje poza
MVP do czasu zakończenia kwalifikacji recorderowej.

Bench HIL 2026-10-19 wykrył przerwę między trwałym zapisem adresu i odebraniem
końcowej odpowiedzi AssignAddress. Wersja 0.5.9 uznaje ponowne przypisanie tego
samego, już zapisanego adresu za idempotentny sukces i po wysłaniu odpowiedzi
wykonuje wymagany restart. Chroni to commissioning po restarcie procesu hosta
lub utracie odpowiedzi na przewodzie.

### Telemetry — temperatura ADXL355

Pion jest oznaczony jako firmware **0.5.10**; 0.5.9 pozostaje ostatnią wersją
bez usługi Telemetry.

Przyjęty kontrakt opisuje
[TELEMETRY-SERVICE.md](../protocol/TELEMETRY-SERVICE.md). Temperatura jest
rzadkim, osobnym strumieniem i nie powiększa próbek XYZ. Core 0 docelowo
odświeża statyczny cache, a protocol 5 zwraca gotowy snapshot o stałym koszcie;
host odpytuje node co 5–10 minut i rozkłada zapytania w czasie.

Sterownik ADXL355 został utwardzony przed włączeniem usługi: jawnie włącza i
weryfikuje blok temperatury, odrzuca niefizyczne kody oraz wymaga dwóch
zgodnych odczytów. Test jednostkowy obejmuje dekodowanie, punkt 25 °C, skrajne
wartości SPI i kryterium spójności. Pełny zestaw testów logiki node przechodzi.

Stan pionu:

1. **Gotowe:** statyczny snapshot cache i okresowy wykonawca na core 0. Błąd
   kolejnego odczytu nie zastępuje ostatniej poprawnej wartości.
2. **Gotowe:** codec oraz handler `Telemetry Service` po obu stronach protocol
   5. Odpowiedź ma stałe 14 B i nie wykonuje SPI w obsłudze RS485.
3. **Gotowe i podłączone w production:** kandydat Telemetry korzysta z tego samego
   arbitra co Measurement i TimeSync. Measurement pracuje pomiędzy terminami,
   a timeout lub `Busy` zwalnia kanał i planuje odłożoną próbę. Harmonogram
   powstaje wyłącznie dla node deklarujących usługę Telemetry, jest fazowany
   per node, a jego koszt wchodzi do admission control kanału.
4. **Gotowy format, routing, bounded worker, recovery i production runtime:** osobny Telemetry WAL
   zapisuje 16 B na obserwację (`device_time_us`, surowy kod temperatury,
   CRC), rozdziela epoki startu node, deduplikuje ten sam cache i ucina
   wyłącznie niepełny ogon.
   Repository nie miesza strumieni node, idempotentnie tworzy kolejną epokę,
   a osobny storage worker przyjmuje dane przez ograniczoną kolejkę, raportuje
   durable receipt i opróżnia bounded completions podczas shutdownu. Globalny
   runtime uruchamia go przed workerami kanałów, nadzoruje błędy zapisu i
   zamyka po kanałach. Restart node wykonuje idempotentny rollover Telemetry na
   wyłącznym wątku storage przed wznowieniem schedulera.
5. **Gotowa konfiguracja interwału:** `runtime_v2.telemetry_interval_seconds`
   ma produktową wartość domyślną 600 s i może być nadpisane przez profil
   instalacji. Slot wire i retry pozostają wewnętrzną polityką. Pierwszy odczyt
   następuje po pełnym interwale i jest przesunięty o jeden slot na kolejny node.
   Runtime Status i dashboard pokazują ostatnią temperaturę, wiek odczytu oraz
   rozróżniają oczekiwanie na pierwszy snapshot od firmware bez capability
   Telemetry; odświeżanie UI nie generuje ruchu RS485. HIL na node B z firmware
   0.5.10 potwierdził poprawne wartości około 25,9 °C oraz brak timeoutów,
   retry, backpressure, gapów i wpływu na ciągłość próbek. Pozostał eksport
   temperatury obok okna analitycznego.
