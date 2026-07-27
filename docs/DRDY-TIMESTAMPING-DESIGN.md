# Projekt deterministycznego czasu próbek DRDY

Status: zaimplementowany i zweryfikowany offline; rollout sprzętowy oczekuje  
Zakres: firmware noda, protokół RS485 i recorder HDF5  
Poza zakresem: programowanie urządzenia, operacje na portach i usługach produkcyjnych

## 1. Decyzje i założenia

1. Źródłem czasu próbki surowej jest zbocze `DRDY` zarejestrowane przez
   `time_us_64()` na RP2350.
2. Czas próbki po symetrycznym filtrze FIR x2 jest czasem wejścia leżącego pod
   środkowym współczynnikiem filtra. Nie jest to czas odczytu FIFO ani czas
   ostatniego wejścia filtra.
3. Każdy zakolejkowany pakiet przechowuje niezmienny blok czasu. Retransmisja
   wysyła te same bajty, łącznie z identyfikatorem epoki i czasami.
4. Nie zakładamy, że nominalny ODR jest idealny. Z kolejnych par
   `(sample_seq, device_time_us)` estymujemy rzeczywisty okres próbkowania oraz
   jego dryft. Pakiet nie zawiera timestampu ani delty dla każdej próbki.
5. Host mapuje czas monotoniczny noda na UTC osobnym protokołem synchronizacji.
   `GetStats` pozostaje wyłącznie diagnostyką.
6. Okna HDF5 są półotwarte `[start, end)`, aktywne pliki mają rozszerzenie
   `.partial`, a kompletne pliki są publikowane atomowym `rename`.
7. Format czasowy jest nową, jawną wersją protokołu. Host produkcyjny nie może
   cicho zaakceptować pakietu bez czasu.

## 2. Weryfikacja sprzętu

Repozytorium zawiera spójne wskazanie dla projektowanej płytki custom V2:

- `hardware/design-notes.md`: `DRDY -> GPIO14`, `INT1 -> GPIO15`;
- `hardware/KiCad - node V2/README.md`: ta sama mapa;
- schemat i PCB zawierają sieci `ADXL_DRDY` oraz `ADXL_INT1`;
- `node/main.cpp` po commicie `686d770` używa `GPIO14` i `GPIO15`.

Nie jest to jednak dowód konfiguracji każdego istniejącego noda. Dokumentacja
starszej wiązki ewaluacyjnej (`node/README-bootloader.md` i generator pakietu
release) nadal podaje `DRDY -> GPIO11`, `INT1 -> GPIO10`. Implementacja musi
wydzielić mapę płytki do jawnego profilu build/config i nie może zostać
uruchomiona na urządzeniu przed potwierdzeniem wariantu sprzętowego operatora.

## 3. Przepływ czasu

```text
ADXL355 DRDY
  -> ISR GPIO: {event_seq, device_time_us}
  -> ograniczony SPSC ring timestampów
  -> binder FIFO: surowa próbka <-> kolejne zdarzenie DRDY
  -> FIR x2: wynik + czas środkowego wejścia filtra
  -> model sample clock: device_time_us = a * sample_seq + b
  -> staging DataPlane (32 próbki)
  -> wersjonowany pakiet: XYZ + epoka + dwie kotwice + jakość modelu
  -> kolejka pakietów (niezmienne bajty)
  -> RS485 grant/burst/retransmisja
  -> parser hosta
  -> model device monotonic -> host monotonic -> UTC
  -> podział pakietu po UTC na okna [start, end)
  -> /nodes/<id>/{samples,time_anchors,clock_sync,gaps}
  -> flush, finalizacja metadanych, rename .partial -> .h5
```

Transport ma własne czasy `t1..t4`, ale nie modyfikuje czasu próbki.

## 4. Przechwytywanie DRDY i wiązanie z FIFO

### 4.1 Ring timestampów

ISR wykonuje tylko:

1. `time_us_64()`;
2. inkrementację `event_seq`;
3. zapis `{event_seq, device_time_us}` do SPSC ringa;
4. ustawienie zatrzaskowej flagi overflow, jeżeli ring jest pełny.

ISR nie wykonuje SPI, alokacji, formatowania tekstu ani logowania. Producentem
ringa jest IRQ na core 0, konsumentem `AcquisitionEngine` na tym samym rdzeniu.
Indeksy są atomowe lub chronione minimalną sekcją IRQ; nie używamy blokującego
mutexa w ISR.

Zaimplementowana pojemność to 512 wpisów. Ring ma compile-time limit i testy
overflow/wrap; rzeczywisty margines czasowy zależy od ODR i pozostaje bramką
testu laboratoryjnego przed włączeniem funkcji na urządzeniu.

### 4.2 Stan bindera

Binder ma stany:

- `UNSYNCED` — brak bezpiecznego powiązania;
- `LOCKED` — liczba i kolejność zdarzeń odpowiadają odczytom FIFO;
- `DEGRADED` — czasy istnieją, ale wykryto zdarzenie obniżające jakość;
- `INVALID` — utracono jednoznaczność segmentu.

Przy wejściu w measurement mode ring jest zerowany, zwiększany jest
`timing_segment_id`, a binder czeka na pierwsze zweryfikowane zdarzenia.
Odczyt N kompletnych próbek XYZ z FIFO konsumuje N najstarszych oczekujących
zdarzeń DRDY. Kilka odczytów po jednym IRQ działa identycznie: INT1 tylko
uruchamia obsługę FIFO, a DRDY numeruje faktyczne próbki.

Warunki jakości:

| Zdarzenie | Zachowanie |
|---|---|
| Pełna partia, N zdarzeń dostępnych | przypisanie 1:1, `LOCKED` |
| Polling fallback, zdarzenia kompletne | przypisanie 1:1, `LOCKED` |
| Za mało zdarzeń DRDY | segment `INVALID`, brak ekstrapolacji |
| Nadmiar zdarzeń względem stanu FIFO | segment `INVALID`, reset bindera |
| Ring overflow | segment `INVALID`, licznik i gap jakości |
| FIFO overrun lub niepewna strata | segment `INVALID`, nowe wiązanie |
| Niepełne/odrzucone XYZ | odpowiadający czas jest odrzucany; segment kończy się |
| Soft recovery/reinit/zmiana ODR | nowy `timing_segment_id`, warm-up od zera |
| Reset noda | nowa `boot_epoch`; stary model hosta jest niedozwolony |

Po `INVALID` dane mogą być transportowane wyłącznie z jawną jakością
`INVALID`; domyślny recorder zapisuje je do `gaps`/kwarantanny, a nie do
kompletnego okna pomiarowego. Nowy segment nie musi przenosić każdego zdarzenia
DRDY dalej: zachowuje okresowe punkty kotwiczące potrzebne do dopasowania
lokalnego zegara próbek.

### 4.3 Włączenie DRDY

Obecne `configure_fifo()` jawnie wywołuje `set_drdy_enabled_internal(false)` i
uruchamia tylko IRQ INT1. Implementacja zmieni to dla profilu sprzętowego z
potwierdzonym DRDY:

- DRDY włączone w `POWER_CTL`;
- IRQ DRDY na właściwym GPIO;
- INT1 nadal sygnalizuje watermark/overrun i uruchamia odczyt FIFO;
- oba źródła mają oddzielne liczniki i semantykę.

## 5. Semantyka czasu po filtrze

Wszystkie trzy profile używają symetrycznych filtrów FIR o nieparzystej liczbie
tapów. Reprezentatywny czas wyjścia jest czasem wejścia:

```text
center = (tap_count - 1) / 2
output_time = timestamp wejścia pod współczynnikiem coefficients[center]
```

Opóźnienie względem najnowszego wejścia użytego do wyniku:

| Profil | Tapy | Opóźnienie grupowe w surowych próbkach | Przy 250 Hz |
|---|---:|---:|---:|
| Light | 15 | 7 | 28 ms |
| Balanced | 31 | 15 | 60 ms |
| Aggressive | 63 | 31 | 124 ms |

Faza decymatora pozostaje deterministyczna po `reset()`. Dla obecnej
implementacji pierwszy wynik powstaje po odpowiednio 16, 32 i 64 wejściach,
ale jego czas pochodzi ze środkowego wejścia aktywnego okna FIR, a nie z chwili
wywołania `process()`.

Filtr dostanie równoległy ring znaczników czasu albo generyczny rekord
`{AccelSample, SampleTimeTag}`. Testy obejmą impuls w każdym profilu, znane
nieregularne timestampy, reset oraz zgodność fazy x2.

Identyfikator metadanych: `fir_x2_symmetric_v1`; osobno zapisujemy profil,
liczbę tapów, decymację i opóźnienie grupowe.

## 6. Reprezentacja w pamięci noda

Nie rozszerzamy 8192-elementowego `AcquisitionBuffer<StoredSample>` o pełny
timestamp: dodałoby to co najmniej 64 KiB RAM i bufor ten nie jest źródłem
pakietów retransmisyjnych.

Zamiast tego:

- `StoredSample` pozostaje rekordem XYZ/sequence dla historycznego bufora;
- filtr zwraca czas reprezentatywnego wejścia razem z wynikiem;
- lekki model sample clock konsumuje pary `(sample_seq, device_time_us)`;
- DataPlane dołącza do pakietu endpointy i snapshot jakości bieżącego segmentu;
- po utworzeniu pakietu niezmienny blok czasu znajduje się w payloadzie kolejki
  i jest zachowany przy retransmisji.

Kotwica nie musi istnieć w każdej przechowywanej próbce. DataPlane otrzymuje ją
razem z batchami wyjściowymi albo przez oddzielny, wersjonowany snapshot modelu.
Unikamy w ten sposób dublowania pełnych timestampów w dużym buforze akwizycji.

## 7. Format pakietu danych v2

Wszystkie pola wielobajtowe są little-endian. Struktura jest pakowana i ma
`static_assert(sizeof(...))`.

```text
BurstDataPayloadPrefix:
  u8  command
  u8  status
  u32 packet_seq
  u64 first_sample_seq
  u16 sample_count
  u8  sample_encoding = RAW_XYZ24_TIME_V2

BurstTimingExtensionV2:
  u8  timing_format_version = 2
  u8  timestamp_source = DRDY_TIME_US_64
  u16 timestamp_quality
  u64 boot_epoch
  u32 timing_segment_id
  u64 first_device_time_us
  u64 last_device_time_us
  u32 sample_period_q16_us
  u32 max_fit_residual_us

payload:
  XYZ24[sample_count]
```

Pierwsza kotwica wskazuje `first_sample_seq`, druga
`first_sample_seq + sample_count - 1`. DataPlane wylicza także maksymalną resztę
czasów próbek pośrednich względem prostej endpointów. `sample_period_q16_us`
jest estymowany z DRDY, a nie kopiowany bezwarunkowo z nominalnego ODR; obsługuje
pakiet jednoelementowy i kontrolę spójności. Parser odrzuca zerowy okres,
niepasującą epokę, cofnięcie czasu oraz model o jakości `INVALID`.

Stary 17-bajtowy prefiks pozostaje bez zmian. Stary host rozpoznaje nieznane
`sample_encoding` i odrzuca v2 zamiast przesunąć offset XYZ. Rozszerzenie nie
ma narzutu zależnego od liczby próbek i pozostaje wyraźnie mniejsze od pełnego
timestampu przy każdym XYZ. Dokładny
wpływ na przepustowość RS485 należy zmierzyć w teście symulowanym i
laboratoryjnym przed wdrożeniem.

Stary host odrzuca nowe `sample_encoding`; nowy host:

- w trybie produkcyjnym wymaga v2;
- może przyjąć v1 wyłącznie z jawną opcją migracyjną i oznacza czas
  `UNSYNCED`, bez publikowania kompletnego pliku czasowego.

## 8. Epoka noda

`boot_epoch` jest 64-bitowym nonce generowanym raz przy starcie z generatora
sprzętowego RP2350/Pico SDK. Generator jest wstrzykiwalny w testach. Wartość
zero jest niedozwolona.

Host kluczuje wszystkie modele i deduplikację przez `(node_id, boot_epoch)`.
Zmiana epoki natychmiast:

- zamyka poprzedni model bez interpolacji;
- zeruje kandydatów synchronizacji;
- ustawia stan `UNSYNCED`;
- tworzy zdarzenie restartu/gap.

Rollback `device_time_us` w tej samej epoce jest błędem `INVALID`, nie wrapem.

## 9. Synchronizacja noda z hostem

Dodajemy osobną komendę `TimeSyncV1`.

```text
request:
  u8  command
  u8  version = 1
  u32 sync_id
  u64 t1_host_monotonic_ns

response:
  u8  command
  u8  status
  u8  version = 1
  u32 sync_id
  u64 boot_epoch
  u64 echoed_t1_host_monotonic_ns
  u64 t2_node_rx_us
  u64 t3_node_tx_us
```

`t2` jest chwytane przy odebraniu i zweryfikowaniu ramki, przed kolejką komend.
`t3` jest chwytane bezpośrednio przed przekazaniem odpowiedzi do UART. Host
chwyta `t4_host_monotonic_ns` natychmiast po odebraniu kompletnej poprawnej
ramki. Razem z `t1` zapisuje też parę korelacyjną
`(host_monotonic_ns, host_utc_ns)` pobraną możliwie blisko wysłania.

Dla obserwacji:

```text
node_mid_us = (t2 + t3) / 2
host_mid_ns = (t1 + t4) / 2
rtt_ns = (t4 - t1) - (t3 - t2) * 1000
```

Model ma postać:

```text
host_monotonic_ns = slope_ns_per_us * device_time_us + intercept_ns
utc_ns = host_monotonic_ns + wallclock_correlation_offset_ns
```

Host zachowuje surowe obserwacje. Z ruchomego zbioru wybiera dolny kwantyl RTT,
a następnie stosuje ważoną regresję liniową dla offsetu i dryftu. Odrzuca
ujemny RTT, cofnięcie zegara, zmianę epoki i obserwacje odstające. Niepewność
obejmuje co najmniej połowę RTT, resztę regresji, wiek modelu i niepewność
korelacji monotonic/UTC.

Stany:

- `UNSYNCED`: brak wystarczających obserwacji;
- `LOCKED`: co najmniej 5 dobrych obserwacji, ograniczony residual i wiek;
- `HOLDOVER`: brak świeżej obserwacji, ale model nie przekroczył limitu wieku;
- `INVALID`: reset, niespójność lub przekroczona niepewność.

Początkowo synchronizacja co 5 s do uzyskania `LOCKED`, następnie co 30 s.
`GetStats` domyślnie co 30–60 s i dodatkowo po anomalii.

## 10. Podział na okna

Parser odtwarza `device_time_us` każdej próbki z endpointów pakietu. Dla
`sample_count >= 2`:

```text
device_time_us =
    first_device_time_us
    + (sample_seq - first_sample_seq)
      * (last_device_time_us - first_device_time_us)
      / (sample_count - 1)
```

Niepewność rośnie z odległością od kotwicy i uwzględnia reszty dopasowania
DRDY. Pole `sample_period_q16_us` obsługuje pakiet jednoelementowy i waliduje
endpointy. Model zegara wylicza UTC i przed zapisem przypisuje:

```text
window_index = floor(utc_ns / window_ns)
window_start = window_index * window_ns
window_start <= utc_ns < window_start + window_ns
```

Jeden burst może więc zostać rozdzielony na kilka list, a każda próbka trafia
dokładnie do jednej listy. Lokalna strefa czasu wpływa tylko na etykietę ścieżki,
nie na arytmetykę przynależności.

Recorder utrzymuje ograniczony słownik otwartych okien (domyślnie bieżące plus
dwa poprzednie). Finalizacja następuje dopiero, gdy watermark czasu akwizycji
przekroczył koniec okna o skonfigurowany margines i wszystkie wcześniejsze
commity zostały potwierdzone. Dane starsze niż otwarty horyzont trafiają do
jawnej kwarantanny `late`, generują alarm i nie zmieniają ukończonego pliku.

Deduplikacja używa klucza `(node_id, boot_epoch, sample_seq)`. Aktywny `.partial`
jest wznawiany po restarcie hosta i jego high-water marks są odczytywane przed
dalszym zapisem. Commit do noda następuje dopiero po zapisie i `flush`; ponowny
pakiet po utraconej odpowiedzi commit nie dopisuje duplikatów.

## 11. Schemat HDF5 v5

Plik:

```text
attrs:
  schema_version = 5
  nominal_window_start_utc_ns
  nominal_window_end_utc_ns
  complete
  finalized_utc_ns

/nodes/<id>/samples
/nodes/<id>/time_anchors
/nodes/<id>/clock_sync
/nodes/<id>/gaps
/nodes/<id>/temperature
```

`samples` zachowuje `sample_seq`, XYZ i `packet_seq`. Nie materializujemy UTC
przy każdym XYZ.

`time_anchors` zawiera co najmniej:

```text
boot_epoch, timing_segment_id, packet_seq,
first_sample_seq, sample_count,
first_device_time_us, last_device_time_us,
sample_period_q16_us, sample_clock_residual_us,
timestamp_source, timestamp_quality,
model_id, max_uncertainty_ns
```

Jeżeli pakiet przecina granicę, każdy plik dostaje fragment kotwicy obejmujący
tylko zapisany zakres sequence i wystarczający do samodzielnego odtworzenia
czasów tego fragmentu.

`clock_sync` przechowuje surowe `t1..t4`, korelację UTC, RTT, decyzję
accept/reject oraz parametry wersjonowanego modelu. `gaps` rozszerzamy o
`boot_epoch`, zakres sequence/device time, powód i jakość.

Atrybuty noda zawierają pierwszy/ostatni sequence i czas faktyczny, liczbę
próbek, epokę/epoki, ODR, `fir_x2_symmetric_v1`, profil/tapy/opóźnienie,
najgorszą niepewność i stan synchronizacji. Przy finalizacji wartości są
przeliczane z datasetów, `complete=true`, plik jest flushowany/zamykany i
atomowo zmieniany z `.partial` na `.h5`.

## 12. Błędy i polityka zapisu

- `LOCKED`: normalny zapis do okna.
- `HOLDOVER`: zapis do okna, jeżeli niepewność nie przecina granicy; inaczej
  plik pozostaje otwarty lub rekord trafia do kwarantanny.
- `UNSYNCED`: buforowanie ograniczone; po przekroczeniu limitu jawna
  kwarantanna, bez zgadywania UTC.
- `INVALID`: brak publikacji jako kompletne dane czasowe.
- przedział niepewności obejmujący dwie granice: próbka nie jest arbitralnie
  przypisywana; czeka na lepszy model albo jest oznaczana jako boundary
  ambiguous.

## 13. Wpływ zasobów

Szacunek przed pomiarem:

- RAM: ring DRDY około 4 KiB; małe okno regresji i snapshot modelu poniżej
  1 KiB; kolejka pakietów około 5 KiB więcej przy 128 pakietach; brak +64 KiB
  w AcquisitionBuffer;
- flash: kod bindera, synchronizacji i parsera — oczekiwane kilkanaście KiB;
- RS485: +40 B na pakiet 32 próbek, do potwierdzenia testem przepustowości;
- CPU noda: jedno `time_us_64()` i zapis ringa na DRDY oraz proste deltowanie;
- CPU hosta/HDF5: regresja na małym oknie obserwacji i grupowanie maksymalnie
  kilkudziesięciu próbek na pakiet.

Build firmware musi raportować zużycie RAM/flash przed i po zmianie. Test
przepustowości obejmie 1–4 nody, 115200 baud, jitter komend diagnostycznych oraz
4 pakiety na grant.

## 14. Migracja i kolejność implementacji

1. Testy i typy czasu: ring DRDY, binder, semantyka filtra i sample clock.
2. Snapshot sample clock i pakiet v2 z testami bajtów, rozmiaru i retransmisji.
3. `TimeSyncV1`, epoka boota i testy `t1..t4`.
4. Host: parser v2, model zegara i testy jitter/dryft/reset.
5. HDF5 v5: wielooknowy writer, deduplikacja, `.partial`, atomowa finalizacja.
6. Test integracyjny: granica 600 s, opóźnienie, retransmisja, luka, reset.
7. Pełne testy host-side, firmware host tests i build bez flashowania.
8. Osobny, zatwierdzony plan testu laboratoryjnego.

Każdy etap ma być osobnym małym commitem. Do czasu potwierdzenia wariantu
sprzętowego wszystkie testy korzystają ze stubów/symulacji; nie wykonujemy
operacji na żywym sensorze.

## 15. Otwarte ryzyka

1. Trzeba potwierdzić, czy docelowy egzemplarz to custom V2 (`GPIO14/15`) czy
   starsza wiązka eval (`GPIO11/10`).
2. Należy potwierdzić dostępność i API sprzętowego generatora losowego w użytej
   wersji Pico SDK albo wybrać trwały licznik epoki.
3. Model afiniczny wygładza krótkookresowy jitter DRDY; niepewność musi być
   na tyle konserwatywna, by nie przypisać arbitralnie próbki przy granicy.
4. Wzrost payloadu może ograniczyć liczbę nodów przy 115200 baud.
5. Dokładność `t2/t3` zależy od miejsca ich przechwycenia w obecnym stosie
   transportowym; test musi zmierzyć asymetrię RX/TX.
6. Dokumentacja release starszej płytki jest obecnie niespójna z `main.cpp` i
   powinna zostać rozdzielona na profile przed jakimkolwiek wydaniem firmware.
