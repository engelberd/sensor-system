# ADR-0010: uporządkowane quality events i diagnostyka poza hot path

- Status: accepted
- Date: 2026-09-30

## Kontekst

Legacy `AcquisitionEngine` przekazuje dalej poprawne próbki, lecz część strat
opisuje wyłącznie przez `dropped_samples` i pola diagnostyczne sterownika.
Odczyt późniejszego snapshotu licznika nie mówi, pomiędzy którymi próbkami
wystąpiła strata. Nie może więc być podstawą rekonstrukcji strumienia.

W obecnej implementacji zapis fault snapshot może również wykonać erase/program
flash bezpośrednio z `DiagnosticStore::record_fault`, wywołanego wewnątrz sekcji
krytycznej akwizycji. Taki zapis ma nieograniczony dla hot path czas wykonania i
może sam spowodować dalsze przepełnienie FIFO.

Nie każda strata przed decymacją daje dokładną liczbę brakujących próbek
wyjściowych. Sticky FIFO overrun, niejednoznacznie przerwany transfer SPI albo
reset filtra mogą potwierdzić utratę ciągłości bez wiarygodnego
`missing_count`. Zamiana tego faktu na arbitralny gap długości jeden byłaby
fałszowaniem danych.

## Decyzja

### 1. Trzy oddzielne ścieżki

Firmware rozdziela:

1. **Measurement stream** — uporządkowane `Sample`, `ExactGap` i
   `ContinuityBreak`; jest częścią kontraktu danych i nie może być best-effort.
2. **Production telemetry** — stałokosztowe saturujące counters, gauges i mały
   fault latch potrzebny do health/recovery; nie służy do odtwarzania danych.
3. **Service diagnostics** — bounded historia zdarzeń, rozszerzone snapshoty i
   odczyty na żądanie; ma osobny budżet czasu, pamięci i łącza.

Zmiana profilu diagnostycznego nie zmienia measurement stream ani polityki
retencji próbek.

### 2. Dwa rodzaje przerwania ciągłości

`ExactGap` oznacza, że producent zna dokładny zakres brakujących logicznych
próbek wyjściowych:

```text
ExactGap(first_missing, missing_count > 0, reason)
```

`ContinuityBreak` oznacza, że utrata ciągłości jest pewna, ale jej rozmiar nie
jest wiarygodnie znany:

```text
ContinuityBreak(at_cursor, reason, timing evidence if available)
```

Break zajmuje jeden element logicznego cursor space, aby mógł podlegać tej
samej retencji, retransmisji, durable commit i release co sample/gap. Ten jeden
element jest znacznikiem, a nie twierdzeniem o utracie jednej próbki. Następna
próbka rozpoczyna nowy continuous segment.

ADR-0009 pozostaje obowiązujący dla próbek i dokładnych luk. Niniejsza decyzja
rozszerza jego dwuwariantowy strumień o trzeci wariant dla nieznanego rozmiaru.

### 3. Klasyfikacja strat

- utrata gotowego `SampleRecord` na bounded ingress daje `ExactGap`, ponieważ
  jego sequence został już przydzielony;
- jawnie policzona utrata próbek wyjściowych może dać `ExactGap`;
- utrata surowych wejść filtra, FIFO overrun o nieznanym rozmiarze, niepewny
  transfer SPI, niewiarygodny timing i reset potoku dają `ContinuityBreak`, jeśli
  potok nie potrafi dowieść dokładnej liczby brakujących wyjść;
- counter lub tekst logu nigdy nie zastępuje żadnego z tych zdarzeń.

### 4. Koszt trybów pracy

W `Production` hot path może wykonać tylko ograniczoną pracę w RAM: aktualizację
metryki, ustawienie fault latch i publikację bounded quality event. Zabronione
są `printf`, formatowanie tekstu, flash erase/program, nieograniczone skanowanie
i oczekiwanie na transport.

W `Diagnostic` dodatkowe zdarzenia trafiają do bounded bufora i są wysyłane
wyłącznie z przyznanego budżetu. Pełny bufor zwiększa jawny counter; nie blokuje
measurement stream.

W `Maintenance`, po zatrzymaniu akwizycji, wolno wykonywać self-test, pełne
dumpy, zapis trwałej diagnostyki i inne operacje o długim czasie wykonania.
Krytyczny fault może zatrzymać akwizycję i przejść do bezpiecznego stanu przed
trwałym zapisem.

### 5. Trwała diagnostyka

Rejestracja faultu tworzy w RAM immutable snapshot/pending work. Osobny
serwisant zapisuje go do flash poza sekcją krytyczną sensora i tylko w
dozwolonym oknie wykonania. Kod rejestrujący fault nie wywołuje synchronicznie
flash I/O.

## Niezmienniki

```text
quality event ma jednoznaczną pozycję względem próbek
ExactGap nigdy nie zawiera estymowanego missing_count
ContinuityBreak nigdy nie udaje pojedynczej brakującej próbki
telemetry/log nie jest źródłem sequence ani recovery cursora
profil Diagnostic nie może wprowadzić backpressure do measurement stream
flash I/O nie działa w sekcji krytycznej akwizycji
każdy bufor diagnostyczny ma limit i counter utraty
```

## Konsekwencje

- Measurement Journal i wire protocol 5 wymagają jawnego wyniku
  `CONTINUITY_BREAK` przed integracją z legacy acquisition.
- `AcquisitionEngine` zostanie rozdzielony na produkcję uporządkowanych faktów i
  niezależną obserwację serwisową.
- Dotychczasowe counters pozostają użytecznymi metrics, ale tracą rolę dowodu
  jakości danych.
- Synchroniczny zapis persistent diagnostic z hot path zostanie zastąpiony
  bounded deferred work.
- Host i Capture zachowają break jako quality event; analiza może rozdzielić
  segmenty bez wymyślania liczby brakujących próbek.

## Odrzucone alternatywy

- **Różnica liczników jako gap:** nie zachowuje kolejności i może połączyć kilka
  niezależnych zdarzeń.
- **Każda strata jako gap długości jeden:** podaje fałszywą liczbę braków.
- **Pełne logowanie w Production:** zmienia timing ścieżki, którą ma obserwować.
- **Nieograniczony ring diagnostyczny:** przesuwa awarię z diagnostyki do całego
  firmware.
- **Zapis flash bezpośrednio przy fault:** blokuje sensor i może wywołać kolejną
  stratę.
