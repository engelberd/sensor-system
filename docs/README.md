# Dokumentacja Sensor System

Ten plik jest punktem wejścia dla każdej osoby i każdego nowego okna
kontekstowego. Jeżeli dokumenty są sprzeczne, obowiązuje kolejność źródeł prawdy
podana niżej.

## Zacznij tutaj

1. [Architecture baseline](architecture/ARCHITECTURE-BASELINE.md) — zamrożone
   decyzje i lista rzeczy celowo jeszcze otwartych.
2. [Implementation roadmap](architecture/IMPLEMENTATION-ROADMAP.md) — obecny
   stan, brakujące elementy, kolejność prac i kryteria ukończenia etapów.
3. [Host v2 user adoption roadmap](architecture/HOST-V2-USER-ADOPTION-ROADMAP.md)
   — droga od działającego rdzenia do automatycznej pracy 24/7, `hostctl doctor`,
   dashboardu i zdalnej obsługi Raspberry Pi.
4. [Core readiness audit](architecture/CORE-READINESS-AUDIT.md) — krytyczne
   ryzyka, realne granice wydajności i mapa parametrów.
5. [Product invariants](architecture/PRODUCT-INVARIANTS.md) — wymagania, których
   implementacja nie może naruszyć.
6. [Measurement data contract](architecture/MEASUREMENT-DATA-CONTRACT.md) —
   znaczenie próbek, sekwencji, luk, czasu i okien danych.
7. [Archive and publication policy](architecture/ARCHIVE-AND-PUBLICATION-POLICY.md)
   — godzinne produkty, watermark, wysyłka i retencja.
8. [RS485 communication policy](architecture/RS485-COMMUNICATION-POLICY.md) —
   zasady jednej magistrali z wieloma node.
9. [Host v2 README](../host_v2/README.md) — realne drzewo kodu i polecenia
   weryfikacji.

Logiczny strumień node łączący zwarty ring próbek z dokładnymi lukami
opisuje [MEASUREMENT-JOURNAL.md](architecture/MEASUREMENT-JOURNAL.md).
Fizyczny format trwałych rekordów hosta i reguły recovery opisuje
[CAPTURE-WAL-V1.md](architecture/CAPTURE-WAL-V1.md).
Warunki bezpiecznego usuwania Capture i Archive opisuje
[RETENTION-CHECKPOINT.md](architecture/RETENTION-CHECKPOINT.md).
Granice timestampów `t1..t4`, model dryfu i zasady niepewności opisuje
[TIME-SERVICE.md](architecture/TIME-SERVICE.md).
Kompaktowy format surowych obserwacji czasu i jego recovery opisuje
[TIME-WAL-V1.md](architecture/TIME-WAL-V1.md).
Relację many-to-many pomiędzy zakresami Capture i produktami godzinnymi opisuje
[ARCHIVE-COVERAGE-INDEX.md](architecture/ARCHIVE-COVERAGE-INDEX.md).
Ograniczony handoff rekordów z kanałów do storage opisuje
[BOUNDED-PERSISTENCE-WRITER.md](architecture/BOUNDED-PERSISTENCE-WRITER.md).
Wersjonowany snapshot procesu dla `doctor`, systemd i dashboardu opisuje
[RUNTIME-STATUS-V1.md](architecture/RUNTIME-STATUS-V1.md).
Bezpieczne kontrole online/offline i jawny test głęboki opisuje
[HOST-DOCTOR-V2.md](architecture/HOST-DOCTOR-V2.md).
Kolejność recovery repozytorium, odczytu durable cursorów i startu workera
opisuje [CAPTURE-RUNTIME-STARTUP.md](architecture/CAPTURE-RUNTIME-STARTUP.md).
Zasady przekazywania sekwencjonowanych próbek i luk między rdzeniami opisuje
[SAMPLE-INGRESS.md](architecture/SAMPLE-INGRESS.md).
Właściciela stanów start/stop/fault na core 0 opisuje
[SENSOR-RUNTIME.md](architecture/SENSOR-RUNTIME.md), a granicę poleceń między
rdzeniami [CORE-OPERATION-MAILBOX.md](architecture/CORE-OPERATION-MAILBOX.md).
Połączenie tych elementów z journalem na core 1 opisuje
[NODE-V2-RUNTIME-COMPOSITION.md](architecture/NODE-V2-RUNTIME-COMPOSITION.md).
Mapowanie błędów FIFO i czasu na fakty measurement opisuje
[ACQUISITION-QUALITY-AND-DIAGNOSTICS.md](architecture/ACQUISITION-QUALITY-AND-DIAGNOSTICS.md).
Reguły bezpiecznego wydawania zakresów hostowi i cykl prepare/complete/abort
opisuje [DELIVERY-SESSION.md](architecture/DELIVERY-SESSION.md).
Adapter usługi protocol 5 i granicę zakończenia odpowiedzi opisuje
[MEASUREMENT-HANDLER.md](architecture/MEASUREMENT-HANDLER.md).
Właściciela framingu, bounded response queue i half-duplex TX opisuje
[PROTOCOL-V3-TRANSPORT-ENDPOINT.md](architecture/PROTOCOL-V3-TRANSPORT-ENDPOINT.md).

Obowiązujący format framingu znajduje się w
[FRAMING-V3-SPEC.md](protocol/FRAMING-V3-SPEC.md), a wybór uzasadnia
[ADR-0008](adr/0008-framing-v3.md). Analiza porównawcza pozostaje w
[FRAMING-V3-DESIGN-STUDY.md](protocol/FRAMING-V3-DESIGN-STUDY.md).
Źródłowe numerowanie próbek oraz oddzielenie zwartego sample ring od bounded
rejestru luk zamraża
[ADR-0009](adr/0009-source-sequence-and-gap-ledger.md).
Sposób przeplatania danych, synchronizacji i poleceń na jednej magistrali opisuje
[COMMUNICATION-MODEL.md](protocol/COMMUNICATION-MODEL.md).
Inwentaryzacja starych poleceń, znalezione problemy z niezawodnością i katalog
potrzeb v2 są w
[LEGACY-TRANSACTION-AUDIT.md](protocol/LEGACY-TRANSACTION-AUDIT.md).
Aktualny kontrakt cursorów i transferu próbek opisuje
[MEASUREMENT-SERVICE.md](protocol/MEASUREMENT-SERVICE.md).
Dokładny układ bajtów znajduje się w
[MEASUREMENT-WIRE-V5.md](protocol/MEASUREMENT-WIRE-V5.md), a bezpieczną migrację
z działającego protocol 4 opisuje
[MIGRATION-V4-V5.md](protocol/MIGRATION-V4-V5.md).
Minimalny payload synchronizacji zegara opisuje
[TIME-WIRE-V5.md](protocol/TIME-WIRE-V5.md).
Pełną tabelę ODR, format liczb całkowitych i znalezione ograniczenia starego
firmware opisuje
[DATA-TYPE-AND-RATE-AUDIT.md](protocol/DATA-TYPE-AND-RATE-AUDIT.md).

Szczegółowe słownictwo znajduje się w
[DOMAIN-LANGUAGE.md](architecture/DOMAIN-LANGUAGE.md), docelowe odpowiedzialności
modułów w [HOST-V2-STRUCTURE.md](architecture/HOST-V2-STRUCTURE.md), a model
skalowania w [HOST-SCALING.md](architecture/HOST-SCALING.md).

## Hierarchia decyzji

W razie sprzeczności obowiązuje:

1. `AGENTS.md` i aktualne wymagania użytkownika;
2. zaakceptowane ADR-y, z uwzględnieniem późniejszych zmian;
3. `ARCHITECTURE-BASELINE.md` i kontrakty produktowe;
4. `IMPLEMENTATION-ROADMAP.md`;
5. pozostałe dokumenty architektury;
6. `docs/legacy`, które opisuje działający stary system, a nie projekt v2.

Nie wyprowadzamy nowych wymagań z nazw istniejących klas starego hosta.

## Status katalogów

- `host_v2/` — nowa implementacja; Rust na drodze krytycznej, Python offline.
- `host/` — działający host Python; referencja zachowania i źródło testów
  porównawczych, ale nie wzorzec zależności dla v2.
- `node/` — działający firmware C++17; będzie zmieniany etapami do protokołu v5.
- `docs/architecture/` — obowiązujący projekt produktu i implementacji v2.
- `docs/adr/` — historia decyzji i ich uzasadnienie.
- `docs/legacy/` — dokumentacja starego hosta, formatów i wdrożeń. Nie jest
  specyfikacją `host_v2`.

Diagramy UML zostały usunięte. W tym projekcie źródłem prawdy są kontrakty,
krótkie przepływy tekstowe, tabele stanów i wykonywalne testy.

## Zasada aktualizacji dokumentacji

Zmiana kodu aktualizuje roadmapę tylko wtedy, gdy zmienia status etapu, kontrakt
lub otwartą decyzję. Zmiana zamrożonego wymagania wymaga ADR. Materiał historyczny
nie może wrócić do katalogu głównego jako aktualna specyfikacja.
