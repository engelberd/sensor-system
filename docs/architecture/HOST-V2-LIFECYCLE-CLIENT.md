# Host v2 Lifecycle Client

- Status: model operacji, `ChannelAgent` i adapter kanału gotowe; fizyczny HIL
  jednej linii, lifecycle i trial confirm zaliczone
- Ostatnia aktualizacja: 2026-10-06

## Dwie tożsamości

Każda próba wire otrzymuje nowy `transaction_id`. Jedna logiczna operacja
`Start`, `Stop` albo `PrepareFirmwareUpdate` zachowuje natomiast ten sam
`operation_id` i identyczny payload przez timeouty oraz odpowiedzi
`InProgress`.

```text
logical operation_id = 77
  wire transaction 10 -> timeout
  quarantine
  wire transaction 11 -> InProgress
  wire transaction 12 -> Ok
```

`TransactionCore` chroni kanał przed spóźnioną ramką poprzedniej próby.
`LifecycleOperation` rozstrzyga semantykę retry i reconciliation powyżej tej
warstwy.

## Stany

- `ReadyToSend` — operacja jeszcze nie została przyjęta albo node odpowiedział
  application `Busy`;
- `AwaitingResponse` — aktywna próba wire;
- `ReconcileRequired` — timeout lub lifecycle `InProgress`; następna próba
  zachowuje `operation_id`;
- `Complete` — otrzymano terminalny wynik, również terminalny błąd domenowy;
- `Failed` — payload albo `operation_id` odpowiedzi naruszył kontrakt.

Handshake w profilu recording wymaga teraz Identity, Measurement i Lifecycle.
Node bez Lifecycle Service nie może być uznany za gotowy recorder v2.

## Integracja z kanałem

`ChannelAgent` jest jedynym właścicielem `TransactionCore` jednego kanału.
Identity handshake i Lifecycle korzystają z tej samej ograniczonej kolejki,
więc nie mogą utworzyć dwóch równoległych transakcji na half-duplex RS485.
Agent:

- nadaje osobne `operation_id` i `transaction_id`;
- zachowuje logiczną operację przez timeout, quarantine, `Busy` i
  `InProgress`;
- obraca pracę wielu node bez nakładania transakcji;
- publikuje małe zdarzenie `TransmitReady`, a ramkę przekazuje jawnie przez
  `take_outbound()`;
- uzbraja timeout odpowiedzi dopiero przez `transmit_completed()`, po wyjściu
  całej ramki z portu;
- po niepewnym lub częściowym TX wykonuje reconciliation z tym samym
  `operation_id`;
- rozróżnia `Busy` od stanu runtime i nie dopisuje fikcyjnego `Idle`.

`ChannelEndpoint` składa agenta z fizycznym portem, stream decoderem i zegarem.
Measurement korzysta z tego samego arbitra i przekazuje pełny chunk do kontraktu
trwałego zapisu. Szczegóły opisuje
[HOST-V2-CHANNEL-ENDPOINT.md](HOST-V2-CHANNEL-ENDPOINT.md).
