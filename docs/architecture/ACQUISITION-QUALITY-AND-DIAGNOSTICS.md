# Akwizycja, jakość danych i diagnostyka node

- Status: model i kompozycja `main_v2` działają fizycznie; deferred zapis
  diagnostyki i budżetowana telemetria pozostają otwarte
- Decyzja: [ADR-0010](../adr/0010-ordered-quality-events-and-service-observability.md)
- Ostatnia aktualizacja: 2026-09-30

## Podział odpowiedzialności

```text
ADXL adapter
  │ raw samples / typed read outcome
  ▼
Acquisition pipeline ──> ordered Sample | ExactGap | ContinuityBreak
  │                                      │
  │ cheap observations                   ▼
  └─> ProductionMetrics             SampleIngress -> Journal
           │
           └─> Health / bounded ServiceEventBuffer
                         │ diagnostic budget only
                         ▼
                    service endpoint
```

`Acquisition pipeline` odpowiada za kolejność faktów pomiarowych. Metrics i
service diagnostics obserwują tę pracę, ale nie tworzą faktów pomiarowych po
czasie.

## Co musi działać zawsze

Niezależnie od trybu:

- każda opublikowana próbka ma źródłową kolejność;
- utrata gotowej próbki staje się dokładnym gapem;
- potwierdzona nieciągłość o nieznanym rozmiarze staje się breakiem;
- późniejsza próbka nie wyprzedza oczekującego quality event;
- failure diagnostyki nie zatrzymuje ani nie zmienia strumienia;
- krytyczny failure measurement path jest jawny i może zatrzymać runtime.

## Wynik jednej transakcji FIFO

`FifoReadOutcome` jest wartością zwracaną przez dokładnie jedno wywołanie
sterownika. Zawiera status, poprawny prefiks próbek, dokładną liczbę odrzuconych
surowych wejść, stan ciągłości oraz przyczynę jej zerwania. `AcquisitionEngine`
nie rekonstruuje już wyniku operacji z później pobranego
`SensorDiagnosticSnapshot`.

`discarded_input_samples` nie jest liczbą brakujących próbek wyjściowych. Przed
filtrem i decymacją nie istnieje bezpieczne przeliczenie jeden do jednego.
Dlatego błędne dane FIFO i niepewny transfer tworzą
`ContinuityBreak::SensorReadUncertain`, a liczba wejść pozostaje metryką
diagnostyczną. Sticky overrun tworzy `SensorOverflow`, a nieważne powiązanie
czasu tworzy `InvalidTiming`.

`MeasurementIngressSink` zamienia próbki wyjściowe na `SampleRecord` i publikuje
break przez ten sam `SampleIngress`. Dzięki temu kolejność jest ustalana na core
0 w momencie zdarzenia. Przepełnienie kolejki gotowych próbek nadal staje się
dokładnym `Gap::IngressOverflow`.

## Profile obserwowalności

| Właściwość | Production | Diagnostic |
| --- | --- | --- |
| Sample/Gap/Break | pełne | pełne |
| Saturating metrics | tak | tak |
| Fault latch/snapshot RAM | tak | tak |
| Szczegółowy event ring | minimum health | rozszerzony, bounded |
| Telemetria po RS485/USB | mały budżet | jawny większy budżet |
| `printf` w pętli próbek | nie | nie |
| Flash erase/program podczas Recording | nie | nie |
| Self-test podczas Recording | nie | nie |

Profil określa ilość obserwacji. Nie włącza ani nie wyłącza dowodów jakości
danych.

`Maintenance` jest stanem lifecycle, a nie trzecim profilem obserwowalności. W
Maintenance akwizycja jest zatrzymana, więc można uruchomić self-test, pełny dump
lub deferred flash writer zgodnie z osobnym budżetem operacji serwisowej.

## Inwentaryzacja problemów legacy

| Obecne zachowanie | Problem | Migracja v2 |
| --- | --- | --- |
| `dropped_samples` | brak położenia w strumieniu | ordered quality event |
| `last_fifo_discarded_samples` | snapshot nadpisywany kolejnym read | outcome jednego odczytu |
| sticky FIFO overrun liczony jako 1 | dolna granica udaje dokładną stratę | `ContinuityBreak` |
| invalid timing zwiększa counter | brak segment boundary w danych | break + nowy timing segment |
| `DiagnosticStore::record_fault` zapisuje flash | flash w sekcji krytycznej akwizycji | pending snapshot + deferred writer |
| logi `printf` w `main` | tekst i timing zależne od build/USB | boot/service adapter poza hot path |
| 512 wpisów zawsze w RAM | stały koszt bez profilu | pojemność kompozycji/profilu |

## Kolejność migracji

1. Gotowe: typowany wynik pojedynczego odczytu FIFO i adapter ADXL355.
2. Gotowe: emisja `ContinuityBreak` z akwizycji do measurement ingress.
3. Gotowe jako czysta logika: lifecycle `Idle`, `Recording`, `Stopping`,
   `Maintenance`, `Faulted`, graceful stop i niezależny profil
   `Production`/`Diagnostic`.
4. Gotowe: runtime, ingress, journal i executor mailboxa są podłączone w
   `main_v2` używanym przez obrazy A/B protocol 5.
5. Przenieść persistent diagnostics do deferred writera uruchamianego po
   zatrzymaniu albo w kontrolowanym oknie serwisowym.
6. Dodać profile Production/Diagnostic do service telemetry i sprawdzić ich
   koszt benchmarkiem na RP2350.

## Kryteria ukończenia

- test pokazuje kolejność `Sample -> Break -> Sample` i
  `Sample -> ExactGap -> Sample` przez ingress, journal, wire i host;
- fault injection SPI/FIFO/timing nie tworzy fałszywego `missing_count`;
- wyłączenie całej diagnostyki serwisowej nie zmienia measurement stream;
- zapełnienie event ring nie blokuje akwizycji i zwiększa counter;
- żaden call graph z obsługi FIFO nie dochodzi do flash erase/program;
- benchmark porównuje koszt Production i Diagnostic przy maksymalnym wspieranym
  ODR.
