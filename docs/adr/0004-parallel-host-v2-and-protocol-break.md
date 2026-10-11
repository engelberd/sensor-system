# ADR 0004: Równoległy host v2 i świadome zerwanie protokołu

- Status: accepted
- Date: 2026-09-24

## Context

Obecny host jest działającą referencją, ale miesza protokół, logikę recordera,
narzędzia laboratoryjne, storage i UI. Stopniowe przenoszenie nowych abstrakcji
do tych samych modułów utrudniłoby rozpoznanie, czy zależności rzeczywiście
zostały usunięte.

Obecny system rozróżnia kilka wersji:

- produkt i firmware: `0.4.1`;
- protokół aplikacyjny node: `4`;
- framing RS485: `2`;
- protokół bootloader update: `2`.

Nazwa „host v2” opisuje generację implementacji hosta, a nie numer żadnego z
tych protokołów.

## Decision

Nowy host powstaje w osobnym pakiecie `host_v2`. Kod produkcyjny w `host_v2`
nie może importować starego pakietu `host`. Stary system pozostaje uruchamialną
referencją do czasu ukończenia bram integracyjnych i sprzętowych.

Nowy protokół jest projektowany wspólnie dla hosta i firmware node. Nie
implementujemy adaptera zgodności z aktualnym firmware. Pierwsza iteracja używa:

- host software `0.5.0-dev`;
- application protocol `5`;
- framing protocol `3`.

Kompatybilność wire wymaga dokładnej zgodności application protocol i framing
protocol. Wersja oprogramowania jest raportowana i audytowana, ale sama nie
decyduje o kompatybilności wire.

Bootloader update pozostaje osobnym, minimalnym protokołem. Jego następna wersja
będzie decyzją osobnego etapu po zaprojektowaniu własności linii i maintenance.

Firmware pozostaje w `node/` i będzie migrowany modułami. Nie powstaje trwały
drugi produkt `node_v2`; sterownik sensora, bootloader A/B i profile płytek są
ponownie używane za nowymi granicami.

## Consequences

- `host` i `host_v2` mogą działać równolegle podczas budowy i porównań.
- `host_v2` oraz stare firmware kończą handshake błędem wersji przed rozpoczęciem
  recording.
- Nie utrzymujemy dwóch ścieżek protokołu w nowym rdzeniu.
- Wspólne zachowanie przenosimy przez testy i kontrakty, nie przez import starej
  implementacji.
- Przed produkcyjnym przełączeniem jedna wersja produktu ponownie stanie się
  źródłem wersji hosta, firmware i artefaktów wydania.

## Removal gate for the legacy host

Stary host może zostać usunięty dopiero, gdy nowy system przejdzie:

1. kontraktowe testy tego samego wire format w Rust i C++;
2. symulację timeoutów, duplikatów, luk, restartów i błędów storage;
3. test wielu node na jednej linii;
4. test 1, 4, 8 i 16 równoległych linii;
5. HIL na docelowym sprzęcie i przewodzie;
6. test utraty zasilania oraz odzyskania trwałego cursora;
7. wymagany okres soak bez cichej utraty danych.

## Rejected alternatives

- **Refaktor starego hosta w miejscu:** zbyt trudno utrzymać działający punkt
  odniesienia i wykrywać importy cofające nową architekturę.
- **Adapter nowego hosta do starego firmware:** utrwaliłby niejasne granice
  obecnego protokołu i zwiększył zakres testów.
- **Nazwanie nowego framingu protocol v2:** koliduje z istniejącym framingiem
  o numerze `2` i utrudnia diagnostykę.
- **Osobny trwały katalog `node_v2`:** duplikuje sprawdzone elementy sprzętowe i
  tworzy ryzyko dwóch rozwijanych produktów firmware.
