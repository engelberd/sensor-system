# Plan wdrożenia timestampingu DRDY i rotacji HDF5 v5

Status: implementacja offline ukończona; rollout L1–L4 oczekuje  
Zasada nadrzędna: żadnych operacji na produkcyjnym czujniku bez osobnej zgody  
Strategia migracji: `legacy -> observe -> required`, host przed firmware

## 0. Stan implementacji

Ukończone:

- profile `custom_v2` i `legacy_eval` oraz domyślnie wyłączony feature gate;
- timestamp DRDY, ring SPSC, binder FIFO, recovery barrier i diagnostyka v8;
- propagacja czasu przez środek FIR i niezmienny Burst v2;
- `boot_epoch`, `GetCapabilities` i czterostemplowy `TimeSync`;
- parser v1/v2, modele affine sample/device/host/UTC i holdover;
- tryby hosta `legacy`, `observe`, `required`;
- routing wymagany według UTC akwizycji do półotwartych okien;
- HDF5 v5 z `.partial`, `complete`, kotwicami per pakiet/segment,
  surowymi obserwacjami synchronizacji, journalem ingest i recovery ogona;
- trwała kwarantanna HDF5 dla próbek `unsynced`, `ambiguous` i `late`, dzięki
  której tryb `required` nie przypisuje niepewnych próbek do zwykłego okna i
  nie blokuje commitów noda;
- trwały zapis przed `CommitReadUpTo`, deduplikacja retransmisji oraz atomowa
  publikacja pliku;
- test symulujący 50 ppm dryfu, jitter RTT, retransmisję i zmianę epoki.

Zweryfikowane offline:

- pełny zestaw testów Pythona;
- testy host-side C++;
- build OFF i ON dla slot A/B, direct i bootloader;
- build timestampingu ON dla profilu `legacy_eval`;
- brak flashowania, dostępu do portów i zmian usług systemowych.

Pozostałe bramki wymagają osobnej zgody i sprzętu: L1–L4, pomiar ISR/ringa przy
maksymalnym ODR, fault injection z realnym zanikiem zasilania oraz canary.

## 1. Cel upgrade'u

System ma:

- przypisywać próbki do plików 10-minutowych według czasu akwizycji;
- być odporny na jitter, opóźnienie i retransmisję RS485;
- estymować rzeczywisty okres próbkowania zamiast ufać idealnemu ODR;
- nie interpolować przez restart noda, zmianę konfiguracji ani utratę
  jednoznacznego powiązania DRDY/FIFO;
- umożliwiać czasowe wyrównanie danych z wielu nodów wraz z niepewnością;
- zachować możliwość bezpiecznego działania mieszanej floty starego i nowego
  firmware;
- zapewnić idempotentny zapis i odtwarzalność aktywnego HDF5 po awarii hosta;
- nigdy nie publikować niezweryfikowanego pliku jako kompletnego.

Nie próbujemy programowo zapewnić wspólnej fazy konwersji kilku ADXL355.
Timestamping pozwoli wyrównywać dane po fakcie. Prawdziwa synchronizacja fazowa
wymagałaby wspólnego sygnału synchronizacji/triggera i jest osobnym projektem.

## 2. Niezmienniki bezpieczeństwa

Każdy etap i test musi zachować następujące reguły:

1. Czas odbioru pakietu nigdy nie zastępuje czasu próbki.
2. Nie tworzymy timestampu przez zgadywanie po utracie powiązania DRDY/FIFO.
3. `sample_seq` jest monotoniczny wyłącznie wewnątrz jednej `boot_epoch`.
4. Nie interpolujemy przez zmianę `boot_epoch` ani `timing_segment_id`.
5. FIFO overrun, overflow ringa DRDY i niepełne XYZ kończą bieżący segment.
6. Raz utworzony payload pakietu jest niezmienny aż do commit/overwrite.
7. Retransmisja tego samego `packet_seq` zwraca identyczne kotwice czasu.
8. Host wysyła commit dopiero po trwałym zapisaniu wszystkich fragmentów
   pakietu.
9. Deduplikacja używa co najmniej
   `(node_id, boot_epoch, sample_seq)`.
10. Próbka trafia do dokładnie jednego półotwartego okna `[start, end)`.
11. Niepewność przecinająca granicę okna nie jest arbitralnie rozstrzygana.
12. Zamknięty `.h5` jest niezmienny; zapis odbywa się do `.partial`.
13. `complete=true` oznacza zakończoną i zwalidowaną finalizację, nie brak luk.
14. ISR DRDY nie wykonuje SPI, alokacji, logowania ani blokujących operacji.
15. Brak wsparcia v2 jest wykrywany negocjacją, a nie interpretowany heurystycznie.

## 3. Docelowa architektura

```text
core 0 / node

DRDY IRQ
  -> SPSC DrdyTimestampRing
  -> FifoTimestampBinder
  -> RawTimedSample
  -> symmetric FIR x2 (czas środkowego tapu)
  -> OutputTimedSample
  -> AcquisitionBuffer: XYZ + seq, bez pełnego czasu
  -> DataPlane staging: 32 próbki + czasy
  -> packet-local sample clock:
       first(seq,time), last(seq,time), max_residual
  -> immutable Burst v2 packet

core 1 / node

TimeSync request RX
  -> t2 po odebraniu poprawnej ramki
  -> odpowiedź oczekująca
  -> t3 i finalne CRC tuż przed DMA TX

host

Burst v1/v2 parser + capability negotiation
  -> packet-local reconstruction czasu device
  -> per-node affine clock model z wielu kotwic
  -> TimeSync t1..t4: device monotonic -> host monotonic
  -> wall-clock correlation: host monotonic -> UTC
  -> uncertainty propagation
  -> split into UTC windows
  -> transactional multi-window HDF5 v5
  -> durable flush/fsync
  -> node commit
```

## 4. Format i kompatybilność protokołu

### 4.1 Negocjacja

Dodajemy komendę `GetCapabilities`, nie zmieniamy
`FRAME_PROTOCOL_VERSION = 2`.

Odpowiedź zawiera:

```text
u8  command
u8  status
u8  capabilities_version
u8  burst_format_max
u32 feature_flags
u16 max_samples_per_packet
```

Flagi co najmniej:

- `CAP_BURST_TIME_V2`;
- `CAP_TIME_SYNC_V1`;
- `CAP_DRDY_CLOCK_MODEL`;
- `CAP_BOOT_EPOCH`;
- `CAP_HOLDOVER_QUALITY`.

Stary node zwróci `Unsupported`. Nowy host traktuje to jako jawne v1, nigdy
jako uszkodzoną odpowiedź. Stary host nadal może rozmawiać z nowym nodem, ale
nie może zaakceptować burstu v2 jako v1.

### 4.2 Burst v2

Pierwsze 17 B pozostaje zgodne z obecnym `BurstDataPayloadHeader`:

```text
u8  command
u8  status
u32 packet_seq
u64 first_sample_seq
u16 sample_count
u8  sample_encoding
```

`sample_encoding = RAW_XYZ24_TIME_V2` informuje, że po prefiksie występuje:

```text
u8  timing_format_version = 2
u8  timestamp_source
u16 timestamp_quality_flags
u64 boot_epoch
u32 timing_segment_id
u64 first_device_time_us
u64 last_device_time_us
u32 sample_period_q16_us
u32 max_fit_residual_us
```

Następnie występuje `XYZ24[sample_count]`.

Pierwsza kotwica wskazuje `first_sample_seq`, druga wskazuje
`first_sample_seq + sample_count - 1`. Dla `sample_count >= 2` host wyznacza
lokalny okres z dwóch kotwic i porównuje go z `sample_period_q16_us`. Pole
okresu obsługuje pakiet jednoelementowy i kontrolę spójności.

Parser odrzuca pakiet, gdy:

- długość nie odpowiada liczbie próbek;
- wersja, encoding lub źródło czasu są nieznane;
- `sample_count == 0`;
- czasy cofają się;
- okres jest zerowy lub poza zakresem wynikającym z konfiguracji ODR;
- residual/quality przekracza politykę `required`;
- sequence nie jest ciągły z nagłówkiem;
- epoka lub segment są niespójne z bieżącą sesją.

Rozmiar struktur, offset każdego pola, endianowość i złote wektory bajtowe
muszą być testowane po obu stronach.

### 4.3 TimeSync v1

Komenda ma własną wersję i `sync_id`. Host rejestruje:

```text
t1_host_monotonic_ns
t1_utc_before_ns
t1_utc_after_ns
```

Node zwraca:

```text
boot_epoch
echoed_t1_host_monotonic_ns
t2_node_rx_us
t3_node_tx_us
```

Host rejestruje `t4_host_monotonic_ns` natychmiast po pełnej ramce.

`t2` przechwytujemy w `Transport::try_decode_one_frame()` po walidacji CRC.
TimeSync jest obsługiwany w warstwie transportu, nie w kontrolerze, ponieważ
`t3` musi być uzupełniony przy faktycznym rozpoczęciu TX. Slot odpowiedzi
TimeSync przechowuje strukturę do późnego kodowania. Tuż przed
`start_tx_dma()` transport:

1. pobiera `t3 = time_us_64()`;
2. uzupełnia payload;
3. koduje ramkę i CRC;
4. rozpoczyna DMA.

Test mierzy też opóźnienie od `t3` do DMA start i dodaje jego limit do
niepewności.

## 5. Model zegara próbki

### 5.1 Packet-local

DataPlane ma wszystkie output timestampy podczas budowy pakietu. Nie przechowuje
ich po jednym w payloadzie. Oblicza:

- czas pierwszej i ostatniej próbki;
- okres z różnicy endpointów;
- maksymalną wartość bezwzględną reszty względem prostej endpointów;
- zbiorczą jakość wszystkich próbek.

Dzięki temu każdy pakiet jest samowystarczalny, a krótkookresowy jitter jest
ujęty w `max_fit_residual_us`.

### 5.2 Długoterminowy

Host utrzymuje model:

```text
device_time_us = slope_us_per_sample * sample_seq + intercept_us
```

Model jest osobny dla `(node_id, boot_epoch, timing_segment_id)`.

Algorytm:

1. przechowuje ograniczone okno endpointów pakietów;
2. odrzuca punkty o złej jakości lub cofnięciu czasu;
3. używa ważonej regresji liniowej;
4. waży punkty odwrotnie do niepewności/residualu;
5. raportuje slope, dryft względem nominalnego ODR, residual i przedział
   niepewności;
6. nie ekstrapoluje dalej niż skonfigurowany holdover;
7. zaczyna od nowa po zmianie epoki/segmentu.

Packet-local endpointy są źródłem czasu dla zapisanych próbek. Model
długoterminowy służy do walidacji, pakietów jednoelementowych, holdover i
wykrywania dryftu.

## 6. Model czasu noda względem hosta

Z dobrych obserwacji `t1..t4` wyznaczamy:

```text
node_mid_us = (t2 + t3) / 2
host_mid_ns = (t1 + t4) / 2
network_rtt_ns = (t4 - t1) - (t3 - t2) * 1000
```

Osobna regresja mapuje:

```text
host_monotonic_ns = a * node_device_us + b
```

Wymagania:

- dolny kwantyl RTT jako kandydaci do modelu;
- co najmniej 5 dobrych obserwacji przed `LOCKED`;
- odporny filtr odstających wartości;
- surowe obserwacje nigdy nie są usuwane z aktywnego HDF;
- limit dryftu, residualu, wieku i RTT jest konfigurowalny;
- ujemny RTT, zła epoka, niezgodne `sync_id` i rollback czasu są odrzucane;
- `HOLDOVER` zwiększa niepewność wraz z wiekiem;
- zmiana epoki natychmiast daje `UNSYNCED`.

Korelację host monotonic -> UTC wykonujemy bracketem:

```text
mono_before, utc, mono_after
```

Niepewność pary obejmuje połowę szerokości bracketu. Skok wall clocka tworzy
nowy segment korelacji UTC. Nie interpolujemy przez korektę NTP ani ręczną
zmianę czasu systemowego.

## 7. Firmware: fazy implementacji

### F0 — profile sprzętowe i kill switch

Zmiany:

- wydzielić `BoardProfile` dla custom V2 i legacy eval;
- usunąć rozproszone definicje pinów z `main.cpp`;
- dodać compile-time `TIMESTAMPING_V2_ENABLED`;
- domyślnie nie aktywować nowej ścieżki na istniejącym buildzie produkcyjnym;
- poprawić dokumentację release, aby oba warianty nie udawały jednej płytki.

Testy/bramka:

- build obu profili;
- statyczna walidacja konfliktów pinów;
- brak zmiany zachowania przy wyłączonej funkcji;
- operator potwierdza profil przed testem fizycznym.

### F1 — typy czasu i semantyka filtra

Zmiany:

- `SampleDeviceTime`, `TimingQuality`, `TimingSegmentId`;
- przeciążenie filtra zwracające czas środkowego tapu;
- reset timestamp ringa filtra razem z ringiem danych.

Testy/bramka:

- Light/Balanced/Aggressive;
- nieregularne timestampy;
- impuls;
- reset i zmiana profilu;
- brak regresji starego API.

Stan: przeciążenie filtra i test nieregularnych timestampów są rozpoczęte
lokalnie; etap wymaga jeszcze typów jakości i testu impulsu.

### F2 — ring DRDY

Zmiany:

- stałopojemnościowy SPSC ring bez alokacji;
- wpis `{drdy_event_seq, device_time_us}`;
- zatrzask overflow i liczniki;
- jawne `arm/reset/disarm`;
- obsługa DRDY i INT1 w jednym callbacku GPIO bez utraty dotychczasowego INT1.

Testy/bramka:

- pusty/pełny ring, wrap indeksów, overflow;
- monotoniczność i odrzucenie cofnięcia;
- reset przy aktywnym producencie;
- test ze stubem ISR;
- pomiar czasu ISR w buildzie laboratoryjnym;
- zero SPI/logowania z handlera potwierdzone przeglądem.

### F3 — binder FIFO/DRDY i recovery barrier

Zmiany:

- `RawTimedSample`;
- przypisanie N najstarszych DRDY do N najstarszych kompletnych XYZ;
- jawny automat `UNSYNCED/LOCKED/DEGRADED/INVALID`;
- `timing_segment_id`;
- wspólna procedura recovery:
  standby -> disarm IRQ -> drain FIFO -> clear ring -> increment segment ->
  arm DRDY -> measurement;
- nowe liczniki diagnostyczne.

Testy/bramka:

- pełny batch i kilka odczytów po jednym IRQ;
- fallback polling;
- za mało/nadmiar DRDY;
- DRDY pojawiające się podczas odczytu SPI;
- ring overflow;
- FIFO overrun;
- niepełne XYZ;
- soft recovery;
- zmiana ODR/profilu;
- wrap pomocniczego event sequence;
- żadnego cichego przesunięcia o jedną próbkę.

### F4 — model pakietowy i Burst v2

Zmiany:

- czas z filtra trafia do stagingu DataPlane;
- endpointy, okres, residual i quality;
- immutable header v2;
- `GetCapabilities`;
- nowe liczniki packet timing invalid/overwrite.

Testy/bramka:

- złote bajty C++/Python;
- `static_assert` rozmiaru i offsetów;
- little-endian;
- retransmisja byte-for-byte;
- queue overwrite zachowuje spójność;
- pakiety 1, 2 i 32 próbek;
- nieciągłe sequence i mieszany segment są odrzucane przed enqueue;
- v1 i v2 rozróżniane bez heurystyki;
- payload mieści się w 1024 B.

### F5 — TimeSync v1

Zmiany:

- specjalny pending response w `Transport`;
- `t2` przy decode, `t3` tuż przed encode/DMA;
- `boot_epoch`;
- capability i diagnostyka sync.

Testy/bramka:

- echo `sync_id/t1`;
- poprawne offsety pól i CRC po późnym uzupełnieniu t3;
- opóźniona kolejka odpowiedzi;
- burst nie wyprzedza odpowiedzi;
- pełna kolejka odpowiedzi;
- restart/nowa epoka;
- monotoniczność t2/t3;
- brak spontanicznych ramek.

### F6 — diagnostyka i resource gates

Nowe liczniki:

- DRDY captured/dropped/ring overflow;
- binder missing/excess/reset;
- timing segment changes;
- invalid timed samples/packets;
- max packet fit residual;
- TimeSync accepted/rejected;
- boot epoch.

Bramki:

- RAM wolny co najmniej 25% po linkowaniu;
- flash slotu wolny co najmniej 20%;
- ISR DRDY p99 poniżej 10 us przy maksymalnym wspieranym ODR;
- zero overflow ringa w godzinnej symulacji max ODR;
- brak wzrostu FIFO overrun względem baseline.

## 8. Host: fazy implementacji

### H1 — parser, capabilities i tryby migracji

Konfiguracja:

```text
timing_mode = legacy | observe | required
```

- `legacy`: obecna rotacja, v2 może być parsowane, ale nie steruje plikiem;
- `observe`: oba czasy są liczone i porównywane, obecna ścieżka zapisuje dane;
- `required`: tylko poprawny czas akwizycji może utworzyć kompletny HDF5 v5.

Host-first oznacza:

1. wdrożyć hosta rozumiejącego v1 i v2;
2. uruchomić `legacy`;
3. aktualizować nody pojedynczo;
4. przejść na `observe`;
5. dopiero po raporcie jakości przejść na `required`.

Testy:

- Unsupported capabilities;
- v1, v2, unknown v3;
- truncated/oversized payload;
- zła jakość, epoka, segment, okres i residual;
- żadnego przesunięcia SAMPLE_PAYLOAD_OFFSET dla v1.

### H2 — modele zegara

Nowe, czyste moduły bez zależności od serial/HDF:

- `SampleClockModel`;
- `NodeHostClockModel`;
- `UtcCorrelationModel`;
- `TimingStateMachine`;
- `TimingUncertainty`.

Testy deterministyczne:

- idealny zegar;
- stały dryft dodatni/ujemny;
- jitter;
- asymetryczny transport;
- odstające RTT;
- utrata synchronizacji i holdover;
- reset hosta/noda;
- zmiana epoki/segmentu;
- skok UTC/NTP;
- overflow i bardzo duże sequence;
- brak NaN/inf i kontrola precyzji dla wielodniowej sesji.

### H3 — multi-window routing

`WindowedWriter` zostaje zastąpiony managerem:

```text
window_start_utc_ns -> ActiveWindow
```

Router:

- wylicza czas każdej próbki przed zapisem;
- dzieli jeden pakiet na fragmenty okien;
- utrzymuje ograniczoną liczbę okien;
- osobno obsługuje `late`, `ambiguous` i `unsynced`;
- używa UTC do arytmetyki, strefy lokalnej tylko do nazwy.

Testy:

- dokładnie `start`, `end - 1 ns`, `end`;
- burst przecinający jedną i dwie granice;
- opóźniony pakiet;
- kolejność pakietów odwrócona;
- retransmisja;
- DST w Europe/Warsaw;
- różne nody w różnych epokach;
- uncertainty obejmujące granicę;
- ograniczenie liczby otwartych okien.

### H4 — HDF5 v5 i zapis transakcyjny

Schemat:

```text
/nodes/<id>/samples
/nodes/<id>/time_anchors
/nodes/<id>/clock_sync
/nodes/<id>/gaps
/nodes/<id>/temperature
/ingest_batches
```

`samples` dodaje `boot_epoch`, aby restart w obrębie jednego okna nie łamał
deduplikacji. Nie dodajemy UTC per próbka.

Aktywny plik:

1. ma nazwę `.h5.partial`;
2. zapisuje fragmenty datasetów;
3. wykonuje HDF flush;
4. wykonuje `fsync` deskryptora dla wspieranego VFD;
5. dopisuje marker ukończonego ingest batch;
6. ponownie flush/fsync;
7. dopiero wtedy packet może zostać commitowany do noda.

Recovery:

- skanuje `/ingest_batches`;
- przycina osierocone ogony datasetów do ostatnich zatwierdzonych offsetów;
- odbudowuje indeks deduplikacji;
- waliduje monotoniczność i zakres okna;
- nie publikuje pliku, którego nie można jednoznacznie naprawić.

Finalizacja:

- przelicza summary z datasetów;
- waliduje wszystkie anchor ranges;
- zapisuje `complete=true` oraz stan jakości;
- flush/fsync;
- zamyka plik;
- atomowy rename `.partial -> .h5`;
- fsync katalogu nadrzędnego.

Testy fault-injection zatrzymują proces po każdym kroku zapisu i sprawdzają
recovery bez duplikatów i bez utraty już commitowanych danych.

### H5 — recorder scheduling i monitoring

- TimeSync co 5 s do `LOCKED`, później co 30 s;
- GetStats domyślnie co 30–60 s;
- natychmiastowy GetStats po anomalii;
- priorytet zapisu/burstu nad diagnostyką;
- dashboard/status pokazuje epokę, segment, slope, dryft, RTT, residual,
  uncertainty i stan.

Alarmy:

- timing unlocked/invalid;
- DRDY overflow/mismatch;
- dryft poza limitem;
- residual poza limitem;
- ambiguous boundary;
- late data;
- HDF recovery/finalization failure.

## 9. Test integracyjny bez sprzętu

Powstaje deterministyczny symulator noda i transportu z seedem zapisanym w
raporcie. Generuje:

- rzeczywisty ODR różny od nominalnego;
- dryft zmienny z temperaturą;
- jitter DRDY;
- FIFO batching;
- opóźnienia i asymetrię RS485;
- retransmisję i utraconą odpowiedź commit;
- gap, FIFO overrun i ring overflow;
- restart hosta oraz noda;
- zmianę ODR;
- skok UTC;
- pakiet przecinający granicę 600 s.

Oracle zna prawdziwy czas każdej próbki. Test przechodzi, gdy:

- żadna poprawna próbka nie trafia do złego okna;
- żadna próbka nie jest zduplikowana;
- wszystkie celowe straty są widoczne w `gaps`;
- segmenty nie łączą się przez reset;
- retransmisja nie zmienia czasu;
- recovery `.partial` daje ten sam wynik co zapis bez awarii;
- uncertainty nigdy nie jest mniejsza od rzeczywistego błędu w scenariuszach
  testowych.

Uruchamiamy macierz seedów oraz długą symulację równą co najmniej 24 h czasu
wirtualnego.

## 10. Bramki CI i review

Każdy commit:

- jest mały i dotyczy jednej odpowiedzialności;
- ma test negatywny dla głównego failure mode;
- przechodzi `-Wall -Wextra -pedantic`;
- nie zmienia danych golden bez jawnego wyjaśnienia;
- nie miesza refaktoru z migracją formatu.

Wymagane joby:

1. firmware host unit tests;
2. host unittest;
3. build slot A/B/direct/bootloader;
4. protocol golden vectors C++ <-> Python;
5. HDF5 crash recovery matrix;
6. deterministic integration simulation;
7. resource budget report;
8. static check braku wywołań niedozwolonych w ISR.

Merge do gałęzi wdrożeniowej dopiero po zielonych wszystkich bramkach.

## 11. Kolejność commitów

Proponowane, przeglądalne commity:

1. `Document timestamping design and implementation gates`
2. `Add board profiles and timestamping feature gate`
3. `Define sample timing types and FIR timestamp semantics`
4. `Add bounded DRDY timestamp ring`
5. `Bind DRDY events to FIFO samples with explicit recovery`
6. `Add packet-local sample clock model`
7. `Version burst timing payload and capabilities`
8. `Add transport-boundary TimeSync command`
9. `Parse timed bursts and negotiate capabilities on host`
10. `Estimate sample, node and UTC clock models`
11. `Route samples into acquisition-time windows`
12. `Add transactional HDF5 v5 writer and recovery`
13. `Integrate recorder scheduling and timing diagnostics`
14. `Add jitter, retransmission, reset and crash simulation`
15. `Report firmware resources and RS485 throughput`

Nie commitujemy całej ścieżki jako jednego diffu.

## 12. Laboratoryjny rollout

Wymaga osobnej zgody operatora.

### L0 — kontrola offline

- potwierdzenie wariantu płytki i pinów na schemacie/netliście;
- build bez flashowania;
- zapis rozmiarów i hashy artefaktów;
- backup aktualnej konfiguracji i firmware.

### L1 — jeden node na stole

- odłączony od magistrali produkcyjnej;
- oscyloskop/logic analyzer: DRDY, INT1, opcjonalny GPIO debug;
- porównanie liczby DRDY, FIFO samples i output samples;
- 1 h przy typowym ODR, następnie test max ODR;
- wymuszone zatrzymanie hosta, retransmisja, restart noda i zmiana ODR;
- brak flashowania kolejnego noda do zakończenia raportu.

### L2 — magistrala testowa

- 2, potem 4 nody;
- 115200 baud, 1–4 pakiety/grant;
- TimeSync + rzadszy GetStats;
- pomiar utilization, queue lag, overwrite i RTT;
- 24 h soak;
- porównanie `legacy` i `observe`.

### L3 — canary

- jeden niekrytyczny kanał;
- host w `observe` przez ustalony okres;
- raport różnicy `receive-time window` vs `acquisition-time window`;
- przejście na `required` tylko po akceptacji raportu.

### L4 — stopniowe wdrożenie

- jeden kanał naraz;
- obserwacja co najmniej jednego pełnego cyklu plików;
- zachowanie artefaktów A/B i procedury powrotu;
- żadnej masowej aktualizacji bez punktu kontrolnego.

## 13. Kryteria stop/rollback

Natychmiast zatrzymujemy rollout, gdy:

- pojawi się DRDY ring overflow w nominalnej pracy;
- liczba związanych próbek różni się od liczby poprawnych FIFO samples;
- residual lub uncertainty przekracza ustalony budżet;
- rośnie FIFO overrun, packet overwrite lub RX overflow;
- throughput nie utrzymuje bieżącego ODR dla liczby nodów;
- wystąpi duplikat lub próbka w dwóch oknach;
- `.partial` nie odtwarza się deterministycznie;
- nowy host błędnie interpretuje v1 albo stary host v2;
- watchdog/recovery frequency pogarsza się względem baseline.

Rollback:

- host: `required -> observe -> legacy`;
- firmware: istniejący mechanizm A/B do poprzedniego obrazu;
- HDF5: czytniki obsługują równolegle v4 i v5;
- ukończonych v5 nie konwertujemy w miejscu;
- `.partial` zachowujemy do analizy, nie usuwamy automatycznie.

## 14. Definition of Done

Upgrade jest gotowy dopiero, gdy:

1. wszystkie niezmienniki z sekcji 2 mają test lub jawny argument statyczny;
2. host działa z v1, v2 i mieszaną flotą;
3. test 24 h wirtualnego czasu nie daje złego okna ani duplikatu;
4. fault injection potwierdza recovery HDF5 po każdym punkcie awarii;
5. build firmware mieści się w budżecie RAM/flash;
6. test RS485 potwierdza wymagany throughput;
7. raport `observe` potwierdza dryft, residual i uncertainty;
8. test laboratoryjny jednego noda przejdzie bez utraty DRDY/FIFO;
9. istnieje zatwierdzony rollback hosta i firmware;
10. operator osobno zatwierdzi przejście canary na `required`.
