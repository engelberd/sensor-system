# ADR 0003: Wspólny język domeny i własność stanu

- Status: accepted
- Date: 2026-09-24

## Context

Firmware i host używają słów `state`, `status`, `event`, `config` i `last_seq`
dla danych o różnych właścicielach i cyklach życia. Rozszerzanie wspólnej klasy
bazowej o mode, state, health, metrics i konfigurację ukryłoby te różnice oraz
utworzyło zależności między niezależnymi maszynami stanów.

System musi pozostać deterministyczny, testowalny bez sprzętu i możliwy do
rozwijania dla wielu node na 1–16 liniach RS485.

## Decision

Przyjmujemy definicje `Mode`, `State`, `Health`, `Command`, `Result`, `Event`,
`Metric` i `Snapshot` opisane w
[DOMAIN-LANGUAGE.md](../architecture/DOMAIN-LANGUAGE.md).

Definicje obowiązują w całym produkcie, lecz konkretne typy należą do bounded
context i mają jednego właściciela. Nie powstanie globalny `SystemState` ani
klasy bazowe `StatefulObject`, `HealthyObject` lub `ObservableObject`.

Wspólną strukturę reprezentujemy przez kompozycję małych typów wartości.
Interfejsy zachowania powstają na granicach wymiennych implementacji, takich jak
zegar, link do node i repozytorium Capture.

Command opisuje intencję, Result wynik jej obsługi, a Event fakt z przeszłości.
Health jest wyliczany z faktów i metrics niezależnie od lifecycle. Snapshot jest
tylko niemutowalnym widokiem bieżącego stanu.

## Consequences

- Node, delivery, recording, channel i update mają oddzielne typy state.
- Nie każdy obiekt ma mode, health, metrics ani konfigurację.
- Wiadomości mogą zawierać wspólny envelope, ale ich payload pozostaje typowany
  według kontekstu.
- Refaktory będą wykonywane pionowymi fragmentami, bez masowej zmiany nazw.
- Każdy nowy state i cursor wymaga jawnego właściciela oraz testowanego
  niezmiennika.
- UI i adaptery nie otrzymują prawa do bezpośredniej zmiany stanu domenowego.

## Rejected alternatives

- **Jedna bazowa klasa komponentu:** wymusza nieistniejące podobieństwo i rośnie
  wraz z każdym nowym podsystemem.
- **Jeden globalny enum state:** tworzy kombinacje stanów różnych maszyn i nie
  reprezentuje pracy równoległej.
- **Event jako uniwersalna wiadomość:** zaciera różnicę między żądaniem, wynikiem,
  faktem domenowym, telemetrią i danymi jakościowymi.
- **Natychmiastowa zmiana wszystkich nazw:** duży zakres utrudnia sprawdzenie
  zachowania i zwiększa ryzyko regresji.
