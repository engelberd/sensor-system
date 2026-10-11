# Archive coverage index

- Status: indeks, trwały codec, dowód pełnego pokrycia i checkpoint Capture v2
  zaimplementowane
- Ostatnia aktualizacja: 2026-10-09

## Cel

Granice około dziesięciominutowego Capture i godzinnego Archive są niezależne.
Jeden Archive może czytać fragmenty wielu segmentów i boot epoch, a jeden
segment Capture może zasilać dwie sąsiednie godziny. Relacja jest many-to-many i
nie może zostać zastąpiona pojedynczym `ArchiveIdentity` w checkpoincie segmentu.

`ArchiveCoverageIndex` należy do jednego niezmiennego produktu Archive. Zawiera:

- `product_id` oraz SHA-256 zweryfikowanego Archive;
- fizyczne `hardware_id` sensora;
- godzinę UTC albo jawny zakres `time_unresolved`;
- numer rewizji i opcjonalne `supersedes_product_id`;
- ograniczoną listę dokładnych podzakresów źródłowych Capture.

Każdy podzakres wskazuje pełny `SegmentKey` oraz przedział
`[first_cursor, end_exclusive)`. Musi mieścić się w segmencie. Nakładające się
wejścia tego samego produktu, inny sensor, zerowy boot epoch i niepełne zakresy
są odrzucane.

## Trwałość

Indeks ma wersjonowany format binarny, stały nagłówek, ograniczenie 1024
podzakresów i CRC32C całego dokumentu. Publikacja wykonuje zapis tymczasowy,
`sync_all`, atomowy rename i synchronizację katalogu. Ponowna publikacja jest
idempotentna wyłącznie dla identycznej treści; konflikt istniejącego indeksu
zatrzymuje operację.

## Dowód usunięcia Capture

`prove_capture_coverage` przyjmuje konkretny segment oraz aktywne produkty z
czasami walidacji i acknowledgement serwera. Następnie:

1. odrzuca acknowledgement wcześniejsze od walidacji;
2. odrzuca duplikat produktu oraz jednoczesne użycie rewizji i produktu, który
   został przez nią zastąpiony;
3. wybiera podzakresy odnoszące się do segmentu;
4. sortuje je po cursorze i wymaga dokładnego pokrycia bez luki i overlapu;
5. zwraca listę produktów oraz najpóźniejsze acknowledgement użyte w dowodzie.

Ta ostatnia chwila jest początkiem grace period Capture. Produkt Archive, który
nie dostarcza danych do danego segmentu, nie może sztucznie przesunąć jego
deadline'u.

## Integracja z retencją Capture

`CaptureCoverageCheckpointStore` utrwala cały `CaptureCoverageProof`: dokładną
tożsamość segmentu, wszystkie produkty Archive potrzebne do jego pokrycia,
najpóźniejsze potwierdzenie replikacji oraz wyliczony termin usunięcia. Format
`SSCOVCP2` ma ograniczenie 32 produktów, CRC32C i atomową publikację. Ponowne
utworzenie jest idempotentne tylko dla identycznego dowodu i polityki.

Cleanup Capture korzysta wyłącznie z tego checkpointu. Po terminie ponownie
sprawdza tożsamość WAL i manifestu, chroni aktywne okno writera, usuwa WAL,
manifest, synchronizuje katalog, a następnie zapisuje `deleted_at`. Brak lub
uszkodzenie dowodu blokuje usunięcie fail-closed.

Cleanup Archive będzie miał osobny checkpoint produktu; nie należy łączyć
retencji jednego produktu z retencją jego wielu źródeł Capture.
