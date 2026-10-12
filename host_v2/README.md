# Sensor System host v2

`host_v2` jest równoległą implementacją następcy obecnego hosta. Stary katalog
`host` pozostaje działającą referencją do czasu kontrolowanego przełączenia.

Droga krytyczna recordera jest implementowana w Rust. Python pozostaje przy
narzędziach offline: inspekcji plików, analizie, migracji danych i testach
zgodności. Firmware node pozostaje w C++17 z Pico SDK. Ten podział ogranicza
narzut pamięci hosta i zachowuje wygodny ekosystem analityczny poza recorderem.
Time persistence czyta identyfikator bootu Linuksa, odzyskuje stałą topologię
kompaktowych WAL-i i uruchamia jeden bounded executor przed udostępnieniem
submitterów kanałom.

## Początkowy workspace

```text
host_v2/
  crates/
    sensor-core/       model domeny i reguły niezależne od I/O
    sensor-protocol/   wersjonowane typy i kodek wire protocol
    sensor-host/       kompozycja procesu i późniejsze adaptery
  contracts/golden/    wektory zgodności host ↔ firmware
  python/              narzędzia offline, poza drogą krytyczną
```

Zależności wskazują do środka:

```text
sensor-host -> sensor-core
sensor-host -> sensor-protocol
sensor-protocol -> sensor-core
sensor-core -X-> I/O, HDF5, serial, HTTP
sensor-protocol -X-> sensor-host
```

Tworzymy kolejne moduły dopiero z użytecznym zachowaniem. Interfejs (trait)
powstaje na granicy, która ma co najmniej dwa adaptery albo musi być izolowana w
teście. Nie tworzymy pustej klasy dla każdego rzeczownika z dokumentacji.

## Model wykonania

Pierwsza wersja to jeden lekki proces `sensor-host`. W jego wnętrzu każdy kanał
ma własny logiczny `ChannelAgent`, ograniczone kolejki i jednego właściciela I/O.
Jeśli pomiary lub testy awarii wykażą potrzebę silniejszej izolacji, granicę
procesu można później przenieść na `TransportDevice` bez zmiany domeny.

## Wersje pierwszej iteracji

- host software: `0.5.0-dev`;
- application protocol: `5`;
- framing protocol: `3`.

## Budowanie i testy

Wymagany jest stabilny Rust określony w `rust-toolchain.toml`.

```bash
cargo fmt --all --check --manifest-path host_v2/Cargo.toml
cargo clippy --workspace --all-targets --manifest-path host_v2/Cargo.toml -- -D warnings
cargo test --workspace --manifest-path host_v2/Cargo.toml
cargo build --release -p sensor-host --manifest-path host_v2/Cargo.toml
```

## Proces produkcyjny

`sensor-host` ma trzy kolejne granice uruchomienia. `validate` sprawdza pełną
topologię, storage i `runtime_v2` bez otwierania portów. `discover` wykonuje
read-only handshake. `run` przechodzi przez preflight filesystemu, bootstrap,
admission, recovery Capture/Time i dopiero wtedy wysyła `Start`:

```bash
host_v2/target/release/sensor-host validate \
  --config host/system_config.json --repository-root "$PWD"
host_v2/target/release/sensor-host run \
  --config host/system_config.json --repository-root "$PWD" \
  --status-file var/tmp/host-v2-status.json
```

`SIGINT` i `SIGTERM` uruchamiają tę samą kontrolowaną sekwencję: quiesce,
rozliczenie pracy w locie, `Stop`, pobranie ogona journala, durable drain,
zamknięcie Capture, dołączenie wszystkich kanałów i zamknięcie wspólnego Time
WAL. Proces wypisuje wynik każdego kanału i kończy się kodem 0 wyłącznie po
czystym shutdownie. Szablon usługi znajduje się w
`systemd/sensor-host-v2.service.in`; nie jest aktywną jednostką, lecz wejściem
dla `./hostctl install-services`. Instalator podstawia rzeczywiste ścieżki,
użytkownika i grupę, instaluje binarium pod wersjonowanym SHA-256 i dopiero
potem publikuje jednostkę. Szczegóły znajdują się w
[`DEPLOY-RASPBERRY-PI.md`](DEPLOY-RASPBERRY-PI.md).

`run` wymaga jawnej ścieżki statusu. Testy ręczne zapisują ją razem z
artefaktami, a usługa produkcyjna używa
`/run/sensor-system/host-v2-status.json`. Plik ma wersjonowany schemat, jest
zastępowany atomowo i pozostaje ostatnim terminalnym snapshotem po shutdownie.

Diagnostyka operatora jest osobnym natywnym binarium i nie wymaga legacy
Python venv:

```bash
cargo build --release -p sensor-host --manifest-path host_v2/Cargo.toml
./hostctl doctor --offline
./hostctl doctor
./hostctl doctor --deep
./hostctl doctor --json
```

Tryb domyślny nie otwiera RS485. `--deep` odmawia discovery, gdy usługa albo
skonfigurowany port są zajęte. Kontrakt raportu opisuje
`docs/architecture/HOST-DOCTOR-V2.md`.

Parametry wykonania są zebrane w `runtime_v2` w `host/configs/common.json`.
Obejmują timeouty wire, quarantine, admission, rozmiar chunku, retry, fazowanie
TimeSync i deadline shutdownu. Są walidowane jako jeden kontrakt. Wartości
kosztów transmisji są konserwatywnym punktem startowym i należy je dostroić z
pomiarów HIL 1/4/8 kanałów, bez zmiany logiki domenowej.

Proces wypisuje konfigurowalne, okresowe snapshoty per kanał. Liczniki RS485,
retry, TimeSync i backpressure oraz maksimum czasu kroku są atomowe i nie
logują każdej próbki. Executor Capture raportuje dodatkowo liczbę group commit,
rekordy, błędy, high-water kolejki, największy batch i najdłuższe wywołanie
durable persistence obejmujące synchronizację nośnika.

Przed użyciem sprzętu powtarzalna bramka symulacyjna uruchamia pełną kompozycję
dla 1, 4 i 8 kanałów, izolację awarii workera oraz recovery po utraconej
odpowiedzi i chwilowym błędzie storage:

```bash
host_v2/tools/run_simulation_gate.sh
```

Przygotowanie Raspberry Pi, commissioning prawdziwego inventory oraz
powtarzalna macierz nagrań 1/4/8 są opisane w
[`DEPLOY-RASPBERRY-PI.md`](DEPLOY-RASPBERRY-PI.md). Skrypty
`tools/run_pi_host_gate.sh` i `tools/run_pi_matrix.sh` zapisują dowody testu i
kończą proces przez kontrolowany `SIGTERM`.

Read-only handshake with a protocol-5 node:

```bash
host_v2/target/release/sensor-hil handshake \
  /dev/sensor-system-rs485 1 0011223344556677 2 115200
```

Time Service on firmware 0.5.6 can be exercised through the same fail-closed
identity and transaction path:

```bash
host_v2/target/release/sensor-hil time-probe \
  /dev/sensor-system-rs485 1 0011223344556677 2 8 1000 115200
```

This prints the raw four timestamps, path delay and uncertainty for every
exchange. A short run validates the wire path; oscillator drift and production
sync intervals require a longer measurement.

For a newly migrated protocol-5 node whose immutable hardware ID has not yet
been added to inventory, use the read-only discovery command first:

```bash
host_v2/target/release/sensor-hil identify \
  /dev/sensor-system-rs485 1 115200
```

`identify` never authorizes recording or a trial confirmation. Those commands
still require the discovered identity as an explicit expected value.

Capabilities schema 2 reports Measurement Service **output rates**, after the
node's mandatory filtering and decimation. The current firmware advertises
`2000, 1000, 500, 250, 125, 62.5 Hz`; it does not expose the ADXL355 input ODR
bitmap as if it were the stream rate. The selected current rate belongs to the
configuration observed state and will be read through Configuration Service.

After an A/B upload, confirm the trial over RS485 with the expected hardware
identity. This command performs the same fail-closed handshake first:

```bash
host_v2/target/release/sensor-hil confirm-trial \
  /dev/sensor-system-rs485 1 0011223344556677 2 0.5.6 115200
```

The node allows 30 seconds after its local initialization. If confirmation does
not arrive, it restarts and the bootloader rolls back to the last active slot.
The expected version is mandatory so a previous protocol-5 image reached by
rollback cannot be accepted as the newly uploaded trial; release versions must
be unique.
The command retries its fail-closed handshake while the bootloader maintenance
window is active. Physical HIL has verified both outcomes: automatic rollback
without confirmation and promotion of a matching protocol-5 trial image.
It has also verified a protocol-5 `0.5.4` to `0.5.5` update entered through
Lifecycle Service. The narrowly scoped schema-1 bridge is accepted only by
`prepare-update` for the exact `0.5.4` firmware identity; ordinary handshakes
continue to require capabilities schema 2.

A bounded vertical recording test uses the same production channel adapter:

```bash
host_v2/target/release/sensor-hil record-cycle \
  /dev/sensor-system-rs485 1 0011223344556677 2 \
  var/tmp/hil-record-cycle.txt 250 32 115200
```

It performs handshake, `Start`, pull, durable file sync, `Release`, `Stop` and
final reconciliation to `Maintenance`. Set
`SENSOR_SYSTEM_HIL_RECORD_LIMIT_SECONDS` to a value from 1 through 300 when a
different test deadline is needed.

A repeatable manual test uses the production Capture WAL, short windows and an
independent validation pass over every sealed segment:

```bash
./host_v2/tools/run_manual_rs485_hil.sh \
  /dev/cu.usbserial-0001 1 0011223344556677 2 2500 48
```

The procedure, artifacts and pass criteria are documented in
[Manual RS485 HIL](../docs/architecture/MANUAL-RS485-HIL.md).
Ten-minute recording, validated CSV export and MATLAB/Jupyter quick-look are
documented in
[Capture analysis export](../docs/architecture/CAPTURE-ANALYSIS-EXPORT.md).

Pełna decyzja o językach i procesach znajduje się w
[ADR-0007](../docs/adr/0007-host-runtime-language-and-process-model.md).
Aktualny stan, brakujące elementy i kolejność prac opisuje
[implementation roadmap](../docs/architecture/IMPLEMENTATION-ROADMAP.md).
Semantyka pierwszego service application protocol jest w
[Measurement Service](../docs/protocol/MEASUREMENT-SERVICE.md).
Sterowanie `GetState`, `Start`, `Stop`, Maintenance i przygotowanie update
opisuje [Host v2 Lifecycle Client](../docs/architecture/HOST-V2-LIFECYCLE-CLIENT.md).
Pierwszy `ChannelAgent` współdzieli jeden `TransactionCore` między handshake i
Lifecycle. `ChannelEndpoint` składa go z adapterem portu, a Measurement Service
zatrzymuje pełny chunk do potwierdzonego durable commit. `ChannelRuntime` i
`MeasurementCoordinator` realizują bounded rotację wielu node, mały budżet
retry, offline backoff oraz handoff do trwałego sinka bez kopiowania sesji.
Fizyczny pion jednej linii jest sprawdzony. `CaptureWalSink` dodaje rzeczywisty
packed-int24 WAL, CRC32C, `sync_data`, recovery niepełnego ogona i fail-closed
korupcję. `CaptureSegmentStream` wykonuje rotację per okno i atomową publikację
manifestu, `CaptureWalReader` niezależnie waliduje źródło dla Archive, a
`CaptureRepository` rozdziela wiele skonfigurowanych node.
`InstallationStorageProfile` rozwiązuje tracked JSON z `extends`, a
`CaptureInstallationProfile` buduje bounded runtime z polityki instalacji i
tożsamości zwróconej przez discovery. Rdzeń cleanupu Capture chroni aktywne
okno i zapisuje wynik dopiero po synchronizacji katalogu. `ArchiveCoverageIndex`
utrwala dokładne podzakresy źródłowe, a walidator tworzy dowód dopiero po pełnym
pokryciu i replikacji wszystkich użytych produktów. `CaptureCoverageCheckpointStore`
utrwala cały zestaw produktów i dopiero po deadline zezwala cleanupowi na
usunięcie segmentu. `ArchiveRetentionCheckpointStore` utrwala niezależny
30-dniowy deadline i zależności wejściowe; cleanup Archive wymaga trwałego
zakończenia cleanupu każdego z tych segmentów Capture. Niskopoziomowy
`CaptureWalBatch` i związany z jednym
strumieniem `CaptureRepositoryBatch` są gotowe: nie publikują durable cursora
przed wspólnym `sync_data` i nie przekraczają granicy okna.
Atomowe checkpointy 48 h bufora Capture i 30 dni lokalnego Archive opisuje
[Retention checkpoint](../docs/architecture/RETENTION-CHECKPOINT.md), a format
danych [Capture WAL v1](../docs/architecture/CAPTURE-WAL-V1.md).
Ograniczoną kolejkę rekordów, completion i regułę zatrzymania po błędzie opisuje
[Bounded persistence writer](../docs/architecture/BOUNDED-PERSISTENCE-WRITER.md).
`NodeSession` i `MeasurementCoordinator` przekazują rekord bez kopiowania,
parkują tylko dany node i korelują durable completion przez `WriteJobId`.
`PersistenceExecutor` wykonuje I/O na osobnym wątku, a
`ThreadedChannelRecordingLoop` używa bounded submit/completion bez `fsync` w
pętli RS485. `CaptureRuntimeConfig` wykonuje recovery repozytorium, zachowuje
snapshot durable cursorów i dopiero potem uruchamia jego wyłącznego executora.
`GroupedPersistenceWorker` zachowuje wszystkie joby niepewnego batcha,
potwierdza odzyskany prefiks i ponawia tylko suffix. `GroupedPersistenceExecutor`
jest podłączony do `CaptureRuntimeConfig` i generycznej pętli recording. Pozostaje
pomiar parametrów fsync na docelowym Raspberry Pi z M.2 SSD. Testowy failpoint
bariery potwierdza recovery pełnej niepewnej grupy bez
ponownego appendu.
`ChannelRecordingLoop` składa endpoint, scheduler/runtime, sesje transferu i
durable sink dla jednego kanału. Deterministyczny `SimulatedRs485Bus` prowadzi
request i response przez rzeczywisty framing oraz Measurement codec. Testy
czterech node przy 125 Hz obejmują utraconą odpowiedź, jednorazowy błąd storage
i node offline; zdrowe node nie są głodzone, a journal nie przepełnia się.
Szczegóły są w
[raporcie symulacji](../docs/architecture/HOST-V2-SIMULATION.md).
`GET_STREAM_STATE` korzysta z tego samego `ChannelAgent` i właściciela portu co
pozostałe transakcje. `stream_recovery` porównuje jego wynik z identity,
capabilities i durable cursorem Capture: nie pozwala zwolnić nieutrwalonego
prefiksu, wykrywa zmianę epoki/pojemności i pozostawia dokładną lukę po
nadpisaniu journala do zapisania przez normalny Measurement pull. Pełna
kompozycja tej polityki w startupie `HostCoordinator` pozostaje otwarta.
`ChannelBootstrap` zachowuje przy tym jeden żywy endpoint przez handshake,
`GetState` i `GET_STREAM_STATE` wszystkich node. `channel_recovery` używa
nagłówka Capture wyłącznie jako origin konfiguracji; sesję pomiarową tworzy
dopiero z durable cursora zwróconego przez rzeczywisty replay WAL i przypina go
do `(hardware_id, boot_epoch)`.
`RecordingActivation` działa dopiero na odzyskanych sesjach: pozostawia
aktywny Recording bez resetu albo uruchamia node z Idle, nie modyfikując
cursorów. Assembler `ChannelWorkCoordinator` pobiera pozycję wyłącznie z
`NodeSession` i fazuje pierwsze TimeSync między node.
`compose_channel_recording` wykonuje dla jednego kanału pełną kolejność od
żywego endpointu, przez replay Capture i aktywację, do
`ThreadedChannelRecordingLoop`. Po błędzie późnej fazy jawnie zatrzymuje
uruchomiony executor storage oraz cofa rozpoczętą aktywację do `Idle`.
`installation_bootstrap` zachowuje endpointy wszystkich aktywnych linii po
równoległym bootstrapie i publikuje wynik dopiero po walidacji kompletnego
inventory. `HostStartupBlueprint` przypina każdy endpoint do jego planu
admission po `channel_id` oraz przygotowuje jedną topologię Time WAL, nadal bez
otwierania Capture i bez wysyłania `Start`. `start_host_runtime` przekracza tę
granicę: otwiera globalny Time, składa komplet tasków i uruchamia supervisor.
Przy shutdownie zatrzymuje oraz dołącza wszystkie kanały przed zamknięciem Time
WAL.
`ChannelSupervisorSet` dostarcza już bounded, niezależny nadzór 1–16 tasków;
awaria jednego kanału nie zatrzymuje pozostałych. `ChannelWorkCoordinator`
potrafi wejść w `quiescing`, a retry-safe shutdown Lifecycle doprowadza node do
`Idle`. `ProductionChannelTask` łączy je pełnym protokołem: bariera work → Stop
→ wymuszony drain zamrożonego ogona journala aż do `NoData` → durable completion
→ zamknięcie Capture. Wynik protokołu oraz wynik shutdownu storage są zwracane
osobno i nie mogą zostać pomylone z pozornym sukcesem.
`HostHealthSnapshot` zachowuje izolację pojedynczej linii, ale wymaga globalnego
Stop dla błędu integralności Time oraz potwierdzonego faultu wspólnego storage.
`StorageHealthProbe` sprawdza read-only i `min_free_bytes` przez `statvfs` bez
zapisu pliku testowego. Po błędzie bariery Capture shutdown nadal podejmuje
próbę `Stop`, aby nie pozostawić node produkującego bez właściciela.
Time Service korzysta z tego samego arbitra kanału i fazuje osobne exchange dla
każdego node. `TimeObservationSubmitter` nie wykonuje I/O w pętli RS485, a
`TimeWalSink` przechowuje surowe `t1..t4` w stałym rekordzie 36 B z CRC32C,
group commit oraz recovery niepełnego ogona. Tożsamości node, jego boot epoch i
boot zegara hosta są zapisane raz w nagłówku. `TimeRepository` rozdziela
skonfigurowane strumienie, a bounded `TimePersistenceWorker` zachowuje
niepewną obserwację i po recovery zapobiega zarówno utracie, jak i ponownemu
appendowi. `TimePersistenceExecutor` wykonuje I/O na osobnym wątku i daje
klonowalne bounded submittery kanałom. Format i otwarty krok startup opisuje
[Time WAL v1](../docs/architecture/TIME-WAL-V1.md), a dobór
konfigurowalnego interwału [Time Service](../docs/architecture/TIME-SERVICE.md).
`CaptureWindowLedger` zamraża niezależne od formatu reguły rotacji: zakresy
manifestów `[first_cursor, end_exclusive)`, kolejne ordinale, jawne gaps i brak
podziału wire chunku. Plan nie przesuwa stanu przed durable append, a manifest
zamkniętego okna staje się dostępny dopiero przy commit. Kontrakt opisuje
[ciągłość okien Capture](../docs/architecture/CAPTURE-WINDOW-CONTINUITY.md).
Powtarzalny benchmark packed int24, WAL, gzip, zstd i HDF5 znajduje się w
[STORAGE-FORMAT-BENCHMARK.md](../docs/architecture/STORAGE-FORMAT-BENCHMARK.md),
a jego narzędzie w `python/tools/storage_benchmark.py`.
Archive powstaje jako godzinny, niezmienny produkt per sensor po przejściu
durable watermarku. Kompakcja i wysyłka na serwer są niezależne; reguły retry,
idempotencji i retencji opisuje
[ARCHIVE-AND-PUBLICATION-POLICY.md](../docs/architecture/ARCHIVE-AND-PUBLICATION-POLICY.md).
Pełny profil ODR ADXL355 i format próbki opisuje
[audyt ODR i reprezentacji](../docs/protocol/DATA-TYPE-AND-RATE-AUDIT.md).
