# Wspólny język domeny i własność stanu

Status: obowiązujący kontrakt architektoniczny.

Ten dokument definiuje znaczenie pojęć używanych przez firmware, host, protokół,
testy i dokumentację. Wspólne jest znaczenie pojęć, nie jedna globalna klasa ani
hierarchia dziedziczenia.

## 1. Reguła nadrzędna

Każda zmienna stanu ma dokładnie jednego właściciela. Inne komponenty mogą:

- wysłać command proszący właściciela o zmianę;
- odczytać niemutowalny snapshot;
- odebrać event opisujący wykonaną zmianę;
- odczytać metric opisującą pracę komponentu.

Nie mogą zmieniać cudzego stanu przez współdzielony wskaźnik, publiczne pole ani
niejawny efekt uboczny.

Współdzielone pojęcia implementujemy jako małe typy wartości i kontrakty.
Obiekty domenowe składają się z potrzebnych elementów. Nie tworzymy klas
`StatefulObject`, `HealthyObject`, `ObservableObject` ani globalnego
`SystemState`.

## 2. Podstawowe pojęcia

### Observability profile

Zamierzona polityka obserwacji wybrana przez operatora lub konfigurację
produktu. Profil odpowiada na pytanie: „ile dodatkowych obserwacji zbieramy?”.

Pierwszym właścicielem profilu jest `ChannelAgent`:

```text
ObservabilityProfile = Production | Diagnostic
```

Profil określa budżet obserwacji i telemetrii. Nie zmienia akwizycji ani
measurement stream. `Maintenance` należy do lifecycle i wymaga wcześniejszego
zakończenia Recording.

### State

Etap lifecycle dokładnie jednej maszyny stanów. State odpowiada na pytanie:
„na jakim etapie wykonania jest ten komponent?”.

Każda maszyna definiuje osobny typ, właściciela, legalne przejścia, timeouty i
zachowanie po restarcie. `AcquisitionState`, `DeliveryState`, `ChannelLifecycle`
i `RecordingState` nie dziedziczą po wspólnym `State`.

Kilka maszyn może jednocześnie opisywać jeden node, jeśli reprezentują
ortogonalne odpowiedzialności, na przykład:

```text
AcquisitionState = Running
DeliveryState    = AwaitingNextPull
ConfigState      = Stable
```

### Health

Wyliczona ocena zdolności komponentu do spełniania kontraktu:

```text
HealthLevel = Unknown | Healthy | Degraded | Faulted
```

Health zawiera powody, a nie tylko poziom. Nie zastępuje lifecycle. Poprawną
kombinacją jest `Running + Degraded`, gdy transfer trwa, ale wzrosła liczba
retransmisji.

`Unknown` oznacza brak wystarczającej wiedzy. `Degraded` oznacza pracę z
naruszonym marginesem lub ograniczoną funkcją. `Faulted` oznacza brak zdolności
do spełnienia podstawowego kontraktu komponentu.

### Command

Żądanie wykonania operacji. Może zostać odrzucone, może się nie udać i nie jest
dowodem zmiany stanu.

Command zmieniający stan ma:

- `operation_id` lub protokołowy identyfikator pozwalający rozpoznać powtórzenie;
- jawny target;
- zwalidowane parametry;
- określoną legalność zależną od profilu i state;
- zdefiniowaną semantykę ponowienia po niejednoznacznym wyniku.

Nazwy command używają trybu rozkazującego, np. `StartRecording`, `ApplyConfig`,
`EnterMaintenance`.

### Result

Wynik obsługi konkretnego command albo query. Result jest skorelowany z żądaniem
i nie jest eventem.

Operacje lokalne mogą zwrócić końcowy wynik synchroniczny. Operacje długie używają
statusu operacji:

```text
Accepted -> Running -> Succeeded
                    -> Failed
                    -> Cancelled
```

`Accepted` oznacza wyłącznie przyjęcie odpowiedzialności za wykonanie.

### Event

Niemutowalny fakt, który już zaszedł. Nazwa jest w czasie przeszłym, np.
`RecordingStarted`, `SequenceGapObserved`, `ConfigApplied`.

Kategorie eventów mają różne miejsca przechowywania i polityki utraty:

| Kategoria | Znaczenie | Przykład | Miejsce docelowe |
| --- | --- | --- | --- |
| Domain event | fakt ważny dla logiki produktu | `RecordingStarted` | runtime/audit według kontraktu |
| Quality event | fakt zmieniający interpretację pomiaru | `SamplesLostAtSensor` | Capture i Archive |
| Operational event | fakt o pracy procesu lub łącza | `ChannelReconnected` | log operacyjny |
| Audit event | kto lub co zażądało zmiany | `ConfigChangeRequested` | trwały audit trail |

Awaria zapisu best-effort operational event nie może zatrzymać drogi danych.
Quality event wymagany do interpretacji danych podlega zasadom trwałości produktu
Capture.

### Metric

Pomiar liczbowy, nie historia zdarzeń. Counter kończy się `_total` i nie maleje
w czasie życia procesu lub jawnie nazwanej epoki. Gauge opisuje bieżącą wartość
i zawiera jednostkę w nazwie, jeśli nie jest bezwymiarowy.

Przykłady:

```text
frames_crc_error_total
delivery_backlog_samples
durable_write_latency_ms
clock_uncertainty_ns
```

Metric może być wejściem `HealthEvaluator`, ale sama zmiana metryki nie jest
automatycznie eventem.

### Snapshot

Niemutowalny obraz obserwowalnego stanu w określonej chwili. Snapshot służy do
query, statusu i UI. Nie jest źródłem historii ani mechanizmem sterowania.

Snapshot zawiera czas wykonania, tożsamość źródła i typowane wartości. Konsument
zakłada, że snapshot może stać się nieaktualny natychmiast po publikacji.

## 3. Kompozycja zamiast dziedziczenia

Wspólne typy wartości mogą być zawierane w wielu snapshotach:

```text
ChannelSnapshot
  channel_id
  transport_binding
  ObservabilityProfile
  ChannelLifecycle
  HealthSnapshot

NodeSnapshot
  NodeIdentity
  AcquisitionState
  DeliveryState
  HealthSnapshot
```

`ChannelSnapshot` i `NodeSnapshot` nie dziedziczą po `SystemSnapshot`. Wspólna
serializacja może używać envelope zawierającego wersję, timestamp i source, ale
payload pozostaje typem właściwym dla kontekstu.

Interfejs zachowania wprowadzamy tylko, gdy istnieją co najmniej dwie sensowne
implementacje spełniające ten sam kontrakt, np. sprzętowa i symulowana:

```text
Clock             -> SystemClock | FakeClock
NodeLink          -> Rs485NodeLink | SimulatedNodeLink
CaptureRepository -> Hdf5CaptureRepository | MemoryCaptureRepository
```

Nie tworzymy interfejsu tylko po to, aby każda klasa miała klasę bazową.

## 4. Właściciele pojęć

| Obszar | Profile/policy | State | Health | Commands | Events/metrics/snapshot |
| --- | --- | --- | --- | --- | --- |
| Node acquisition | brak | `AcquisitionState` | wkład do `NodeHealth` | start, stop, maintenance action przez mailbox | sample/fault facts, acquisition metrics, acquisition snapshot |
| Node delivery | brak | `DeliveryState` | wkład do `NodeHealth` | pull/grant/commit | delivery facts, transfer metrics, delivery snapshot |
| Node configuration | brak | `ConfigState` | config fault | validate/apply/persist | `ConfigApplied`, revision i config snapshot |
| Node bootloader | `BootMode` lokalny dla bootloadera | `UpdateEngineState` | boot/update health | begin/write/finalize/activate | update events i progress snapshot |
| Host channel | `ObservabilityProfile` | `ChannelLifecycle` | `ChannelHealth` | start/stop/maintenance/change profile | line events, metrics, channel snapshot |
| Host node session | dziedziczy politykę kanału przez kompozycję, nie klasę | `NodeTransferState`, `ClockState` | `NodeSessionHealth` | request samples/commit/sync | quality i transfer events, metrics, node snapshot |
| Recording | polityka kanału jest wejściem | `RecordingState` | `RecordingHealth` | prepare/start/stop/finalize | lifecycle i quality events, storage metrics, recording snapshot |
| Operations | brak | `OperationState` per operation | health usługi | przyjmuje command operatora | result, audit event, read models |

„Dziedziczy politykę” w tabeli oznacza otrzymanie jawnej wartości lub portu
polityki. Nie oznacza dziedziczenia klas C++ ani Python.

## 5. Nieprzekraczalne granice

1. Profil nie może służyć jako wynik operacji.
2. State nie może kodować health ani przyczyny błędu w nazwie kombinowanej.
3. Health nie może być ustawiany przez UI ani adapter transportu.
4. Command nie może być logowany jako event wykonania przed sukcesem operacji.
5. Result nie zastępuje domain event, gdy inne komponenty muszą poznać trwały fakt.
6. Operational event nie może być jedynym zapisem luki lub utraty próbek.
7. Metric nie może być używana jako trwały cursor.
8. Snapshot nie może być modyfikowany przez konsumenta ani używany jako command.
9. Adapter wire/storage tłumaczy reprezentację, ale nie podejmuje decyzji domenowych.
10. Publiczne pole `state`, `status`, `time` lub `last_seq` bez właściciela i
    jednostki nie przechodzi review nowego kodu.

## 6. Minimalny kontrakt maszyny stanów

Każda nowa lub wydzielana maszyna stanów musi mieć tabelę:

| Current state | Input | Guard | Action | Next state | Timeout/failure |
| --- | --- | --- | --- | --- | --- |

oraz testy obejmujące:

- każde legalne przejście;
- odrzucenie nielegalnego przejścia;
- timeout;
- błąd akcji bez publikacji fałszywego sukcesu;
- powtórzony command;
- restart w każdym stanie nietrwałym.

## 7. Reguła wdrażania do istniejącego kodu

Nie wykonujemy masowej zmiany nazw ani jednorazowego rewrite'u. Dla każdego
pionowego fragmentu:

1. wskazujemy właściciela zmienianego stanu;
2. nazywamy command, result i fakty powstające podczas operacji;
3. definiujemy niezmienniki i limity;
4. dodajemy test zachowania;
5. przenosimy jedną odpowiedzialność;
6. usuwamy stare pole lub ścieżkę dopiero po przełączeniu wszystkich konsumentów.

Pierwsze planowane fragmenty to: kontrakt protokołu i golden vectors,
`NodeLink` z symulatorem, model cursorów/commit oraz wydzielenie grup command z
`NodeController`.

## 8. Kryterium ukończenia tego etapu

Słownik jest wdrożony architektonicznie, gdy:

- nowe dokumenty i kod używają znaczeń z tego dokumentu;
- każda maszyna z planu ma wskazanego właściciela;
- review odrzuca nowe ogólne klasy bazowe dla policy/state/health;
- kolejne zadania refaktoryzacyjne podają dotknięte commands, states, events,
  metrics, snapshots i niezmienniki;
- odstępstwo wymaga jawnej decyzji ADR zamiast lokalnego wyjątku w kodzie.
