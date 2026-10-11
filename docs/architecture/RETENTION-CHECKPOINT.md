# Retention checkpoint

- Status: osobne checkpointy i crash-safe cleanup Capture oraz Archive
  zaimplementowane; integracja z compactor/publisher pozostaje otwarta
- Ostatnia aktualizacja: 2026-10-09

## Polityka domyślna

- Capture może zostać usunięty 48 godzin po potwierdzonej replikacji
  zweryfikowanego Archive.
- Lokalny Archive może zostać usunięty 30 dni po potwierdzeniu, ale dopiero po
  trwałym zakończeniu cleanupu wszystkich zależnych segmentów Capture.
- Brak potwierdzenia serwera oznacza brak deadline'u usunięcia, niezależnie od
  wieku danych.
- Produkt Archive nie będzie usuwany, dopóki wszystkie zależne segmenty Capture
  nie mają trwale zapisanego zakończenia cleanupu.

Wartości są konfigurowalne przez `RetentionPolicy`. Deadline'y zostają obliczone
i utrwalone w momencie przyjęcia acknowledgement, dzięki czemu późniejsza
zmiana konfiguracji nie skraca po cichu retencji istniejących danych.

## Maszyna stanu Capture

```text
sealed and validated Capture
  -> exact coverage by verified Archive products
  -> every contributing product replicated and acknowledged
  -> CaptureCoverageProof persisted
  -> capture deadline elapsed
  -> Capture deleted
```

Checkpoint wiąże stan z hardware ID, boot epoch, ordinalem okna i dokładnym
zakresem cursorów. Przechowuje wszystkie produkty Archive użyte w dowodzie,
każdy jako stabilny product ID i SHA-256. Dzięki temu okno Capture przecinające
granicę godziny może bezpiecznie zależeć od dwóch produktów. Dowód wymaga
dokładnego pokrycia bez luk i nakładania zakresów.

Przejście do `deleted_at` jest idempotentne dla identycznego zdarzenia. Inna
treść istniejącego checkpointu, niepełny dowód, regresja względem deadline'u,
overflow czasu albo uszkodzony plik są błędem fail-closed.

## Trwałość i czas

Checkpoint `SSCOVCP2` ma wersjonowany format binarny, ograniczenie 32 produktów
i CRC32C. Aktualizacja zapisuje plik
tymczasowy, wykonuje `sync_all`, atomowy rename i synchronizację katalogu. Stan
w RAM zmienia się dopiero po trwałej publikacji nowej wersji.

Odczyt waliduje nie tylko CRC, ale również łańcuch czasu:

```text
fully_replicated_at < delete_after <= deleted_at
```

Decyzja o usunięciu przyjmuje `TrustedUnixSeconds`. Dopóki Time Service nie
potwierdzi wiarygodnego UTC, cleanup nie powinien konstruować tej wartości.
Regresja czasu opóźnia usunięcie; nie może go przyspieszyć. Overflow timestampu
blokuje utworzenie deadline'u.

## Granica odpowiedzialności

`ArchiveCoverageIndex` opisuje wejścia produktu, a `prove_capture_coverage`
tworzy dowód dopiero po pełnym pokryciu i potwierdzonej replikacji wszystkich
użytych produktów. `CaptureCoverageCheckpointStore` utrwala ten dowód i
odpowiada, czy usunięcie jest już dozwolone. `cleanup_capture_segment` otwiera
checkpoint, wyprowadza obie ścieżki z jego ordinalu, ponownie sprawdza tożsamość
segmentu, usuwa najpierw WAL, potem manifest, synchronizuje katalog i dopiero
wtedy zapisuje `deleted_at`.
Brak pliku po crashu jest uzgadniany idempotentnie. WAL bez manifestu jest
odrzucany, ponieważ taka kolejność nie może powstać w wykonawcy cleanup.

Wywołujący musi przekazać ochronę strumienia: ordinal aktywnego okna albo jawne
potwierdzenie, że boot stream jest zamknięty. Cleanup nigdy nie odłącza
aktywnego okna writera. Fault injection obejmuje crash po unlink WAL oraz po
sync katalogu przed aktualizacją checkpointu.

Do zamknięcia pozostają:

1. wpięcie tworzenia indeksu, dowodu i checkpointów do przyszłego procesu
   compactora/uploadera;
2. alarmy pojemności oraz polityka zatrzymania, gdy brak kopii nie pozwala
   zwolnić miejsca.

## Maszyna stanu Archive

`ArchiveRetentionCheckpointStore` tworzy osobny checkpoint po zweryfikowaniu
produktu i przyjęciu potwierdzenia replikacji. Utrwala `product_id`, SHA-256,
czasy verification/acknowledgement, niezmienny deadline oraz pełny, unikalny
zbiór `SegmentKey` wejściowych Capture.

```text
verified Archive + server acknowledgement
  -> archive deadline persisted
  -> every dependent Capture checkpoint has deleted_at
  -> archive deadline elapsed
  -> Archive data deleted
  -> coverage index deleted
  -> Archive directory synchronized
  -> archive deleted_at persisted
```

Cleanup ponownie odczytuje i waliduje coverage index oraz wymaga dokładnie tego
samego zbioru zależności co checkpoint. Brak, duplikat lub obcy checkpoint
Capture blokuje operację. Crash po usunięciu danych albo po synchronizacji
katalogu jest uzgadniany idempotentnie przy kolejnym uruchomieniu. Obecność
artefaktów po zapisanym `deleted_at` jest traktowana jako konflikt fail-closed.

Starszy `RetentionCheckpointStore` pozostaje tymczasowo jako przetestowany zapis
historycznego modelu 1:1. Nie jest już źródłem decyzji cleanupu Capture ani
Archive i zostanie usunięty podczas integracji procesu compactora.
