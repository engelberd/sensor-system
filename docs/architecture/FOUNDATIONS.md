# Fundamenty nowej architektury hosta

Status: obowiązujący projekt; implementacja fundamentu Rust rozpoczęta.

Aktualny stan, brakujące elementy i kolejność implementacji znajdują się w
[IMPLEMENTATION-ROADMAP.md](IMPLEMENTATION-ROADMAP.md).
Obowiązujące definicje mode, state, health, command, result, event, metric i
snapshot znajdują się w [DOMAIN-LANGUAGE.md](DOMAIN-LANGUAGE.md).

Szczegółową politykę pracy wielu node na jednej linii opisuje
[RS485-COMMUNICATION-POLICY.md](RS485-COMMUNICATION-POLICY.md). Skalowanie hosta
do 1–16 równoległych linii opisuje [HOST-SCALING.md](HOST-SCALING.md).

## 1. Cel

Host ma niezawodnie odebrać uporządkowany strumień pomiarów z jednego lub wielu
node, zapisać go trwale i udostępnić stan pracy operatorowi. Logika odpowiedzialna
za poprawność odbioru nie może zależeć od:

- RS485 ani konkretnej biblioteki portu szeregowego;
- HDF5 ani konkretnego systemu plików;
- HTTP, dashboardu lub sposobu uruchamiania procesu;
- Raspberry Pi, systemd ani ścieżek jednej instalacji;
- szczegółów sterownika ADXL355.

Dzięki temu ten sam rdzeń będzie można wykorzystać z innym czujnikiem,
transportem, formatem zapisu albo na innym systemie operacyjnym.

## 2. Zakres pierwszej wersji

Pierwsza wersja obejmuje tylko drogę krytyczną:

```text
node -> odbiór ramek -> kontrola sekwencji -> trwały zapis -> potwierdzenie
```

Poza rdzeniem pozostają dashboard, konfigurator, aktualizacja firmware, USB
observer, archiwizacja i wizualizacja. Mogą korzystać z rezultatów rdzenia, ale
nie uczestniczą w potwierdzaniu danych.

## 3. Niezmienne reguły systemu

Te reguły są ważniejsze od wyboru języka i bibliotek:

1. Jeden właściciel otwiera dany kanał komunikacyjny.
2. Host potwierdza próbki dopiero po trwałym zapisaniu całej potwierdzanej partii.
3. Kursor odbioru jest częścią domeny, a nie szczegółem portu szeregowego.
4. Duplikat można rozpoznać po `(node_identity, boot_epoch, sample_seq)`.
5. Luka, utrata próbki w sensorze i błąd transportu są różnymi zdarzeniami.
6. Każda kolejka i każdy bufor mają jawny, skończony limit.
7. Wolny zapis powoduje kontrolowane zatrzymanie lub backpressure; nie powoduje
   cichego odrzucania danych.
8. Błąd dashboardu, podglądu albo logowania tekstowego nie może zatrzymać zapisu.
9. Konfiguracja runtime nie zmienia tożsamości sprzętu.
10. Restart procesu nie może przesunąć kursora dalej niż ostatni trwały zapis.

## 4. Model domeny

Rdzeń operuje na kilku pojęciach, które nie znają protokołu binarnego ani HDF5:

### `Sample`

Pojedynczy wynik pomiaru z tożsamością źródła, epoką uruchomienia, numerem
sekwencji, czasem urządzenia, wartościami osi i flagami jakości.

### `TransferChunk`

Spójna, uporządkowana porcja transferu jednego node. Zawiera zakres sekwencji i
dane potrzebne do sprawdzenia ciągłości, ale nie jest utrwalaną granicą
analityczną. Inny adapter może podzielić ten sam strumień inaczej.

### `DeliveryCursors`

Trzy granice wyłączne `received`, `durable` i `released`. Opisują odpowiednio
ciągły odbiór, trwały zapis i zgodę na zwolnienie retencji node. Obowiązuje
`released <= durable <= received`.

### `Gap`

Jawny zapis różnicy między oczekiwaną i otrzymaną sekwencją. Nie jest tylko
komunikatem w logu.

### `NodeSession`

Mała maszyna stanów prowadząca jedną sesję odbioru. Zna reguły pull, odbioru,
walidacji, trwałego zapisu i release, ale komunikuje się wyłącznie przez porty
opisane poniżej.

## 5. Porty rdzenia

Port jest małym kontraktem, który może mieć różne adaptery.

### `NodeLink`

```text
read_identity() -> NodeIdentity
read_stream_state() -> StreamState
pull_samples(boot_epoch, from, release_before, limit) -> PullResult
release_samples(boot_epoch, release_before) -> ReleaseResult
```

Adapter RS485 tłumaczy te operacje na obecne ramki. Przyszły adapter TCP, USB lub
testowa pamięć implementuje ten sam kontrakt.

### `CaptureRepository`

```text
recover_cursor(node_identity) -> ReceiveCursor
append(samples, gaps, timing, quality) -> DurableReceipt
close() -> void
```

`DurableReceipt` jest jedynym dowodem, po którym sesja może przesunąć durable
cursor i przekazać node zgodę `release_before`.
Implementacją może być HDF5, dziennik binarny lub baza danych.

### `EventSink`

Odbiera zdarzenia operacyjne i diagnostyczne. Awaria tego portu nie zmienia
kursora danych pomiarowych.

### `Clock`

Dostarcza czas monotoniczny i UTC. Pozwala testować timeouty i synchronizację bez
rzeczywistego zegara.

## 6. Zależności

```text
aplikacja recordera
        |
        v
rdzeń sesji i model domeny
   ^        ^        ^
   |        |        |
RS485    HDF5     JSONL/status
adapter  adapter     adapter
```

Zależności wskazują do rdzenia. Rdzeń nie importuje adapterów. Interfejs webowy
czyta opublikowany stan i dane; nie wywołuje bezpośrednio `NodeLink` podczas
nagrywania.

## 7. Minimalna maszyna stanów sesji

```text
STARTING -> RECOVERING -> REQUESTING -> RECEIVING -> PERSISTING -> RELEASING
                            ^                                |          |
                            +--------------------------------+----------+

Każdy stan -> STOPPED        na żądanie zatrzymania
Każdy stan -> FAILED         po błędzie trwałym
REQUESTING/RECEIVING -> RETRY_WAIT -> REQUESTING po błędzie przejściowym
PERSISTING -> FAILED         po błędzie storage; bez przesunięcia durable/release
```

Stan ma być jawny w kodzie i testach. Nie powinien wynikać przypadkiem z kolejności
dużej funkcji.

## 8. Granice wykonania

Pierwsza wersja nie potrzebuje mikroserwisów. Zalecany układ to:

- jeden lekki proces Rust `sensor-host` z `HostCoordinator`;
- od jednego do szesnastu logicznych `ChannelAgent`, po jednym na communication
  channel; każdy działa jako niezależne zadanie z ograniczonymi kolejkami;
- w każdym `ChannelAgent` jeden właściciel transport binding i `NodeLink`;
- UI i narzędzia serwisowe pozostają poza drogą krytyczną recordera;
- granicę osobnego procesu można później ustawić per `TransportDevice`, jeśli
  pomiary albo testy awarii uzasadnią koszt IPC i dodatkowej pamięci.

Izolacja własności stanu nie wymaga osobnego interpretera lub procesu dla każdej
linii. Granice modułów i komunikatów pozostają takie same po zmianie topologii.

## 9. Strategia ponownego użycia

Kod wielokrotnego użycia powinien tworzyć bibliotekę bez CLI i bez globalnej
konfiguracji:

```text
sensor_core/
  model
  session
  policies
  ports
  protocol_spec lub protocol_types

adapters/
  rs485
  hdf5
  json_events

apps/
  recorder
  supervisor
  operator_ui
```

W innym systemie wymieniamy adaptery i kompozycję aplikacji. `NodeSession`,
reguły sekwencji, retry i trwałego potwierdzania pozostają te same.

## 10. Język implementacji

- Firmware node pozostaje w C++17 z Pico SDK.
- Krytyczny runtime hosta powstaje w stabilnym Rust: protokół, scheduler,
  sesje, trwały commit, recovery i adaptery online.
- Python obsługuje pracę offline: inspekcję, migrację, walidację i analizę
  danych. Nie jest zależnością procesu recordera.
- UI jest cienkim klientem Operations API i nie uczestniczy w drodze próbek.

Granice językowe mają wspólne, wersjonowane kontrakty i golden vectors. Nie
próbujemy współdzielić klas ani modeli runtime przez FFI. Pełne uzasadnienie
zawiera [ADR-0007](../adr/0007-host-runtime-language-and-process-model.md).

## 11. Bezpieczna migracja

Nie zastępujemy całego systemu jednym ruchem.

1. Spisujemy wersjonowaną specyfikację protokołu i tworzymy binarne wektory
   testowe wspólne dla firmware i hosta.
2. Tworzymy niezależny model domeny i testujemy reguły cursorów, luk, duplikatów,
   retry, trwałego zapisu oraz release bez portu szeregowego.
3. Implementujemy adapter obecnego protokołu RS485 do `NodeLink`.
4. Implementujemy adapter obecnego Capture v1 do `CaptureRepository`.
5. Uruchamiamy nową sesję na zapisanych strumieniach testowych i porównujemy
   wynik ze starym recorderem.
6. Testujemy na jednym fizycznym kanale, bez zmiany firmware node.
7. Dopiero po pomiarach i teście utraty zasilania zastępujemy stary recorder.
8. Narzędzia konfiguracyjne, UI i archive przenosimy osobno albo pozostawiamy,
   jeśli ich przebudowa nie daje wartości.

## 12. Kryteria zakończenia pierwszego etapu

Fundament jest gotowy, gdy:

- protokół ma jedną wersjonowaną specyfikację i wspólne wektory testowe;
- rdzeń sesji nie importuje pyserial, h5py, HTTP ani kodu UI;
- symulowane testy obejmują timeout, duplikat, lukę, restart node, restart hosta,
  niejednoznaczny wynik release i błąd storage;
- test awarii dowodzi, że node nie traci danych przed trwałym zapisem;
- można podmienić `NodeLink` i `CaptureRepository` bez zmiany `NodeSession`;
- profil na docelowym hoście pokazuje wymagany zapas CPU, RAM i przepustowości.
