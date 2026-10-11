# Mailbox operacji core 1 ↔ core 0

- Status: mechanika zaimplementowana i przetestowana host-side
- Ostatnia aktualizacja: 2026-09-30

## Cel i właściciele

Mailbox jest lokalną, ograniczoną granicą między protokołem na core 1 a jedynym
właścicielem sensora na core 0:

```text
core 1: service/controller -> Command(operation_id) ─┐
                                                      ├─ bounded mailbox
core 1: service/controller <- Result(operation_id)  ─┘
                                 core 0: SensorRuntime
```

`OperationMailbox` zawiera dwie niezależne kolejki SPSC:

- command: jeden producer na core 1 i jeden consumer na core 0;
- result: jeden producer na core 0 i jeden consumer na core 1.

Nie ma sterty, blokad, oczekiwania aktywnego ani nadpisywania. Pojemności są
parametrami kompozycji i są znane w czasie kompilacji.

## Gwarancja wykonania przy backpressure

`OperationExecutor` wykonuje najwyżej jedno z poniższych działań w jednym
`poll`:

1. publikuje wcześniej zatrzymany wynik;
2. wykonuje jedno polecenie i publikuje wynik;
3. wykonuje jedno polecenie i zatrzymuje jego wynik, jeśli kolejka wyników jest
   pełna;
4. nie robi nic, jeśli nie ma pracy.

Gdy wynik nie mieści się w kolejce, executor przechowuje dokładnie jeden
`pending_result`. Do czasu jego publikacji nie pobiera następnego polecenia.
Wykonane polecenie nie wraca do kolejki i nie jest wykonywane drugi raz.

Executor zawsze przepisuje `operation_id` z polecenia do wyniku. Błąd handlera
nie może więc przypisać wyniku do innej operacji.

## Czego mailbox nie rozstrzyga

Mailbox nie zna:

- ramek, transaction ID, adresu node ani timeoutu RS485;
- znaczenia start, stop, konfiguracji lub self-testu;
- polityki retry i idempotency poleceń przychodzących z hosta;
- tego, czy wynik ma zostać zapisany w cache odpowiedzi;
- deadline ani anulowania operacji sensora.

Timeout transakcji na hoście nie oznacza, że operacja na node nie została
wykonana. Warstwa usługi na core 1 musi zachować aktywne `operation_id` do
odebrania wyniku i umożliwić późniejsze uzgodnienie stanu. Nie wolno usuwać
opóźnionego wyniku tylko dlatego, że host przestał czekać na poprzednią ramkę.

## Pierwsze operacje runtime

Typy `SensorRuntimeCommand` i `SensorRuntimeResult` oraz obsługa `GetState`,
`Start`, `Stop`, `EnterMaintenance`, `ExitMaintenance` i zmiana profilu
obserwowalności są gotowe. `SensorRuntime` jest właścicielem przejść stanu na
core 0, a `OperationExecutor` zachowuje korelację i backpressure. Przejścia są
idempotentne, co pozwala bezpiecznie uzgodnić stan po timeout hosta.

`Stop` może zwrócić `InProgress` ze stanem `Stopping`. Oznacza to zaakceptowaną
operację, której końcowe zdarzenia czekają jeszcze na przejęcie przez core 1.
Ponowienie tego samego zamiaru nie wykonuje drugiego drainu FIFO; host uzgadnia
zakończenie przez ponowiony `Stop`, `GetState` albo przyszły cache operacji.

Nie są jeszcze podłączone do ramek protocol 5 ani do kompozycji produkcyjnej w
`main`.

## Celowo brakujące operacje

Nie tworzymy jeszcze pustych handlerów ani enumów dla przyszłych możliwości.
W następnych pionowych fragmentach trzeba dodać i przetestować:

- atomowe zastosowanie konfiguracji pomiaru;
- self-test i odczyty serwisowe;
- przypadki użycia wykonywane wewnątrz Maintenance, w tym update;
- trwały zapis konfiguracji;
- cache/reconciliation dla ponowionego polecenia hosta — gotowe w
  `LifecycleServiceHandler`; pozostaje podłączenie do produkcyjnego `main`.

Konfiguracja i operacje serwisowe zostaną dołączone jako osobne przypadki użycia
wewnątrz gotowej granicy Maintenance.

## Weryfikacja

Testy dowodzą, że:

- pełna kolejka command nie nadpisuje wcześniejszego polecenia;
- jeden `poll` wykonuje najwyżej jedno polecenie;
- wynik zawsze zachowuje `operation_id` pobranego polecenia;
- pełna kolejka result zatrzymuje wynik i blokuje pobranie następnego command;
- wielokrotne `poll` podczas backpressure nie wykonuje operacji drugi raz;
- po zwolnieniu miejsca wyniki zachowują kolejność.

Właściwości współbieżne samego SPSC są sprawdzane oddzielnie przez test
`SampleIngress` z 200 000 elementów i ThreadSanitizerem.
