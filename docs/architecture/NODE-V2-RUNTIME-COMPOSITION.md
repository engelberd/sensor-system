# Kompozycja runtime node v2

- Status: granice core 0/core 1, produkcyjny `main_v2` i HIL jednej linii gotowe;
  pomiary obciążenia i fault injection pozostają otwarte
- Ostatnia aktualizacja: 2026-10-01

## Właściciele

Kompozycja zachowuje dwóch jednoznacznych właścicieli:

```text
core 0                                      core 1

SensorRuntime                              Core1MeasurementRuntime
  ├─ AcquisitionEngine                       ├─ MeasurementJournal
  ├─ MeasurementIngressSink                  ├─ DeliverySession
  └─ Core0SensorRuntime                       └─ MeasurementServiceHandler
         │                                             │
         ├── SampleIngress SPSC ──────────────────────>│
         └<─ OperationMailbox command/result ─────────>│
```

`Core0SensorRuntime` wykonuje najwyżej jedno polecenie mailboxa, a następnie
jeden krok `SensorRuntime`. Polecenie `Stop` działa więc przed kolejnym odczytem
sensora. Backpressure kolejki wyników nie zatrzymuje akwizycji: executor zachowuje
oczekujący wynik, a runtime nadal wykonuje swój ograniczony krok.

`Core1MeasurementRuntime` jest jedynym właścicielem journala, sesji delivery i
handlera Measurement Service. Konsumuje ograniczoną liczbę ingress events w
jednym wywołaniu. Budżet jest argumentem runtime, więc transport nie może zostać
zagłodzony przez nieograniczone opróżnianie kolejki.

## Propagacja krytycznej awarii

`RuntimeFaultLatch` jest jednokierunkowym, lock-free i write-once sygnałem core
1 → core 0. Zachowuje pierwszą przyczynę do restartu. Jest potrzebny, ponieważ po
odrzuceniu zdarzenia przez journal core 0 nie może czekać, aż pośrednio wyczerpie
się ingress.

Obecne przyczyny:

- `InvalidBootEpoch`;
- `MeasurementJournalRejected`.

Core 0 sprawdza latch przed obsługą polecenia i przed krokiem sensora. Aktywny
fault przenosi `SensorRuntime` do terminalnego `Faulted`, więc następna próbka nie
powstaje.

## Dowód pionowego przepływu

Test kompozycji przechodzi przez:

```text
Start command (Idle -> Recording)
  -> mailbox
  -> SensorRuntime
  -> sample / ContinuityBreak / sample
  -> ingress SPSC
  -> journal
  -> MeasurementServiceHandler
  -> ApplicationDispatcher
  -> decoded protocol 5 response
  -> completion
  -> Stop command (Recording -> Stopping)
  -> końcowy ingress drain
  -> Idle
```

Sprawdza również błędną kolejność źródłową, zerową epokę, zachowanie pierwszej
przyczyny faultu, kooperacyjny graceful stop i zatrzymanie core 0 przed następnym
odczytem sensora.

Lifecycle Service ma już codec C++/Rust, routing dispatchera i ograniczony cache
wyników. Rozróżnia nieprzyjęte `Busy` od przyjętego `InProgress` i uzgadnia
kooperacyjny `Stop` aż do terminalnego wyniku.

Koordynator pętli core 1 i produkcyjny composition root są gotowe w czystej
logice. Tymczasowe pojemności są scentralizowane w `RuntimeCapacityProfile` i
mają jawne limity RAM; ich wartości pozostają do strojenia HIL.

## Następna bramka

Composition root zawiera osobną operację systemową przejścia do bootloadera,
dozwoloną po `Stop` i wejściu w Maintenance. Jest zaimplementowana przez
oddzielny system mailbox, core-0 admission check i post-transmit latch. Restart
następuje dopiero po wysłaniu odpowiedzi, aby node nie potwierdzał operacji,
której host nie mógł odebrać. Gotowa pętla core 1
wykonuje kolejno:

1. bounded drain ingress;
2. bounded obsługa mailbox results;
3. jedno `TransportEndpoint::update`;
4. bounded observer/service telemetry.

Przed przełączeniem domyślnego firmware wymagany jest test HIL na board rev. 2.
Ostateczne pojemności zostaną dobrane z pomiarów najgorszego przypadku; ich
zmiana nie może wpływać na granice odpowiedzialności ani format protokołu.

Target `sensor_system_node_v2_integration` pozostaje diagnostycznym obrazem UF2
bez łańcucha A/B. Te same elementy protocol 5 są już składane jako slot A i B
oraz trafiają do paczki update. Paczka nie jest dopuszczona do zdalnego node,
dopóki fizyczny HIL nie potwierdzi zarówno confirm, jak i rollback.
