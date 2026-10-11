# MeasurementJournal firmware v2

- Status: rdzeń zaimplementowany i przetestowany host-side
- Ostatnia aktualizacja: 2026-09-30
- Decyzja: [ADR-0009](../adr/0009-source-sequence-and-gap-ledger.md)

## Odpowiedzialność

`MeasurementJournal` przedstawia jeden logiczny strumień sequence nad trzema
ograniczonymi magazynami:

- zwartym ringiem częstych `SampleRecord`;
- małym `GapLedger` dla rzadkich, dokładnych zakresów braków.
- małym `ContinuityLedger` dla nieciągłości o nieznanym rozmiarze.

Źródło nadaje sequence przez `SourceSequencer` przed przekroczeniem stratnej
granicy. Journal nie wymyśla brakujących numerów i przyjmuje wyłącznie zdarzenie
zaczynające się dokładnie w aktualnym `produced`.

```text
SourceSequencer
    ├── Sample(sequence, payload)
    ├── Gap(first_missing, missing_count, reason)
    └── ContinuityBreak(at_sequence, reason)
                    ↓
            MeasurementJournal
```

## Wynik odczytu

Odczyt od `from` zwraca dokładnie jeden rodzaj wyniku:

| Wynik | Znaczenie |
| --- | --- |
| `Samples` | ciągły fragment próbek zaczynający się w `from` |
| `Gap` | zachowany dowód dokładnego braku zaczynający się w `from` |
| `ContinuityBreak` | dowód nieciągłości o nieznanym rozmiarze w `from` |
| `NoData` | `from == produced` |
| `RangeUnavailable` | `from < retained_from`; prefiks nie jest już odtwarzalny |
| `AheadOfProduced` | błędny cursor hosta |

Chunk próbek kończy się przed gapem, breakiem, zmianą konfiguracji albo zmianą
segmentu czasu. Odczyt zaczynający się wewnątrz gapu zwraca pozostały dokładny
zakres, co pozwala bezpiecznie wznowić sesję od trwałego cursora hosta.
`ContinuityBreak` zajmuje jedną pozycję cursora potrzebną do delivery/release,
ale nie twierdzi, że utracono dokładnie jedną fizyczną próbkę.

`Gap` i `RangeUnavailable` nie są synonimami. `Gap` jest zachowanym zdarzeniem
domenowym o dokładnej przyczynie. `RangeUnavailable` mówi, że node utracił
możliwość przedstawienia całego starszego prefiksu, na przykład przez
przepełnienie sample ring albo kontrolowany collapse metadanych gapów.

## Retencja i przepełnienie

Normalny append próbki nie skanuje całego sample ring. Gdy ring próbek jest
pełny, journal odrzuca najstarszy prefiks kończący się za najstarszą fizyczną
próbką. Prefiks może zawierać wcześniejsze gapy.

Gdy `GapLedger` jest pełny i nowego gapu nie można scalić, journal kontrolowanie
odrzuca prefiks kończący się za najstarszym zapisanym gapem. Dzięki temu:

- nowy gap nigdy nie znika po cichu;
- `retained_from` dokładnie wyznacza utracony prefiks;
- host otrzyma `RangeUnavailable` i zapisze utratę przed przesunięciem cursora.

`ContinuityLedger` stosuje tę samą fail-closed politykę: pełny ledger skraca
retencję do końca najstarszego breaka zamiast cicho odrzucić nowy dowód.

Stan rozróżnia:

- `discarded_samples` — liczbę fizycznych payloadów utraconych z retencji;
- `discarded_sequences` — długość całych odrzuconych prefiksów, razem z gapami;
- `retention_collapses` — liczbę kontrolowanych skróceń retencji.

## Delivery i release

Przygotowany widok próbki, gapu albo breaka ma jednorazową granicę
`prepared_through`.
`mark_delivered` przyjmuje wyłącznie dokładnie tę granicę. Każda mutacja
unieważnia stary widok.

Release usuwa próbki, gapy i breaki jako jeden prefiks logicznego strumienia.
Nie może wyprzedzić zakresu dostarczonego, poza już utraconym prefiksem do
`retained_from`. Powtórzony release pozostaje idempotentny.

## Koszt i granice

Pamięć jest przydzielana statycznie:

```text
sizeof(SampleRecord) * SampleCapacity
+ sizeof(GapRecord) * GapCapacity
+ sizeof(ContinuityBreakRecord) * ContinuityCapacity
```

Nie ma sterty, wyjątków ani wewnętrznych blokad. Zwykły append próbki jest O(1).
Odczyt i operacje na retencji skanują najwyżej stałą, skonfigurowaną liczbę
gapów/breaków oraz zwracany chunk, a nie całą pojemność próbek.

`DeliverySession` korzysta bezpośrednio z `MeasurementJournal` zarówno dla
chunków próbek, dokładnych gapów i breaków. Poprzedni ciągły `SampleJournal`
został usunięty; w firmware istnieje jeden mechanizm retencji pomiarów.

## Weryfikacja

Testy obejmują:

- `sample → gap → sample` w jednym cursor space;
- `sample → continuity break → sample` bez fałszywego missing count;
- recovery zaczynające się wewnątrz gapu;
- release przez próbki, gapy i breaki;
- overflow sample ring z gapem przed najstarszą próbką;
- pełny `GapLedger` i kontrolowany collapse prefiksu;
- odrzucenie nieciągłych zdarzeń;
- 50 000 deterministycznych zdarzeń porównanych z modelem referencyjnym;
- wykonanie testu pod UBSan.
