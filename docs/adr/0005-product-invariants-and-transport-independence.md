# ADR 0005: Źródłowe dane, retencja i niezależność transportu

- Status: accepted
- Date: 2026-09-28

## Context

System ma służyć przede wszystkim do niezawodnego zbierania surowych pomiarów.
Przyszłe instalacje mogą używać wielu konwerterów, wielu node na channel albo
innego medium niż obecne RS485. Wymagania produktu nie mogą być ukryte w HDF5,
pyserial ani w klasie obsługującej konkretny konwerter.

## Decision

Obowiązują wymagania zapisane w
[PRODUCT-INVARIANTS.md](../architecture/PRODUCT-INVARIANTS.md).

Źródło prawdy zachowuje surowe wartości, sekwencję, konfigurację, quality events
oraz dowody czasu pozwalające ponownie obliczyć timestampy, ODR i niepewność.
Przeliczone wartości i analiza są odtwarzalnymi widokami lub produktami
pochodnymi.

Node przechowuje niepotwierdzone próbki w jednym bounded `SampleJournal`. Host
potwierdza je dopiero po trwałym zapisie. Krótkie awarie są obsługiwane przez
idempotentne ponowienie; przekroczenie policzonej retencji jest jawną utratą
danych.

`Channel` jest domeną własności, schedulera i budżetu komunikacji. `Rs485Line`
jest implementacją channel, a wieloportowy konwerter jednym `TransportDevice`
udostępniającym kilka channels. Domena transferu korzysta z `NodeLink`, dzięki
czemu inne medium może zostać dodane jako adapter po przejściu tego samego
kontraktu niezawodności.

Operations API jest lokalnym control plane hosta. Maintenance może zatrzymać
pomiary całej wybranej instalacji; niezawodne wznowienie, weryfikacja i rollback
są ważniejsze niż dostępność podczas serwisu.

## Consequences

- Capture i Archive nie mogą przechowywać wyłącznie przeliczonych timestampów
  lub wartości SI.
- ODR wyznaczone offline nie zastępuje źródłowych dowodów czasu.
- Wielkość journala i gwarantowany czas retencji wymagają pomiaru oraz jawnego
  budżetu dla każdego profilu ODR.
- Scheduler jest niezależny od biblioteki serial i obsługuje wiele node.
- Framing strumieniowy pozostaje w adapterze RS485; application messages nie
  zakładają UART.
- Zdalne sterowanie będzie osobnym zabezpieczonym adapterem, jeśli stanie się
  wymaganiem.
- System nie wymaga rolling update ani recording podczas Maintenance.
