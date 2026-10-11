# ADR 0006: Minimalny strumień pomiarowy i ruchome okna plików

- Status: accepted
- Date: 2026-09-28

## Context

Pliki wyrównane do pełnych granic UTC sugerowały, że próbka została pobrana
dokładnie na granicy minuty lub sekundy. Granica pliku jest organizacją storage,
a nie faktem pomiarowym. Powtarzanie numeru sekwencji przy każdej próbce także
nie jest konieczne, jeżeli ciągłość jest opisana jednoznacznie segmentami.

Wire batches i durable commit służą niezawodnemu transportowi, lecz nie mają
znaczenia dla późniejszej analizy wartości sensora.

## Decision

Obowiązuje [MEASUREMENT-DATA-CONTRACT.md](../architecture/MEASUREMENT-DATA-CONTRACT.md).

Źródłowy strumień zawiera surowe XYZ i logiczną tożsamość próbki opartą na
`hardware_id`, `boot_epoch` i `sample_seq`. Fizyczny storage może zapisać
`first_sample_seq` raz dla ciągłego segmentu i wyprowadzać kolejne numery z
indeksu.

Luka kończy segment i jest zapisywana jawnie. Capture i Archive nie interpolują
brakujących wartości.

Domyślne okno pliku ma docelowo 600 sekund, ale jest konfigurowalne. Rozpoczyna
się od rzeczywistej pierwszej próbki i nie jest wyrównywane do granicy UTC.
Metadata, a nie nazwa pliku, zawierają oszacowany czas pierwszej i ostatniej
próbki oraz nominalny/effective ODR.

Pierwsze profile output ODR to 250, 125 i 62.5 Hz po decymacji node. Operator
wybiera profil, a admission control sprawdza, czy mieści się w budżecie channel.

Wire batch, retry i commit pozostają wewnętrznym stanem transportu i recovery.
Nie są częścią analitycznego kontraktu danych.

## Consequences

- Pliki różnych sensorów nie muszą mieć identycznych granic.
- Analiza łączy dane według rzeczywistych timestampów/anchorów, sekwencji i luk.
- Krótkie pliki są legalne; nie dopełniamy ich syntetycznymi próbkami.
- Daily Archive może w przyszłości ciąć dane według zrekonstruowanego czasu
  próbek, niezależnie od granic Capture.
- Dokładny layout i kompresja pozostają decyzją adaptera storage popartą pomiarem.

## Superseded decision

Ten ADR zastępuje wyłącznie wcześniejszą decyzję o wyrównywaniu Capture do
dziesięciominutowych granic UTC. Rozdzielenie Capture, Archive i Derived z ADR
0001 pozostaje obowiązujące.
