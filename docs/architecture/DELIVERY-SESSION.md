# DeliverySession firmware v2

- Status: czysta logika zaimplementowana i przetestowana host-side
- Ostatnia aktualizacja: 2026-09-30

## Odpowiedzialność

`DeliverySession` jest pojedynczym właścicielem reguł udostępniania
`MeasurementJournal` hostowi. Waliduje tożsamość uruchomienia, release i cursory,
przygotowuje ograniczony chunk, buduje dowód czasu oraz pozwala zakończyć albo
anulować próbę delivery. Nie zna framingu, codeców, UART, schedulera ani trwałego
storage hosta.

Implementacja znajduje się w `node/measurement/delivery_session.h`, a niezależne
obliczanie timing evidence w `node/measurement/sample_timing.h`. Obiekty sesji i
journala są niekopiowalne, dzięki czemu kod nie może przypadkowo utworzyć dwóch
właścicieli tych samych cursorów.

## Przygotowanie i zakończenie odpowiedzi

```text
Idle
  │ prepare_pull(valid request)
  ▼
Prepared(token, range, max two spans)
  ├─ complete(token) -> Idle, journal delivered przesunięty
  └─ abort(token)    -> Idle, delivered bez zmian
```

`prepare_pull` nie oznacza danych jako dostarczonych. Zwraca jednorazowy token i
widok na maksymalnie dwa fragmenty ring buffer. Handler ma zakodować odpowiedź i
przekazać ją do ograniczonej kolejki transportu. Dopiero zaakceptowanie kompletnej
odpowiedzi przez transport pozwala wywołać `complete(token)`. Błąd kodowania albo
pełna kolejka kończą się `abort(token)`.

Token chroni przed spóźnionym callbackiem poprzedniej operacji. Niewłaściwy token
nie zmienia stanu, a mutacja journala pomiędzy prepare i complete powoduje
`JournalChanged`. W stanie `Prepared` kolejny pull i osobny release zwracają
`Busy`.

## Wyniki pull

| Decyzja domenowa | Znaczenie dla handlera |
| --- | --- |
| `SampleChunk` | zakoduj chunk, potem complete albo abort |
| `Gap` | zakoduj dokładny zakres, potem complete albo abort |
| `ContinuityBreak` | zakoduj znacznik nieciągłości, potem complete albo abort |
| `NoData` | odpowiedź Measurement `NO_DATA` |
| `RangeUnavailable` | odpowiedź `RANGE_UNAVAILABLE` z pełnym stanem |
| `StreamChanged` | odpowiedź `STREAM_CHANGED` z aktualnym boot epoch |
| `InvalidRelease` | odpowiedź `INVALID_RELEASE` z pełnym stanem |
| `CursorAhead` | wspólny application status `InvalidState` |
| `InvalidRequest` | wspólny status `MalformedRequest` |
| `Busy` | wspólny status `Busy` |
| `InternalError` | wspólny status `InternalError` |

Żaden z tych wyników nie koduje bajtów. Mapowanie należy do
`MeasurementServiceHandler`.

## Release i retransmisja

Release z `PULL_SAMPLES` jest stosowany przed odczytem i jest idempotentny.
Nie może wyprzedzić zakresu wcześniej przygotowanego i zakończonego przez
`complete`. Journal dopuszcza także release do `retained_from`, gdy host trwale
zapisał już dokładny gap po overflow.

Timeout hosta nie przesuwa release. Powtórzenie pull od tego samego `from`
ponownie zwraca ten sam zachowany zakres. `transaction_id` pozostaje własnością
framingu i Transaction Core, dlatego nie jest stanem `DeliverySession`.

## Timing evidence

`sample_timing.h` wyznacza pierwszą i ostatnią kotwicę, największy residual
względem liniowej rekonstrukcji oraz agreguje flagi jakości. Obliczenie jest
ograniczone rozmiarem jednego chunku. Odrzucane są odwrócone lub niemonotoniczne
timestampy oraz równe kotwice oznaczone jako `VALID` dla wielu próbek.

## Integracja z handlerem

Warstwa `MeasurementServiceHandler` jest gotowa i opisana w
[MEASUREMENT-HANDLER.md](MEASUREMENT-HANDLER.md). Handler:

1. dekoduje request protokołu v5;
2. tłumaczy go na command `DeliverySession`;
3. koduje wynik bez pełnej kopii chunku;
4. wywołuje complete dopiero po przyjęciu ramki przez transport;
5. zawsze wywołuje abort, jeżeli przygotowanej odpowiedzi nie udało się oddać.

Pozostało podłączenie handlera do dispatchera i przekazanie tokenu do właściciela
kolejki transportu.
