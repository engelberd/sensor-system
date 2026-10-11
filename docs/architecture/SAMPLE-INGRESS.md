# SampleIngress core 0 → core 1

- Status: czysta logika zaimplementowana i przetestowana host-side
- Ostatnia aktualizacja: 2026-09-30
- Decyzje nadrzędne: [ADR-0009](../adr/0009-source-sequence-and-gap-ledger.md),
  [ADR-0010](../adr/0010-ordered-quality-events-and-service-observability.md)

## Odpowiedzialność

`SampleIngress` jest jedyną drogą zdarzeń pomiarowych od właściciela sensora na
core 0 do właściciela `MeasurementJournal` na core 1. Przenosi:

```text
Sample(sequence, SampleRecord)
Gap(first_missing, missing_count, reason)
ContinuityBreak(at_sequence, reason)
```

Nie steruje ADXL355, nie zapisuje journala, nie koduje protokołu i nie podejmuje
decyzji transportowych. `SourceSequencer` należy do producenta, dlatego sequence
powstaje przed kolejką.

## Kolejka SPSC

Transportem jest bounded single-producer/single-consumer ring. Używa dwóch
lock-free cursorów atomowych i pamięci statycznej. Producer publikuje gotowy slot
przez release, consumer odczytuje go przez acquire. Kolejka:

- nie używa sterty ani blokady;
- nie nadpisuje najstarszego elementu;
- nie czeka na drugi rdzeń;
- ma dokładnie skonfigurowaną liczbę slotów;
- pozwala consumerowi wykonać `front → journal append → commit pop`, więc
  odrzucony przez journal element pozostaje na froncie i prowadzi do jawnego
  faultu runtime.

`size_approx` służy wyłącznie diagnostyce. Poprawność nie zależy od chwilowego
wyniku obserwowanego równolegle przez oba rdzenie.

## Polityka przepełnienia

Jeżeli sample nie mieści się w kolejce:

1. jego sequence pozostaje zużyty;
2. producent tworzy lokalny pending gap `INGRESS_OVERFLOW`;
3. kolejne próbki nie mogą wyprzedzić pending gapu;
4. dopóki gap nie zostanie opublikowany, następne utracone próbki rozszerzają
   jego dokładny zakres;
5. po zwolnieniu slotu gap trafia do kolejki przed późniejszą próbką.

Jeśli w jednym ciągłym pending zakresie wystąpią różne przyczyny, zakres nadal
jest dokładny, a reason przechodzi na `UNKNOWN_DISCONTINUITY`. Nie raportujemy
fałszywie jednej przyczyny dla mieszanego zdarzenia.

Break nie może zostać scalony z gapem, ponieważ opisuje inną informację. Core 0
ma więc mały, stałopojemnościowy backlog pending quality events. Domyślna
pojemność to trzy wpisy i jest parametrem kompozycji. Producer zawsze próbuje
najpierw opublikować cały starszy pending prefix. Break, późniejszy gap i próbka
zachowują kolejność także wtedy, gdy główna kolejka była pełna.

Jeśli pending backlog zapełni się niezależnymi zdarzeniami, których nie można
uczciwie scalić, ingress ustawia `faulted` i nie przyjmuje kolejnych próbek.
Nie odrzuca po cichu breaka ani nie zamienia go na fałszywy exact gap. Runtime
musi wtedy zatrzymać akwizycję i rozpocząć recovery z nowym strumieniem.

Nieprawidłowy `SampleRecord` zostaje rozliczony jako `SOURCE_LOSS`. Naruszenie
wewnętrznej kolejności lub overflow arytmetyki ustawia `faulted`; dalsza
publikacja kończy się `InternalError`.

## Pamięć i dobór pojemności

`IngressEvent` nadal ma 56 B. Przykładowo 16 slotów zajmuje 896 B na rekordy
plus dwa atomowe cursory. Domyślne trzy pending sloty kosztują dodatkowo 168 B.
Obie pojemności pozostają parametrami kompozycji i zostaną dobrane z pomiaru
maksymalnej przerwy w obsłudze core 1.

Kolejka nie jest retencją komunikacyjną. Ma tylko zamortyzować krótki jitter
między rdzeniami. Właściwą retencją do retransmisji jest `MeasurementJournal`.

## Weryfikacja

Testy obejmują:

- przepełnienie zmienione w dokładny gap;
- zakaz wyprzedzenia pending gapu przez późniejszą próbkę;
- kolejność `sample → break → overflow gap → sample` przy pełnej kolejce;
- jawny fault po przepełnieniu bounded pending quality backlog;
- mieszane przyczyny i degradację do `UNKNOWN_DISCONTINUITY`;
- nieprawidłową próbkę jako jawny gap;
- pozostawienie odrzuconego zdarzenia na froncie kolejki;
- 200 000 elementów przy równoległym producer/consumer i wielokrotnym wrap;
- wykonanie testu współbieżnego pod ThreadSanitizer.

`MeasurementIngressSink` łączy już `AcquisitionEngine` z `SampleIngress` jako
testowana czysta kompozycja i zachowuje kolejność sample/break. Produkcyjny
`main` nadal używa legacy data plane; podłączenie runtime, kolejki i journala
jest osobną bramką integracyjną.
