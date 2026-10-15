# Uruchomienie host v2 na Raspberry Pi

Ten dokument prowadzi do pierwszej wiarygodnej macierzy 1/4/8 kanałów. Nie jest
instrukcją przełączenia produkcji. Najpierw zbieramy prawdziwe inventory,
uruchamiamy kontrolowane nagrania i oceniamy artefakty. Legacy host i host v2
nie mogą jednocześnie posiadać tych samych portów.

## 1. Warunki wejściowe

- 64-bit Raspberry Pi OS z działającym `/proc/sys/kernel/random/boot_id`;
- Rust zgodny z `host_v2/rust-toolchain.toml` oraz GNU `timeout`; pakiet GNU
  `time` jest zalecany do pomiaru RSS i CPU;
- użytkownik procesu należy do grupy mającej dostęp do portów szeregowych;
- każdy port ma stabilny symlink `/dev/sensor-system-line-*`; nazw
  `/dev/ttyUSB*` nie zapisujemy jako tożsamości instalacji;
- zegar systemowy jest synchronizowany przez NTP/chrony;
- docelowy filesystem Capture jest zamontowany, zapisywalny i ma wymaganą
  rezerwę miejsca;
- wszystkie node używają protocol 5, a ich board revision została fizycznie
  potwierdzona.

Przed dotknięciem RS485 uruchamiamy pełne testy i bramkę symulacyjną:

```bash
cargo fmt --all --check --manifest-path host_v2/Cargo.toml
cargo clippy --workspace --all-targets --manifest-path host_v2/Cargo.toml -- -D warnings
cargo test --workspace --manifest-path host_v2/Cargo.toml
host_v2/tools/run_simulation_gate.sh
```

## 2. Commissioning inventory

Każdy node identyfikujemy osobno, bez zapisu konfiguracji:

```bash
host_v2/target/release/sensor-hil identify \
  /dev/sensor-system-line-a 1 115200
```

Dla każdego fizycznego portu zapisujemy: nazwę linii, stabilny endpoint,
`device_id` konwertera, adres node, `hardware_id`, board revision i oczekiwany
output rate. `hardware_id` nie wolno zgadywać ani kopiować między liniami.
Jeśli na jednym kanale jest kilka node, wszystkie muszą mieć unikalne adresy.

Po zebraniu pełnej mapy aktualizujemy tracked profil instalacji, na przykład
`host/configs/systems/rpi-sanok.json`. Lokalny `host/system_config.json` rozszerza
ten profil i zawiera wyłącznie lokalne ścieżki storage, endpointy oraz czasowe
włączenie lub wyłączenie kanałów. Dane runtime, nagrania i surowe logi pozostają
poza Git.

Do macierzy przygotowujemy trzy lokalne konfiguracje z dokładnie 1, 4 i 8
aktywnymi kanałami. Można przechować je w ignorowanym `var/tmp/pi-profiles/`.
Każdy wariant musi zachować te same tożsamości i kolejność fizycznych linii.

## 3. Kontrola przed nagraniem

```bash
./hostctl paths
./hostctl doctor
host_v2/target/release/sensor-host validate \
  --config host/system_config.json --repository-root "$PWD"
host_v2/target/release/sensor-host discover \
  --config host/system_config.json --repository-root "$PWD"
```

`validate` nie otwiera portów. `discover` jest read-only i musi potwierdzić
dokładny hardware ID, board revision, protokół, capabilities i output rate
każdego node. Rozbieżność blokuje recording.

## 4. Powtarzalna macierz fizyczna

Pojedynczy etap uruchamiamy tak:

```bash
host_v2/tools/run_pi_host_gate.sh \
  var/tmp/pi-profiles/1-channel.json 600
```

Skrypt buduje release, wykonuje `validate` i `discover`, uruchamia produkcyjne
`sensor-host run`, po zadanym czasie wysyła `SIGTERM` i czeka na pełny Stop,
drain Capture oraz Time WAL. Artefakty trafiają domyślnie do
`var/tmp/pi-host-gate-<UTC>/`.

Całą macierz wykonujemy kolejno, nigdy równolegle:

```bash
host_v2/tools/run_pi_matrix.sh 600 \
  var/tmp/pi-profiles/1-channel.json \
  var/tmp/pi-profiles/4-channels.json \
  var/tmp/pi-profiles/8-channels.json
```

Pierwsza macierz trwa po 10 minut na etap. Po jej analizie wykonujemy dłuższy
soak test z czasem wynikającym z obserwowanych błędów i retencji journala.

Każdy katalog etapu zawiera:

- wynik walidacji i read-only discovery;
- pełny log procesu z okresowymi metrykami per kanał;
- końcowy raport kontrolowanego shutdownu;
- niezależną walidację CRC, manifestu i ciągłości wszystkich strumieni Capture
  z bieżącego discovery, bez rozwijania próbek do CSV;
- maksymalny RSS, CPU i statystyki procesu, jeśli dostępny jest GNU `time`;
- próbki `vmstat`, jeśli narzędzie jest zainstalowane;
- wersję binarki, boot ID i dane systemu.

## 5. Kryteria oceny

Skrypt automatycznie wymaga poprawnego profilu, discovery, kodu wyjścia 0,
końcowych metryk i `host shutdown: clean=true`. Przed zwiększeniem liczby
kanałów ręcznie sprawdzamy także:

- brak nieuzasadnionych `decode_errors`, timeoutów, disconnectów i odpowiedzi
  late/unrelated/unsolicited;
- brak `persistence_backpressure` i `time_sync_backpressure`;
- retry mają wyjaśnioną przyczynę i nie rosną stale;
- `max_step_us`, `storage_max_persist_us`, high-water kolejki, CPU, RSS oraz
  I/O wait nie pogarszają się w czasie;
- pliki Capture przechodzą walidację, a kursory kolejnych okien tworzą jeden
  ciąg; każdy gap albo continuity break jest jawnie zapisanym dowodem;
- po restarcie proces odzyskuje durable cursor i nie dubluje ani nie pomija
  zakresu próbek;
- odłączenie jednego node degraduje jego strumień, lecz pozostałe kanały nadal
  pracują; powrót node używa ponownego handshake i recovery.

Nie ustalamy dziś arbitralnych limitów CPU, RSS ani `max_step_us`. Pierwsza
macierz tworzy baseline dla Raspberry Pi 4/5 i użytego nośnika. Zero błędów
transportu i backpressure pozostaje oczekiwaniem dla stabilnego testu
laboratoryjnego; każdy wyjątek analizujemy przed dalszym etapem.

## 6. Instalacja systemd

Tracked szablony znajdują się w `host_v2/systemd/`. Nie są aktywnymi
jednostkami. Instalator generuje z nich lokalny recorder oraz timer eksportu,
dlatego rzeczywiste ścieżki, użytkownik oraz lokalny `host/system_config.json`
nie trafiają do Git.

```bash
./hostctl install-services --dry-run
./hostctl install-services
./hostctl start
./hostctl status
./hostctl logs --since today
# jeśli nowy release zawiedzie:
./hostctl rollback-services
```

Pierwsze polecenie pokazuje wynik bez zapisu do `/opt` i `/etc`. Instalacja
buduje release, ponownie waliduje profil, liczy SHA-256 i umieszcza niezmienne
binaria w `/opt/sensor-system/releases/<wersja>-<sha>`. Symlink `current` jest
przełączany atomowo, a poprzedni cel zachowuje `previous`. Instalator włącza
start przy boot, lecz celowo nie uruchamia procesu; start pozostaje jawną
operacją po zatrzymaniu legacy hosta i sprawdzeniu właścicieli portów.
`rollback-services` czeka na kontrolowany Stop, sprawdza wykonywalność
poprzedniego artefaktu, atomowo zamienia `current/previous` i uruchamia usługę.

Jednostka wysyła SIGTERM i daje 120 sekund na kontrolowany Stop oraz drain WAL.
Błąd konfiguracji, utworzenia statusu albo storage nie jest restartowany.
Przejściowy błąd bootstrapu ma odstęp 15 sekund oraz limit czterech startów w
ciągu pięciu minut. To ograniczenie chroni magistrale przed szybką pętlą, ale
nie zastępuje docelowego częściowego bootstrapu przy nieobecnym node; ten
przypadek pozostaje otwartą bramką Etapu 3. Node wykryty poprawnie, lecz
odrzucający aktywację, jest już izolowany do swojego kanału: pozostałe kanały
pracują, a Runtime Status zachowuje jawny `faulted` i ogólny stan `degraded`.

`sensor-analysis-export-v2.timer` uruchamia niezależny proces tylko dla
zamkniętych manifestów Capture. Eksporter nie posiada portów RS485 i nie może
wpłynąć na potwierdzanie próbek. Wyniki trafiają domyślnie do `var/analysis`;
format oraz ograniczenie czasu UTC opisuje `ANALYSIS-CSV-V1.md`.

Przełączenie wykonujemy w oknie serwisowym: zatrzymujemy legacy host, upewniamy
się, że zwolnił wszystkie porty, uruchamiamy v2 i sprawdzamy discovery oraz
nowy Capture. Rollback zatrzymuje v2 przed przełączeniem artefaktu albo ponownym
uruchomieniem legacy.
