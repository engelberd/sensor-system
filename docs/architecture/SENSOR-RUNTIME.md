# SensorRuntime node v2

- Status: lifecycle i profil obserwowalności zaimplementowane i przetestowane;
  brak kompozycji w produkcyjnym `main`
- Ostatnia aktualizacja: 2026-10-01

## Odpowiedzialność

`SensorRuntime` jest właścicielem przypadku użycia akwizycji na core 0. Nie zna
RS485, ramek, adresu node, journala ani ADXL355. Otrzymuje przez konstruktor
akwizycję oraz adapter measurement ingress.

```text
core 1 command -> OperationMailbox -> OperationExecutor
                                         |
                                         v
                                  SensorRuntime (core 0)
                                      |       |
                              Acquisition   MeasurementIngressSink
```

Lifecycle jest jawny:

- `Idle` — domyślny stan po poprawnym boot; sensor jest gotowy, ale nie zbiera
  danych;
- `Recording` — każde `poll` wykonuje ograniczony krok akwizycji;
- `Stopping` — sensor jest zatrzymany, FIFO zostało obsłużone, a runtime czeka
  na przejęcie końcowych ingress events przez core 1;
- `Maintenance` — wyłączna praca serwisowa bez akwizycji;
- `Faulted` — runtime zatrzymał akwizycję, ponieważ start sensora albo
  measurement path nie zachował wymaganych gwarancji.

`Faulted` jest terminalny dla akwizycji: zwykły `Start` nie ukrywa faultu ani
nie próbuje zgadywać polityki ponownej inicjalizacji sensora. Pozostaje jednak
zdalnie zarządzalny. `RestartNode` oraz `PrepareFirmwareUpdate` są dozwolone w
`Faulted`, aby instalacja obsługiwana wyłącznie po RS485 nie wymagała fizycznej
interwencji.

## Operacje

`GetState`, `Start`, `Stop`, `EnterMaintenance`, `ExitMaintenance` i
`SetObservabilityProfile` są stałymi typami wartości i mogą przechodzić przez
`OperationMailbox`. Przejścia są idempotentne. Wynik zawiera `operation_id`, kod,
lifecycle, profil obserwowalności i ostatni `SensorStatus`.

`RestartNode` jest osobną operacją systemową. Jest dozwolony wyłącznie w
`Idle`, `Maintenance` i `Faulted`; `Recording` oraz `Stopping` zwracają
`InvalidState`. Restart zostaje uzbrojony dopiero po fizycznym zakończeniu TX
odpowiedzi. `PrepareFirmwareUpdate` ma tę samą gwarancję post-transmit i jest
dopuszczone w `Maintenance` oraz `Faulted`.

Start wykonuje `prepare_for_runtime`, a dopiero po sukcesie wznawia akwizycję.
Stop wykonuje bounded `finish_recording`: blokuje nowe kroki, opróżnia pełne
próbki z ograniczonego sprzętowego FIFO i publikuje końcowy
`ContinuityBreak::ProcessingReset`. Jeżeli ingress nie został jeszcze przejęty
przez core 1, wynik ma kod `InProgress`, a lifecycle pozostaje `Stopping`.
Kolejne `poll` i ponowiony `Stop` są nieblokujące oraz idempotentne. `Idle`
zostaje osiągnięte dopiero, gdy nie ma lokalnego pending eventu ani zdarzenia w
kolejce core 0 → core 1.

`poll` sprawdza health ingress przed i po kroku akwizycji. Fault ingress
zatrzymuje sensor, ponieważ dalsza praca mogłaby produkować dane bez zachowanego
dowodu o ich ciągłości.

`EnterMaintenance` jest legalne wyłącznie z `Idle`. Host musi najpierw wykonać
`Stop`. `Start` w Maintenance zwraca `InvalidState`; samo `Stop` jest bezpiecznie
idempotentne i nie opuszcza Maintenance.

## Profil obserwowalności

Profil jest niezależny od lifecycle:

```text
Idle | Recording | Stopping | Maintenance
             ×
Production | Diagnostic
```

Dlatego `Recording + Production` i `Recording + Diagnostic` korzystają z tej
samej akwizycji, journala i protokołu. Profil zmienia wyłącznie ilość bounded
obserwacji serwisowych. Może zostać przełączony podczas Recording; nie może
zmieniać measurement stream ani jego dowodów jakości.

## Celowe granice obecnego fragmentu

Nie ma jeszcze operacji konfiguracji, self-testu ani update. Wejście i wyjście z
Maintenance stanowią tylko bezpieczną granicę dla tych przyszłych przypadków
użycia. Codec, dispatcher oraz bounded handler Lifecycle Service są gotowe. Nie
są jeszcze złożone z produkcyjnym `main` i adapterem RS485.

Lifecycle Service zachowuje rozróżnienie `InProgress`/`Ok` i pozwala hostowi
uzgodnić wynik po timeout przez ponowienie tego samego `operation_id` oraz
`GetState`.
