# Ciągłość okien Capture

- Status: obowiązujący kontrakt logiczny
- Ostatnia aktualizacja: 2026-11-02

## Gwarancja

Podział na pliki nie zmienia strumienia pomiarowego. Dla jednego
`(hardware_id, boot_epoch)` manifesty tworzą uporządkowaną partycję cursor space:

```text
window 17: [0, 75000)
window 18: [75000, 150032)
window 19: [150032, 224991)
```

`end_exclusive` jednego okna musi być równy `first_cursor` następnego. Nazwa
pliku, UTC i docelowe 600 sekund nie uczestniczą w tym dowodzie.

Cursor space zawiera trzy typy uporządkowanych rekordów:

- próbki XYZ;
- dokładny `Gap`, który opisuje brakujące sample sequence;
- `ContinuityBreak`, gdy znana jest utrata ciągłości bez wiarygodnej liczby
  brakujących próbek.

Dlatego „ciągły pomiar” oznacza ciągły i kompletny ledger źródłowych faktów. Po
złączeniu plików analiza dostaje wszystkie istniejące próbki oraz jawny opis
każdego miejsca, w którym próbki nie istnieją. Recorder nie interpoluje danych.

## Rotacja

Wspólna polityka WAL/CSV wyznacza granice kalendarzowe w strefie instalacji.
Transferowy chunk może zostać podzielony między próbkami: pierwsza próbka
na granicy lub po niej trafia do następnego okna. Zmiana `config_revision`
zamyka poprzednie okno przed pierwszą próbką z nowymi ustawieniami.
[ADR-0013](../adr/0013-shared-capture-calendar.md) opisuje zamrożenie mapowania
czasu, recovery fragmentów i codzienne katalogi CSV.

Okno przechowuje osobno `first_sample_cursor`; gap lub continuity break mogą
poprzedzać pierwszą rzeczywistą próbkę. Czas okna zaczyna się od timing evidence
tej próbki. Plik nie udaje, że quality event posiada timestamp próbki.

## Kolejność trwałości

Adapter Capture wykonuje operację w tej kolejności:

1. `CaptureWindowLedger` przygotowuje plan i sprawdza hardware, boot epoch oraz
   dokładny następny cursor.
2. Adapter dopisuje rekord lub fragment chunku do bieżącego pliku/WAL.
3. Durable barrier potwierdza rekord zgodnie z polityką storage.
4. Adapter zatwierdza plan ledgera; dopiero wtedy może otrzymać manifest okna.
5. Manifest jest zapisywany i publikowany atomowo, po czym następny rekord
   trafia do kolejnego ordinalu.
6. `DurableReceipt` może pozwolić hostowi zwolnić dane w node.

Jeżeli krok 2 lub 3 zawiedzie, plan jest porzucany, cursor pozostaje bez zmian i
ten sam rekord może zostać zapisany ponownie. Jeśli proces padnie po durable
append, recovery odtwarza ledger z rekordów, zamiast ufać stanowi w RAM.

## Walidacja łączenia

`CaptureWindowManifest::relation_to` wymaga tego samego hardware, kolejnego
ordinalu i dokładnie przylegających cursorów. Zwraca osobną granicę dla zmiany
boot epoch. Ukryty zakres, overlap albo pomieszanie sensorów są błędem i nie
mogą zostać naprawione zmianą nazwy pliku.

## Stan implementacji

Warstwa [Capture WAL v2](CAPTURE-WAL-V2.md) implementuje packed int24, CRC32C,
trwały marker seal, odtwarzanie ledgera, fizyczny segment per okno i atomowy
manifest. Bezpiecznie obcinany jest tylko fizycznie niepełny ogon; pełna
korupcja CRC zatrzymuje recovery fail-closed. Read-only reader i niezależny
validator odtwarzają ledger przed przekazaniem segmentu do Archive. Pozostają
checkpoint retencji, bounded handoff oraz benchmark group commit. Te decyzje
nie mogą zmienić opisanej semantyki.
