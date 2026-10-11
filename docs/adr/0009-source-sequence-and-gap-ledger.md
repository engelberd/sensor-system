# ADR-0009: źródłowa sekwencja i osobny GapLedger

- Status: accepted
- Date: 2026-09-30

## Kontekst

`SampleJournal` początkowo nadawał numer dopiero podczas zapisu próbki na core
1. Docelowy runtime przenosi próbki z core 0 przez ograniczony mailbox. Jeżeli
mailbox albo wcześniejsza akwizycja utraci próbkę, kolejna próbka dostałaby
ciągły numer i host nie potrafiłby wykryć straty.

Strata może powstać także przed odczytem sensora, na przykład przez overflow
FIFO lub pierścienia DRDY. Mechanizm nie może więc zależeć od UART, ADXL355 ani
konkretnego kodowania XYZ.

Rozważono wspólny ring rekordów `Sample/Gap` oraz zwarty ring próbek z osobnym
rejestrem luk. Wspólny ring zwiększa rozmiar każdego częstego rekordu do
rozmiaru największego wariantu i utrudnia bezpośrednie kodowanie ciągłego
fragmentu próbek.

## Decyzja

1. Jeden `SourceSequencer` na strumień nadaje sequence na granicy akwizycji,
   przed każdym mailboxem i inną potencjalnie stratną granicą.
2. Logiczny strumień składa się z uporządkowanych zdarzeń `Sample(sequence)` lub
   `Gap(first_missing, missing_count, reason)`.
3. Mailbox core 0 → core 1 przenosi oba rodzaje zdarzeń. Producent, który nie
   może wstawić próbki, zachowuje lokalny pending gap. Nie może opublikować
   późniejszej próbki przed tym gapem.
4. `SampleJournal` zachowuje zwarty, stałopojemnościowy ring payloadów próbek.
   Osobny bounded `GapLedger` przechowuje rzadkie zakresy luk. Nad nimi działa
   jeden logiczny measurement journal i jeden cursor space.
5. Sąsiednie gapy o tej samej przyczynie są scalane. Gapy o różnych przyczynach
   pozostają oddzielne.
6. Pełny `GapLedger` nie może cicho odrzucić wpisu. Właściciel journala zwalnia
   miejsce przez kontrolowane odrzucenie najstarszego prefiksu retencji. Dla
   hosta taki prefiks staje się `RANGE_UNAVAILABLE`.
7. `PULL_SAMPLES(from)` zwraca dokładnie jedno z: ciągły chunk próbek zaczynający
   się w `from`, dokładny gap zaczynający się w `from`, `NO_DATA` albo wynik
   reconciliation. Chunk próbki kończy się przed następnym gapem.
8. Host przesuwa `received` po gapie dopiero po walidacji jego dokładnego
   zakresu, a `durable` dopiero po trwałym zapisie dowodu luki.

## Niezmienniki

```text
released <= retained_from <= produced

sample.sequence albo gap.first_missing == poprzedni cursor
gap.missing_count > 0
gap.end <= produced
brak nakładających się gapów
brak publikacji próbki ponad nieopublikowanym gapem
```

`produced` jest cursorem logicznego strumienia i obejmuje zarówno zachowane
próbki, jak i jawnie rozliczone braki. Liczba fizycznie przechowywanych próbek
nie jest już równa `produced - retained_from`.

## Konsekwencje

- zwykła próbka zachowuje obecny koszt RAM i szybki układ przyjazny kodowaniu;
- wyszukiwanie może skanować bounded liczbę rzadkich gapów, niezależnie od
  pojemności sample ring;
- Measurement Service otrzyma typowany wynik `GAP`;
- `RANGE_UNAVAILABLE` nadal opisuje prefiks, którego node nie potrafi już
  odtworzyć, a `GAP` opisuje dokładny zachowany brak wewnątrz strumienia;
- pojemność `GapLedger` oraz mailboxa pozostaje parametrem dobieranym pomiarem;
- ta sama logika sekwencji i gapów może obsłużyć inne źródło pomiarowe. Format
  payloadu próbki pozostaje odpowiedzialnością konkretnego sensora i service.
