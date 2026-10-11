# Capture WAL v1

- Status: warstwa rekordu, recovery, segmentacja, atomowe manifesty, reader i
  checkpoint core, bounded handoff, threaded executor oraz bezpieczny batch
  jednego WAL, group commit, typed profile, coverage index/checkpoint i cleanup
  Capture zaimplementowane; checkpoint i cleanup Archive pozostają otwarte
- Ostatnia aktualizacja: 2026-10-09

## Odpowiedzialność

`CaptureWalSink` jest trwałym adapterem dla jednego
`(hardware_id, boot_epoch)`. Przyjmuje `SampleChunk`, `Gap` i
`ContinuityBreak`, sprawdza ich kolejność przez `CaptureWindowLedger`, dopisuje
rekord i wykonuje `sync_data`. Pojedyncze `persist_*` korzysta z batcha jednego
rekordu. `CaptureWalBatch` może przygotować kilka rekordów w pamięci, zapisać je
jednym ciągiem i wykonać jedną barierę. Stan durable writera zmienia się dopiero
po jej sukcesie. Dopiero ten sukces może spowodować `DurableReceipt` oraz
późniejsze zwolnienie danych w node.

`CaptureSegmentStream` utrzymuje jeden fizyczny WAL na jedno logiczne okno.
Publikuje wersjonowany manifest przez zapis pliku tymczasowego, `sync_all`,
atomowy rename i synchronizację katalogu. Brakujący manifest po awarii jest
odtwarzany wyłącznie ze zweryfikowanego, zapieczętowanego WAL.

`CaptureRepository` jest ograniczonym routerem wielu skonfigurowanych
strumieni. Nie tworzy segmentu dla nieznanego hardware ID albo nowego boot
epoch. Zmiana strumienia wymaga jawnego recovery/reconciliation, dzięki czemu
dane dwóch sensorów nie mogą przypadkowo trafić do jednego właściciela cursora.
Udostępnia niekopiujący `CaptureRepositoryBatch`, zawsze związany z jednym
identity, boot epoch i fizycznym WAL-em.

`CaptureWalReader` jest ścieżką tylko do odczytu. Zwraca po jednym ograniczonym
rekordzie i nigdy nie obcina ani nie naprawia źródła. `validate_capture_segment`
odtwarza niezależny `CaptureWindowLedger`, wymaga zakończonego okna i porównuje
wyliczony manifest ze wszystkimi polami opublikowanego manifestu. Archive może
więc rozpocząć pracę wyłącznie na pełnym, semantycznie spójnym segmencie.

## Układ pliku

Plik zaczyna się 64-bajtowym nagłówkiem zawierającym:

- magic i wersję formatu;
- hardware ID i boot epoch;
- początkowy cursor oraz ordinal okna;
- docelową długość okna;
- CRC32C nagłówka.

Każdy rekord ma długość, typ, wersję rekordu, pola zarezerwowane, payload i
CRC32C. Próbki XYZ zachowują źródłową reprezentację signed int24, czyli 9 bajtów
na próbkę przed narzutem rekordu. Rekordy obejmują:

- sample chunk wraz z `retained_from`, `produced`, config revision i timing
  evidence;
- dokładny gap i jego przyczynę;
- continuity break i jego przyczynę;
- trwały marker jawnego zamknięcia okna.

Rozmiar pojedynczego rekordu jest ograniczony. Recovery alokuje pamięć tylko dla
jednego rekordu i stanu ledgera, więc RAM nie rośnie z długością nagrania.

## Recovery i granica zaufania

Recovery sprawdza nagłówek, wersje, zarezerwowane pola, długość, CRC, kontrakt
rekordu oraz dokładny następny cursor. Następnie odtwarza ledger w tej samej
kolejności, w której działa writer.

- Fizycznie niepełny końcowy rekord jest niezakończonym appendem. Plik zostaje
  obcięty do ostatniej w pełni zweryfikowanej granicy i zsynchronizowany.
- Pełny rekord z błędnym CRC zatrzymuje start fail-closed, także gdy jest
  ostatni. Mógł wcześniej otrzymać `DurableReceipt`, więc automatyczne usunięcie
  mogłoby ukryć utratę danych już zwolnionych przez node.
- Błąd append albo sync zatruwa bieżącą instancję writera. Nie wolno ponawiać
  zapisu w tym samym obiekcie; trzeba otworzyć plik ponownie i rozstrzygnąć stan
  przez recovery. Zapobiega to podwójnemu dopisaniu wyniku niepewnego I/O.
- Rekord zsynchronizowany przed awarią, lecz jeszcze niezatwierdzony w stanie
  RAM, zostaje poprawnie odtworzony po restarcie.
- Batch nie może przejść przez granicę okna. Rekord, który pieczętuje okno, jest
  ostatnim rekordem grupy; następna grupa rozpoczyna się po rotacji segmentu.
- Wszystkie obliczenia, które mogą zawieść, kończą się przed zapisem. Po udanym
  `sync_data` writer wykonuje wyłącznie nieomylne podstawienie przygotowanego
  ledgera, liczników i cursora.

## Zweryfikowane przypadki

Testy Rust obejmują signed int24 na granicach, próbki/luki/continuity break,
jawny seal, ucięty ogon, crash po sync przed commit w RAM, błędny CRC wewnątrz i
na końcu, niezgodność identity/policy, read-only niepełnego źródła, rotację
plików, crash przed publikacją manifestu, konflikt i semantyczne kłamstwo
manifestu, batch bez przedwczesnego durable cursora, odrzucenie batcha starego
lub przekraczającego okno, błąd bariery po zapisie całej grupy i recovery bez
duplikatu oraz dwa niezależne node w jednym repozytorium.

## Następny pion

Przed uznaniem całej ścieżki Capture za produkcyjną trzeba dodać:

1. checkpoint produktu, trwały publication outbox i cleanup Archive;
2. pomiar limitu rekordów i opóźnienia group commit na Raspberry Pi z M.2 SSD;
3. rozszerzony fault injection procesu podczas wszystkich granic rotacji i
   publikacji manifestu.

Te elementy nie zmieniają formatu próbki ani semantyki cursorów.
