# Core1ProtocolRuntime node v2

- Status: koordynator jest podłączony do `main_v2` i sprawdzony HIL jednej linii;
  heartbeat, high-water marks i test maksymalnego obciążenia pozostają otwarte
- Ostatnia aktualizacja: 2026-10-01

## Odpowiedzialność

`Core1ProtocolRuntime` jest wyłącznie koordynatorem ograniczonych kroków. Nie zna
znaczenia próbek, poleceń ani ramek. W każdym `poll` wykonuje dokładnie tę samą
kolejność:

1. opróżnia najwyżej skonfigurowany budżet measurement ingress;
2. odbiera najwyżej jeden wynik Lifecycle Service;
3. wykonuje jeden krok `TransportEndpoint`;
4. wykonuje jeden krok obserwatora.

Żaden z tych elementów nie zawiera pętli czekającej na pracę innego rdzenia lub
hosta. Awaria measurement ingress nie wyłącza transportu ani obserwatora, dzięki
czemu node może nadal ujawnić stan błędu.

Polityka z zerowym budżetem ingress jest odrzucana przed wykonaniem jakiejkolwiek
części pętli. Liczniki koordynatora są saturujące i nie należą do measurement
stream.

## Kompozycja

`node_v2::DataPlane` jest właścicielem kolejki SPSC, `SampleIngress` oraz
`MeasurementIngressSink`. Powstaje przed `AcquisitionEngine`, aby przekazać mu
gotowy sink bez zależności cyklicznej.

`node_v2::RuntimeComposition` składa:

```text
core 0                         granice SPSC                 core 1

SensorRuntime       -> ingress queue ->       Core1MeasurementRuntime
Core0SensorRuntime <-> command/result mailbox <-> LifecycleService
                                                   Dispatcher
                                                   TransportEndpoint
                                                   Core1ProtocolRuntime
```

Kompozycja jest composition root. Udostępnia właścicieli przypadków użycia, ale
nie przenosi ich reguł do `main`.

## Weryfikacja

Testy sprawdzają stałą kolejność, ograniczony budżet, obsługę transportu podczas
faultu ingress, routing rzeczywistej ramki lifecycle oraz złożenie produkcyjnych
typów bez globalnych singletonów.
