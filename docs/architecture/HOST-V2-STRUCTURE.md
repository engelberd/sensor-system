# Docelowa struktura hosta v2

Status: zatwierdzony plan modułów; zaimplementowane protocol, sesja node i
pierwszy pion endpointu kanału. Audyt zgodności: 2026-10-06.

Aktualny stan wykonania oraz kolejność modułów opisuje
[IMPLEMENTATION-ROADMAP.md](IMPLEMENTATION-ROADMAP.md). Niezależne od
implementacji wymagania źródłowego zapisu, retencji, topologii i
maintenance definiuje [PRODUCT-INVARIANTS.md](PRODUCT-INVARIANTS.md).

Dokument sprawdza strukturę hosta v2 względem wszystkich znanych zadań produktu.
Drzewo katalogów nie jest listą klas do utworzenia z góry. Moduł powstaje, gdy
implementujemy pierwszy należący do niego pionowy fragment.

## 1. Zasady struktury

1. Każdy zmienny stan ma jednego właściciela.
2. `sensor_core` nie zna wire format, serial, HDF5, HTTP, systemd ani plików
   konfiguracyjnych.
3. `sensor_protocol` koduje i dekoduje wiadomości. Może używać niezmiennych
   typów wartości z `sensor_core`, ale nie podejmuje decyzji o retry, commit,
   konfiguracji ani trybie pracy.
4. Adapter tłumaczy mechanizm zewnętrzny na port rdzenia. Nie zawiera polityki
   produktu.
5. `apps` są composition roots: składają gotowe komponenty i zarządzają ich
   lifecycle, ale nie przechowują logiki domenowej.
6. Data products mają wersjonowane kontrakty niezależne od runtime recordera.
7. Operacje operatora przechodzą przez jeden control plane z `operation_id` i
   audytem. UI nie otwiera portu RS485.
8. Narzędzia laboratoryjne mogą obserwować i symulować, ale nie są importowane
   przez produkcję.
9. Nie powstają katalogi `common`, `utils`, `helpers` ani klasy `Manager` bez
   jednej nazwanej odpowiedzialności.

## 2. Fizyczne drzewo pierwszej iteracji

Nie odwzorowujemy całego modelu domeny na osobny crate ani klasę. Początkowy
workspace ma trzy jednostki kompilacji:

```text
host_v2/
  Cargo.toml
  crates/
    sensor-core/src/               # model i polityki bez I/O
    sensor-protocol/src/           # wersjonowany kontrakt wire
    sensor-host/src/               # runtime, adaptery i composition root
  contracts/golden/                # wspólne wektory Rust/C++
  python/
    sensor_data/                    # odczyt i walidacja offline
    tools/                          # narzędzia offline
```

Poniższa mapa pokazuje docelowe odpowiedzialności. Nazwy `.rs` są planowanymi
modułami wewnątrz trzech crate'ów, a nie listą plików do utworzenia z góry.

```text
sensor-protocol/src/                # czyste kontrakty wire
    framing.rs                     # envelope, limity, flagi i typy ramek
    integrity.rs                   # CRC i walidacja integralności
    stream_decoder.rs              # bounded resynchronizacja strumienia
    version.rs                     # framing/application version
    messages/
      discovery.rs                 # hello, identity, capabilities
      transfer.rs                  # pull, sample chunk i release cursora
      time_sync.rs                 # obserwacje zegara
      configuration.rs             # desired/applied/revision
      diagnostics.rs               # zwarte health i bounded diagnostyka
      lifecycle.rs                 # restart/bootloader/acquisition
      commissioning.rs             # unassigned node i przypisanie adresu
    bootloader/                    # osobny minimalny update protocol

sensor-core/src/                    # domena bez I/O i serializacji
    identity.rs                    # HostId, ChannelId, NodeAddress, HardwareId
    health.rs                      # HealthLevel, reasons i evaluatory
    events.rs                      # domain/quality/operational/audit facts
    errors.rs                      # klasyfikacja błędów i retry category
    limits.rs                      # typowane, sprawdzane budżety
    ports/
      clock.rs
      node_link.rs
      capture_repository.rs
      event_sink.rs
      status_publisher.rs
      operation_store.rs
    node/
      cursors.rs                   # received/durable/released
      transfer_state.rs
      clock_model.rs
      configuration.rs             # desired/observed/applied
    channel/
      observability.rs             # Production/Diagnostic
      lifecycle.rs
      scheduler.rs                 # deadline-aware weighted scheduling
      admission.rs                 # budżet jednej linii
      runtime.rs                   # koordynacja usług jednej linii
    recording/
      session.rs                   # prepare/start/finalize/fail
      ingest.rs                    # ciągłość, deduplikacja i zapis strumienia
      recovery.rs                  # odzyskanie trwałego cursora
    maintenance/
      coordinator.rs               # bezpieczne wejście/wyjście z maintenance
      commissioning.rs
      config_transaction.rs
      firmware_update.rs
    coordination/
      host_budget.rs               # CPU/RAM/storage dla 1–16 linii
      group_operation.rs           # wynik per linia, deadline i policy
      operation.rs                 # command/result/progress/idempotency

sensor-host/src/
  node_session.rs                 # składa wire Measurement z cursorami domeny
  measurement_transfer.rs        # przebieg pull/result/persistence/recovery
  channel_agent.rs               # wykonawca jednej transakcji kanału
  channel_scheduler.rs           # czysta polityka wyboru jednej pracy
  channel_admission.rs           # konserwatywny budżet przepustowości linii
  channel_runtime.rs             # bounded stan i rotacja node jednej linii
  measurement_coordinator.rs     # runtime ↔ session ↔ agent ↔ durable sink
  system_config/                   # konfiguracja instalacji
    model.rs                       # immutable typed config
    loader.rs                      # common/system/local overlays
    validation.rs                  # tożsamości, porty, zakresy, konflikty
    reconciliation.rs              # desired kontra observed
  adapters/                        # zewnętrzne mechanizmy
    rs485/                         # serial port, turnaround i NodeLink codec
    storage/                       # Capture, fsync i atomic publish; format otwarty
    runtime/                       # IPC, status read model, audit/event log
    platform/                      # system clock, device inventory, disk stats
    update/                        # bootloader transport i pakiety firmware
  app.rs                           # composition root jednego procesu
  main.rs                          # cienki entrypoint

python/                             # poza procesem recordera
  sensor_data/                     # Capture/Archive/validation
  tools/                           # verifier, replay, inspector, USB observer
```

Nazwy plików są docelowe. Małe, silnie powiązane typy mogą pozostać w jednym
pliku. Nie tworzymy klasy dla każdego command ani event.

## 3. Kierunek zależności

```text
sensor_core <- sensor_protocol
      ^              ^
      |              |
      +--- adapters -+
              ^
              |
 sensor_data <- apps -> system_config
```

Dokładne reguły:

- `sensor_protocol` importuje wyłącznie stabilne typy wartości z `sensor_core`;
  nie importuje adapterów ani aplikacji;
- `sensor_core` nie importuje protokołu, data products, konfiguracji, adapterów
  ani aplikacji;
- `sensor_data` może używać stabilnych typów wartości z `sensor_core`, ale nie
  runtime kanału ani adapterów;
- `system_config` może tworzyć typowane wartości rdzenia, ale nie uruchamia I/O;
- adaptery implementują porty rdzenia i mogą używać codec protokołu;
- aplikacje mogą składać wszystkie warstwy;
- `tools` mogą używać publicznych kontraktów, lecz produkcja nie importuje tools;
- żaden moduł `host_v2` nie importuje starego `host`.

## 4. Pokrycie zadań produktu

| Zadanie | Właściciel logiki | Mechanizm zewnętrzny |
| --- | --- | --- |
| Framing, CRC, resynchronizacja | `sensor_protocol` | `adapters.rs485` dostarcza bajty |
| Wersje i capabilities | handshake hosta | RS485 codec |
| Wiele node na linii | `channel.scheduler` | jeden serial adapter |
| 1–16 linii | `HostCoordinator` + `host_budget` | zadania runtime/systemd |
| Admission control linii | `channel.admission` | metrics adaptera RS485 |
| Globalny budżet CPU/RAM/storage | `coordination.host_budget` | disk/platform stats |
| Walidacja odbioru i cursory | hostowy `NodeSession` | Measurement codec |
| Retry i wybór kolejnej pracy | `ChannelScheduler` + `ChannelAgent` | `ChannelPort` |
| Trwały zapis i release | `recording.session` + cursory | `CaptureRepository` |
| Recovery po restarcie | `recording.recovery` | Capture metadata/node cursor |
| Synchronizacja czasu | `node.clock_model` | time-sync messages + `Clock` |
| Production/Diagnostic | `channel.observability` | bounded polityka telemetrii |
| Idle/Recording/Stopping/Maintenance/Faulted | `channel.lifecycle` | `ChannelAgent` wykonuje przejście |
| Konfiguracja node | `maintenance.config_transaction` | config messages/config store |
| Commissioning | `maintenance.commissioning` | commissioning messages |
| Firmware update i rollback | `maintenance.firmware_update` | update adapter/bootloader |
| Health | evaluatory w `sensor_core` | facts i metrics z adapterów |
| Diagnostyka produkcyjna | bounded policy kanału | compact diagnostics messages |
| Diagnostyka developerska | `tools` i Diagnostic mode | inspector/USB/simulator |
| Capture | recording port + Capture contract | storage adapter; format po benchmarku |
| Archive | `sensor_data.archive` | `archive_worker` + HDF5 adapter |
| Walidacja i manifesty | `sensor_data.verification` | verifier CLI |
| Retencja i disk pressure | `sensor_data.retention`, host budget | platform/storage adapter |
| Status operatora | typed snapshots/read models | runtime IPC/API adapter |
| Start/stop/config/update | operation model | Operations API/CLI/UI |
| Operacje grupowe | `group_operation` | HostCoordinator/ChannelControl |
| Audit | operation model | trwały operation store/event sink |
| Podgląd/FFT/download | osobni konsumenci sealed/live read model | UI/data endpoint |
| Konfiguracja instalacji | `system_config` | JSON teraz; format wymienny |
| Stable device binding | config identity rules | udev/device inventory adapter |
| Soak, replay, fault injection | scenariusze testowe | simulator/fake clock/HIL |

## 5. Topologia runtime

Minimalna topologia produkcyjna wewnątrz jednego procesu `sensor-host`:

```text
HostCoordinator
  |-- ChannelAgent(channel 1) -- one transport owner -- NodeSession[]
  |-- ChannelAgent(channel 2) -- one transport owner -- NodeSession[]
  `-- ... maksymalnie 16

Operations API <-> HostCoordinator/ChannelControl
Operator UI    -> Operations API
Archive worker -> tylko sealed Capture
```

Docelowy `ChannelAgent` jest izolowanym zadaniem z ograniczonymi kolejkami. Jego
model nie zależy od wyboru IPC, więc cały `TransportDevice` można później
wydzielić do osobnego procesu. Awaria zadania jednego channel nie może uszkodzić
stanu pozostałych; restart całego procesu korzysta z trwałych cursorów. Obecna
implementacja jest synchronicznym wykonawcą pierwszego pionu i nie ma jeszcze
task runtime ani nadzoru `HostCoordinator`.

Wewnątrz jednego RS485 channel operacje na magistrali są sekwencyjne. Nie używamy
równoległych requestów do wielu node na half-duplex RS485. Współbieżność służy
obsłudze niezależnych channels i niezależnego storage, nie ukrywaniu niejawnego
współdzielenia portu.

## 6. Zakres głównych klas

### `NodeSession`

Jest hostową usługą aplikacyjną łączącą DTO Measurement Service z domenowymi
cursorami jednego node. Waliduje ciągłość, przechowuje jeden wynik oczekujący na
trwałość i przechodzi do jawnego recovery. Nie wybiera terminów retry, nie
otwiera serial portu, nie zna HDF5 i nie publikuje HTTP. Czyste typy cursorów,
gap i continuity pozostają w `sensor-core`.

### `ChannelRuntime`

Egzekwuje mode i lifecycle jednego channel, uruchamia scheduler oraz deleguje pracę
do `NodeSession`, recording i maintenance. Nie implementuje codec, HDF5, update
engine ani modeli zegara.

### `ChannelAgent`

Jest właścicielem wykonania transakcji jednego channel i jedynego
`TransactionCore`. Docelowo uruchamia `ChannelRuntime`, publikuje snapshot i
wykonuje kontrolowany shutdown. Polityka wyboru pracy należy do
`ChannelScheduler`; Measurement i Time korzystają już z jednego arbitra oraz
osobnych bounded ścieżek trwałości. Health i Configuration dołączają jako
kolejne jawne klasy pracy, bez drugiego właściciela magistrali.
Agent nie zawiera globalnej kompozycji procesu.

### `HostCoordinator`

Zarządza lifecycle agentów, admission control hosta i operacjami grupowymi. Nie
dekoduje ramek i nie przesuwa cursorów node. `sensor-host` składa coordinator,
agentów i adaptery jako jeden composition root.

### `OperationsApi`

Waliduje uprawnienie i składnię command, nadaje `operation_id`, zapisuje audit,
przekazuje operację właścicielowi oraz publikuje result/progress. Nie wykonuje
bezpośrednio maintenance ani recording.

## 7. Elementy świadomie poza gorącą ścieżką

- Archive, kompresja długoterminowa i replikacja działają na sealed Capture.
- FFT, wykresy i resampling są danymi pochodnymi.
- Pełne zrzuty diagnostyczne i protocol inspector należą do Diagnostic lub
  Maintenance, nigdy do obowiązkowej pętli produkcyjnej.
- USB observer pozostaje bounded narzędziem obserwacyjnym i nie posiada cursora
  produkcyjnego.
- UI nie jest częścią gwarancji trwałego commit.

## 8. Zachowania awaryjne wymagające właściciela

| Awaria | Decyzja należy do |
| --- | --- |
| CRC/śmieci w strumieniu | stream decoder + channel retry policy |
| Timeout node | `NodeSession` i scheduler |
| Restart node/nowy boot epoch | `NodeSession` + recording recovery |
| Brak miejsca lub błąd fsync | Capture adapter raportuje; recording zatrzymuje commit |
| Wolny storage | host budget/channel policy stosuje backpressure lub stop |
| Przepełnienie retencji node | quality event + health; nigdy cichy reset cursora |
| Awaria jednego ChannelAgent | HostCoordinator restartuje według bounded policy |
| Awaria Operations API/UI | recording działa dalej |
| Niezgodna wersja/capability | handshake fail-closed przed recording |
| Niepełny update | maintenance/update recovery; ostatni potwierdzony slot pozostaje bootowalny |

## 9. Reguła dodawania przyszłej funkcji

Przed utworzeniem nowego modułu odpowiadamy:

1. Czy to mechanizm wire, reguła domenowa, produkt danych, konfiguracja,
   adapter, aplikacja czy narzędzie?
2. Kto jest właścicielem jego zmiennego stanu?
3. Czy uczestniczy w gwarancji trwałego commit?
4. W którym mode może działać i jaki ma budżet?
5. Jaki ma bounded failure i jak jest testowany?
6. Czy istniejący publiczny port wystarcza?

Jeżeli odpowiedź nie wskazuje naturalnego miejsca, najpierw aktualizujemy model
architektury. Nie dokładamy funkcji do najbliższej dużej klasy.

## 10. Kolejność budowy bez pustych abstrakcji

1. Framing v3, stream decoder i golden vectors.
2. Minimalne discovery/version/capabilities.
3. Typy identity, sample, continuous segment, gap i cursory w `sensor_core`.
4. `SimulatedNodeLink` oraz deterministyczny fake clock.
5. `NodeSession` z MemoryCaptureRepository i testami commit/recovery.
6. Scheduler wielu node i admission control jednej linii.
7. `ChannelRuntime` z trzema mode i typed snapshots.
8. Adapter RS485 oraz zgodny firmware node.
9. Crash-safe Capture adapter i recovery po utracie zasilania.
10. `HostCoordinator`, budżet 1–16 linii i IPC.
11. Maintenance: konfiguracja, commissioning i update.
12. Operations API, audit, CLI i UI read models.
13. Archive, retention i narzędzia offline.

Każdy etap kończy się działającym pionowym scenariuszem i nie pozostawia dwóch
aktywnych właścicieli tego samego stanu.

## 11. Decyzje produktowe i parametry do pomiaru

Wymagania poniżej zostały rozstrzygnięte na poziomie produktu. Otwarte pozostają
wartości liczbowe, które muszą wynikać z pomiarów, a nie z przypadkowego kodu.

### Rodzaj synchronizacji

Zakres produktu to wyrównanie timestampów z mierzoną niepewnością i zachowanie
źródłowych obserwacji czasu. Fazowo jednoczesne próbkowanie nie jest obecnym
wymaganiem.

### Retencja podczas awarii

`MeasurementJournal` wykorzystuje jawnie przydzielony, ograniczony budżet pamięci.
Pomiar rozmiaru rekordu oraz profili ODR wyznaczy gwarantowany czas retencji,
dopuszczalny czas recovery i alarm `time_to_overflow`.

### Maksymalna konfiguracja jednej linii

Liczba node nie jest stałą niezależną od output ODR. Admission control oblicza
legalność konfiguracji dla właściwości aktywnego adaptera i wymaganego marginesu.
Obecne stanowisko 8 × RS485 z jednym node na channel jest pierwszym profilem
HIL, a wiele node na channel osobnym obowiązkowym profilem testowym.

### Zdalny control plane

Pierwsza wersja Operations API jest lokalna i dostępna przez lokalne IPC albo
loopback. Zdalny dostęp może zostać dodany wyłącznie jako uwierzytelniony,
szyfrowany gateway bez zmiany domeny.

### Polityka dostępności podczas update

Maintenance może zatrzymać całą wybraną instalację. Aktualizacje mogą przebiegać
sekwencyjnie; wymagane są bezpieczny drain, możliwość wznowienia, post-update
verification i rollback. Dostępność recording podczas serwisu nie jest celem.

Pozostają do zmierzenia: koszt rekordu journala, retencja per ODR, rzeczywisty
narzut adaptera, bezpieczny limit wykorzystania channel, dryf zegarów i wymagany
interwał synchronizacji. Nie wymagają one zmiany kierunku zależności.
