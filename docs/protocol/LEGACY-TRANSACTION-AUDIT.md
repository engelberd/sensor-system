# Audyt transakcji starego protokołu i katalog potrzeb v2

- Status: audyt istniejącej implementacji i kandydat do dalszego modelowania
- Date: 2026-09-29
- Zakres: application protocol; framing i bootloader update są opisane osobno

## 1. Co naprawdę istniało

Stary application protocol używał jednej płaskiej listy `CommandType`.

| Grupa | Transakcje |
| --- | --- |
| System | `Ping`, `Restart`, `EnterBootloader`, `GetVersion`, `RunSelfTest` |
| Konfiguracja | `GetConfig`, osobne `SetNodeId`, `SetOdr`, `SetRange`, `SetOffsets`, `SetFifoWatermark`, `SetHighPass`, `SetBaudRate`, `SaveConfig`, `LoadConfig`, `ResetConfigToDefaults` |
| Commissioning | `CommissionDiscover`, `CommissionAssignNodeId` |
| Stan i czas | `GetStatus`, `GetTemperature`, `GetBufferState`, `GetStats`, `GetCapabilities`, `TimeSync` |
| Diagnostyka | `GetDiagnosticInfo`, `GetFaultSnapshot`, `ReadDiagnosticEvents`, dwa polecenia `Clear...` |
| Dane | `ReadLatest`, `ReadFromSeq`, `GrantBurstRead`, `CommitReadUpTo` |

Bootloader miał osobny protokół: `Hello`, `Begin`, `Chunk`, `End`, `Abort` oraz
`Ack/Error`. Rozdzielenie bootloadera od protokołu aplikacji jest warte
zachowania.

Lista jest długa głównie dlatego, że detale implementacji stały się publicznymi
poleceniami. Przykładem jest osobne ustawianie każdego pola, a następnie osobne
`SaveConfig`. Nie każda stara komenda reprezentuje trwałą potrzebę produktu.

## 2. Jak działał stary transfer danych

```text
host -> node : GrantBurstRead(start_seq, max_frames)
node -> host : grant result
node -> host : DATA packet 1
node -> host : DATA packet 2
node -> host : DATA packet 3
node -> host : DATA packet 4

host zapisuje i flushuje dane

host -> node : CommitReadUpTo(last_sample_seq)
node -> host : committed_sample_seq
```

`GrantBurstRead` rozpoczynał serię spontanicznych ramek DATA. Ramki DATA miały
własny `frame sequence` i `packet_seq`, ale nie powtarzały identyfikatora
transakcji grant. Host zbierał wszystkie poprawnie zdekodowane DATA z danego
adresu aż do limitu albo timeoutu.

Node zachowywał pakiety w RAM do `CommitReadUpTo`. Commit przesuwał lokalny
`committed_sample_seq` i usuwał z kolejki pełne pakiety kończące się nie później
niż podana próbka.

## 3. Czym był commit

Commit nie oznaczał utworzenia pliku ani zatwierdzenia pomiaru. Był komunikatem:

> Host zapewnia, że wszystkie próbki do numeru N są zapisane trwale. Node może
> zwolnić ich kopie z ograniczonego bufora retransmisji.

Ta potrzeba jest prawdziwa, ale nazwa `CommitReadUpTo` zacierała odpowiedzialność.
W v2 używamy pojęć:

- `durable_through`: największy ciągły `sample_seq`, który przeżyje restart hosta;
- `release_through`: zgoda hosta na usunięcie tego samego ciągłego prefiksu z
  retencji node;
- `retained_from`: najstarsza próbka, którą node nadal może retransmitować.

Host nigdy nie może przesunąć `release_through` przez niewyjaśnioną lukę.

## 4. Znalezione źródła zawodności

### 4.1 Commit przechodził przez lukę

W `record_one_burst` host po wykryciu, że `packet.first_sample_seq` nie zgadza się
z oczekiwanym numerem, zapisywał gap, przestawiał oczekiwanie na otrzymany pakiet
i kontynuował. Na końcu commitował `last_contiguous_seq` ostatniego otrzymanego
pakietu.

Jeżeli środkowy pakiet zginął tylko w transmisji, commit obejmował także jego
zakres. Node usuwał wtedy jedyną kopię danych, które mogły zostać
retransmitowane. Chwilowy błąd transmisji stawał się trwałą luką.

### 4.2 DATA nie należało jednoznacznie do grantu

Grant miał request `sequence`, ale kolejne DATA dostawały niezależne numery
ramek. Odbiornik filtrował je po typie, source i destination. Nie sprawdzał
`transaction_id` grantu, ścisłej kolejności `packet_seq`, zadeklarowanego zakresu
ani duplikatów przed przekazaniem ich wyżej.

### 4.3 Niejednoznaczny wynik po utracie odpowiedzi commit

`CommitReadUpTo` był idempotentny dla niższej lub równej wartości, ale po utracie
odpowiedzi host nie uzgadniał jawnie cursora. Node mógł już usunąć dane, podczas
gdy host nadal używał starego `expected_sample_seq`. Kolejny transfer mógł zostać
oznaczony jako fałszywa luka.

### 4.4 Node przyjmował zbyt daleki commit

Implementacja przyjmowała dowolny większy `last_sample_seq`, bez sprawdzenia,
czy zakres został wysłany, istnieje w bieżącym `boot_epoch` i nie wykracza poza
najnowszą próbkę. Poprawnie oprawiona, ale błędna logicznie komenda mogła
przesunąć cursor w przyszłość.

### 4.5 Burst blokował prostą politykę magistrali

Po grancie node mógł nadać do 16 ramek bez kolejnych requestów hosta. Domyślnie
host prosił o cztery. Utrudniało to wstawienie sync lub krótkiego polecenia,
wyznaczenie maksymalnego czasu zajęcia magistrali i korelację odpowiedzi.

### 4.6 Parser hosta miał słabą resynchronizację

Stary framing opierał się na dwubajtowym magic, długości i CRC16. Host usuwał z
bufora całą długość wskazaną przez header przed stwierdzeniem błędu CRC. Błędna,
ale mieszcząca się w limicie długość mogła więc pochłonąć bajty następnej ramki.
To jest jeden z powodów wyboru delimitera COBS w kandydacie framing v3.

### 4.7 Scheduler był pętlą operacji

Dla każdego node wykonywano kolejno ewentualny sync, dodatkowe odczyty i burst.
Nie istniała wspólna decyzja oparta o deadline sync, czas do overflow, koszt
transakcji i retry. Zachowanie pogarszało się wraz z liczbą node.

## 5. Prostszy transfer v2

Normalny transfer ma jedną odpowiedź na jeden request:

```text
PULL_SAMPLES request:
  stream_id
  start_seq
  max_samples albo max_payload_bytes
  release_through          # opcjonalny durable cursor z poprzedniego zapisu

PULL_SAMPLES response:
  stream_id
  retained_from
  newest_seq
  first_seq
  sample_count
  more_available
  timing evidence
  raw XYZ
```

Host wykonuje następującą sekwencję:

1. prosi od dokładnie pierwszej brakującej próbki;
2. przyjmuje tylko ciągły zakres z właściwego `stream_id`;
3. zapisuje zakres do crash-recoverable journal;
4. wyznacza największy ciągły `durable_through`;
5. przekazuje go jako `release_through` w następnym `PULL_SAMPLES`.

Piggyback cursora usuwa osobną transakcję z normalnej pętli. Osobny
`RELEASE_SAMPLES` pozostaje potrzebny przy spokojnym zatrzymaniu albo gdy długo
nie będzie kolejnego pull.

Obie operacje są idempotentne. Powtórzenie tego samego pull może zwrócić te same
próbki, a host deduplikuje je po `(stream_id, sample_seq)`. Powtórzenie release
nie zmienia wyniku. Jeśli odpowiedź zginie, nie powstaje nieodwracalna luka.

`stream_id` identyfikuje ciągłość po restarcie node. Powinien zawierać lub
jednoznacznie zastąpić obecny `boot_epoch`.

## 6. Katalog potrzeb v2

To katalog odpowiedzialności, nie zamrożona lista bajtowych ID.

### Identity

- `GET_IDENTITY`: hardware ID, node address, board revision, wersje firmware,
  framing i application protocol oraz dostępne service;
- `GET_CAPABILITIES`: tylko cechy opcjonalne, których nie określa sama wersja.

Zwykły `Ping` nie jest niezbędny: poprawny `GET_IDENTITY` albo `GET_HEALTH`
potwierdza komunikację i mówi więcej.

### Measurement

- `GET_STREAM_STATE`: `stream_id`, `retained_from`, `newest_seq`,
  `release_through`, pojemność i jawne informacje o utracie;
- `PULL_SAMPLES`: jeden ograniczony, ciągły zakres w jednej odpowiedzi;
- `RELEASE_SAMPLES`: opcjonalne jawne zwolnienie trwałego prefiksu;
- możliwe `START_ACQUISITION` i `STOP_ACQUISITION`, jeśli produkt zdecyduje, że
  dotyczą pracy sensora, a nie tylko sesji hosta.

Nie przenosimy `ReadLatest`, `ReadFromSeq` i `GrantBurstRead` jako trzech
równorzędnych mechanizmów. `PULL_SAMPLES(start_seq, limit)` pokrywa niezawodny
transfer. Podgląd „latest” może korzystać z danych hosta lub osobnej diagnostyki.

### Time

- `SYNC_CLOCK`: krótki exchange z timestampami brzegowymi;
- `GET_TIME_STATE` tylko jeśli dane potrzebne schedulerowi nie mieszczą się w
  kompaktowym health.

Host buduje model czasu. Nie ustawiamy zegara node każdą synchronizacją.

### Health i diagnostics

- `GET_HEALTH`: mały, stabilny zestaw potrzebny w Production;
- `GET_DIAGNOSTIC_SNAPSHOT`: rozbudowane liczniki tylko w Diagnostic;
- `READ_EVENTS(after_event_id, limit)`;
- `RELEASE_EVENTS(through_event_id)` zamiast niejednoznacznego `Clear`;
- `RUN_SELF_TEST` w Maintenance.

### Configuration

- `GET_CONFIG` zwraca pełny snapshot i `config_revision`;
- `VALIDATE_CONFIG` może sprawdzić konfigurację bez zastosowania;
- `APPLY_CONFIG(expected_revision, config, apply_policy)` atomowo stosuje pełny
  spójny zestaw i zwraca nową revision oraz `effective_sample_seq`;
- `RESET_CONFIG` jest operacją Maintenance.

Jeden atomowy model zastępuje serię `Set...` plus `SaveConfig`, w której awaria
pośrodku pozostawiała niejasny stan. Zmiana baudrate wymaga osobnego, bezpiecznego
handshake i planu odzyskania łączności.

### Lifecycle i maintenance

- `GET_OPERATION_STATE`;
- `START_RECORDING` i `STOP_RECORDING`, jeśli recorder ma zdalnie definiować
  sesję produktu;
- `RESTART`;
- `ENTER_BOOTLOADER`;
- `CONFIRM_TRIAL_BOOT` dopiero po lokalnym health i handshake z hostem v2.

Transfer obrazu firmware pozostaje w małym protokole bootloadera.

### Commissioning

- odczyt niezmiennej tożsamości;
- bezkolizyjne znalezienie niezaadresowanych node;
- przypisanie i weryfikacja adresu.

Dokładny algorytm discovery wymaga osobnego projektu, ponieważ wiele
niezaadresowanych node nie może odpowiedzieć jednocześnie na broadcast.

## 7. Czy service można modelować niezależnie

Tak. Każdy service powinien mieć osobny moduł z własnymi message types, payload,
walidacją, handlerem i testami. Nie powinien jednak samodzielnie implementować
framingu, retry ani dostępu do UART.

Wspólne dla wszystkich service pozostają:

- envelope: wersja, adresy, service, message i `transaction_id`;
- jeden request/response na channel;
- status i klasy błędów;
- limity rozmiaru i czasu;
- reguły retry i idempotency;
- dozwolony system mode;
- właściciel zmienianego stanu;
- obserwowalność bez wpływu na semantykę operacji.

Nowa transakcja może zostać dodana wtedy, gdy pojawi się potrzeba domenowa, ale
przed nadaniem ID musi mieć opisane: intencję, preconditions, skutek, odpowiedź,
zachowanie po timeout, możliwość bezpiecznego retry i dozwolony mode. Dzięki
temu rozwój pozostaje otwarty bez powrotu do jednego rosnącego `switch`.

## 8. Decyzje jeszcze niezamrożone

- czy acquisition node działa stale, czy `START/STOP` faktycznie steruje
  sensorem;
- dokładna definicja `stream_id` i zachowanie przy restarcie;
- maksymalny zakres `PULL_SAMPLES` oraz reguły dynamicznego chunku;
- format timing evidence w odpowiedzi danych;
- atomowa konfiguracja oraz bezpieczna zmiana parametrów łącza;
- commissioning wielu nowych node na wspólnej linii;
- które pola należą do kompaktowego health.

Te decyzje mogą powstawać service po service. Inwariant ciągłego durable cursora
i zakaz zwalniania danych przez lukę należy zamrozić przed implementacją.
