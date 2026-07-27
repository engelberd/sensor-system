# Prompt implementacyjny: deterministyczne przypisywanie czasu próbek z DRDY

## Cel

Zaprojektuj i zaimplementuj profesjonalny mechanizm przypisywania próbek
akcelerometru ADXL355 do 10-minutowych plików HDF5 na podstawie momentu
powstania próbki, a nie czasu odebrania pakietu przez host.

Zmiany mają być przygotowane i przetestowane w odseparowanej przestrzeni.
Nie wdrażaj ich na działającym czujniku na moście i nie wykonuj żadnych operacji
na jego portach szeregowych, usługach systemd, plikach produkcyjnych ani
firmware bez osobnej, jawnej zgody operatora.

## Kontekst systemu

- Sensor: ADXL355.
- MCU: RP2350.
- Akwizycja działa na core 0, transport RS485 na core 1.
- ADXL355 pracuje z FIFO i filtrowaniem/decymacją x2.
- Typowa konfiguracja:
  - sensor ODR: 250 Hz,
  - output ODR: 125 Hz,
  - 32 próbki wyjściowe w pakiecie,
  - do 4 pakietów na przydzielony burst.
- Host zapisuje dane w nominalnych, wyrównanych oknach 600 s.
- `sample_seq` jest nadawany po filtracji/decymacji.
- Transport ma semantykę grant/burst/commit i może dostarczyć wcześniej
  zakolejkowane pakiety. Czas odbioru pakietu przez host nie jest czasem
  pomiaru.
- Restart hosta lub noda nie musi zachowywać globalnej, ciągłej historii czasu.
  Mechanizm ma poprawnie działać w obrębie aktualnej sesji i umożliwić
  przypisanie próbki do właściwego pliku 10-minutowego.
- Lokalny czas kalendarzowy jest potrzebny do rotacji plików. Algorytm analizy
  nie wymaga osobnego timestampu przy każdej próbce historycznej.

Istotne pliki:

- `node/main.cpp`
- `node/adxl355/adxl355_driver.h`
- `node/adxl355/adxl355_driver.cpp`
- `node/acquisition/acquisition_engine.h`
- `node/processing/decimating_filter.h`
- `node/storage/stored_sample.h`
- `node/transport/command_payloads.h`
- `node/transport/data_plane.h`
- `node/transport/transport.h`
- `host/host_lab.py`
- `host/host_recorder.py`
- `host/tests/test_host_recorder.py`
- `node/tests/`

## Obecny stan i problemy

1. `StoredSample` zawiera tylko `sample_seq` oraz XYZ.
2. Nagłówek burstu zawiera `packet_seq`, `first_sample_seq`, `sample_count`
   i `sample_encoding`, ale nie zawiera kotwicy czasu akwizycji.
3. Host wybiera plik na podstawie bieżącego czasu hosta przed zapisaniem całego
   batcha. Burst przecinający granicę okna może w całości trafić do niewłaściwego
   pliku.
4. `GetStats` jest wykonywane okresowo dla monitoringu zdrowia. Nie może być
   podstawowym źródłem czasu próbek.
5. `last_progress_ms` opisuje czas przetworzenia próbki/FIFO, a nie dokładny
   moment DRDY.
6. Czas odbioru RS485 zawiera zmienne opóźnienie kolejki, grantów, komend
   sterujących i transmisji.

## Wymagany model czasu

Zdefiniuj trzy osobne czasy:

1. `sample_device_time` — czas powstania próbki w domenie monotonicznego zegara
   RP2350, wyprowadzony z DRDY.
2. Model mapowania zegara RP2350 na czas hosta potrzebny do rotacji plików.
3. Czasy transportowe request/response/receive — wyłącznie diagnostyczne.

Czas transportu nie może zmieniać czasu próbki. Retransmisja tego samego pakietu
musi zachować identyczną kotwicę.

## Decyzja architektoniczna

### DRDY jako źródło czasu

Wykorzystaj sprzętowy sygnał DRDY ADXL355 oraz 64-bitowy monotoniczny timer
RP2350:

- włącz DRDY w konfiguracji sensora;
- włącz IRQ GPIO dla istniejącego pinu DRDY;
- w handlerze wykonuj tylko minimalne operacje:
  - odczyt `time_us_64()`,
  - inkrementacja numeru zdarzenia,
  - zapis do ograniczonego ring buffera;
- nie wykonuj SPI, alokacji ani rozbudowanego logowania w ISR;
- zapewnij bezpieczną komunikację IRQ/core 0;
- wykrywaj przepełnienie bufora timestampów jako błąd jakości danych.

Najpierw zweryfikuj w schemacie i konfiguracji płytki, że DRDY jest fizycznie
podłączone do pinu używanego przez firmware. Nie zakładaj, że bieżące lokalne
zmiany w `node/main.cpp` są zatwierdzoną konfiguracją produkcyjną.

### Powiązanie DRDY z próbkami FIFO

Zaprojektuj jawny mechanizm wiążący kolejne zdarzenia DRDY z kolejnymi surowymi
próbkami odczytanymi z FIFO. Musi obsłużyć:

- zwykły odczyt pełnej partii;
- kilka odczytów FIFO po jednym IRQ;
- fallback polling;
- brak zdarzenia DRDY;
- nadmiar zdarzeń DRDY;
- FIFO overrun;
- odrzuconą lub niepełną próbkę XYZ;
- restart/reinicjalizację sensora;
- soft recovery;
- zmianę konfiguracji ODR;
- wrap lub reset liczników pomocniczych.

Nie wolno po cichu przesuwać powiązania timestamp–próbka. Przy utracie
jednoznaczności oznacz segment jako zdegradowany lub nieważny i rozpocznij nowe
wiązanie od zweryfikowanego punktu.

### Filtr i decymacja

Określ formalną semantykę czasu próbki wyjściowej po filtrze x2.

- Przeanalizuj implementację filtra.
- Wyznacz deterministyczne opóźnienie grupowe lub dokładną regułę wyboru czasu
  wyjścia na podstawie timestampów wejściowych.
- Dodaj testy dla impulsu i znanych timestampów wejściowych.
- Zapisz w metadanych identyfikator/wersję filtra i semantykę timestampu.

Nie przypisuj automatycznie czasu ostatniej próbki wejściowej, jeżeli matematyka
filtra wskazuje inny czas reprezentatywny.

## Kotwica w protokole

Nie dodawaj timestampu do każdego rekordu XYZ. Dodaj samowystarczalną kotwicę
do każdego pakietu burstu, co najmniej:

```text
boot/session epoch
anchor_sample_seq
anchor_device_time_us
timestamp_source
timestamp_quality/status
```

Kotwica musi wskazywać konkretną próbkę zawartą w pakiecie. Preferowana jest
ostatnia próbka pakietu, jeśli upraszcza to odporne powiązanie z DRDY.

Wymagania:

- ta sama zawartość pakietu i kotwicy przy ponownym odczycie/retransmisji;
- wersjonowanie formatu protokołu;
- parser odrzuca niespójne `anchor_sample_seq`;
- starszy host nie może błędnie zinterpretować nowego nagłówka;
- nowy host powinien jawnie obsłużyć albo odrzucić stary format;
- testy serializacji, rozmiaru struktur, endianowości i niepoprawnych danych.

Jeżeli przechowywanie timestampu w każdej `StoredSample` nie jest potrzebne,
zaproponuj strukturę per batch/per packet ograniczającą użycie RAM. Najpierw
udowodnij, że nie utraci ona poprawnego powiązania podczas kolejkowania.

## Mapowanie czasu noda na czas hosta

Zaprojektuj osobną, wersjonowaną komendę synchronizacji. Nie wykorzystuj
`GetStats` jako protokołu czasu.

Preferowany mechanizm czterech timestampów:

```text
t1: host monotonic — wysłanie requestu
t2: node monotonic — odebranie requestu
t3: node monotonic — rozpoczęcie odpowiedzi
t4: host monotonic — odebranie odpowiedzi
```

Wymagania:

- timestampy noda pobierane możliwie blisko granicy RX/TX, nie po zmiennym
  przetwarzaniu;
- host używa zegara monotonicznego do RTT;
- osobna korelacja host monotonic ↔ UTC;
- wybór próbek synchronizacji o najmniejszym RTT;
- estymacja offsetu i dryftu, nie tylko pojedynczy offset;
- przechowywanie surowych obserwacji i oszacowanej niepewności;
- brak interpolacji przez restart/zmianę epoki noda;
- jawne stany co najmniej `LOCKED`, `HOLDOVER`, `UNSYNCED`, `INVALID`.

Globalna ciągłość czasu przez restarty nie jest wymagana. Po restarcie można
utworzyć nową epokę i nowy model. Nie wolno jednak przypisać próbek z nowej
epoki do starego modelu.

## Rotacja i zapis HDF5

Host ma przypisywać próbki do okna na podstawie czasu akwizycji wyliczonego
z kotwicy DRDY i modelu zegara, a nie na podstawie `datetime.now()` w chwili
zapisu.

Wymagania:

- batch przecinający granicę 10 minut zostaje rozdzielony;
- zakres okna jest półotwarty: `[start, end)`;
- jedna próbka może trafić dokładnie do jednego okna;
- opóźniony pakiet trafia do okna czasu pomiaru;
- retransmisja nie tworzy duplikatu;
- host utrzymuje ograniczoną liczbę otwartych/oczekujących okien albo jawnie
  obsługuje spóźnione dane;
- plik jest publikowany jako kompletny dopiero po finalizacji;
- aktywny plik powinien używać nazwy tymczasowej, a zamknięcie atomowego rename.

Zaproponuj schemat HDF5 obejmujący:

```text
/nodes/<id>/samples
/nodes/<id>/time_anchors
/nodes/<id>/clock_sync
/nodes/<id>/gaps
```

Każdy plik powinien zawierać co najmniej:

- wersję schematu;
- początek i koniec nominalnego okna UTC;
- pierwszy/ostatni `sample_seq`;
- faktyczny czas pierwszej/ostatniej próbki;
- liczbę próbek;
- stan i maksymalną niepewność czasu;
- identyfikator epoki/boota;
- informację o ODR, filtrze i opóźnieniu filtra;
- flagę kompletności.

Nie wymagamy materializowania UTC przy każdej próbce. Czytnik ma móc odtworzyć
czas lub co najmniej jednoznacznie potwierdzić przynależność próbki do pliku.

## GetStats i diagnostyka

Oddziel telemetrię zdrowia od synchronizacji czasu:

- zachowaj `GetStats` do baseline, health monitoring i diagnostyki;
- zaproponuj rzadszy domyślny interwał, np. 30–60 s;
- wykonuj dodatkowy odczyt po anomalii i na żądanie;
- rozważ kompaktowe flagi zmiany krytycznych liczników w odpowiedzi na grant;
- nie wysyłaj spontanicznych ramek na magistrali multidrop;
- nie pogarszaj wykrywania FIFO overrun, dropped samples, RX overflow,
  packet overwrite i soft recovery;
- zmierz wpływ komend diagnostycznych na przepustowość RS485.

## Kryteria akceptacji

Implementacja jest gotowa dopiero, gdy:

1. Czas próbki nie zależy od czasu odbioru burstu.
2. Sztuczne opóźnienie/retransmisja pakietu nie zmienia docelowego okna.
3. Burst przecinający granicę okna jest poprawnie rozdzielany.
4. Restart hosta tworzy nowy model synchronizacji bez błędnego użycia starego.
5. Restart noda/zmiana epoki nie powoduje interpolacji przez restart.
6. Utrata timestampu DRDY daje jawną degradację jakości, nie ciche przesunięcie.
7. FIFO overrun lub niejednoznaczne powiązanie unieważnia odpowiedni segment.
8. Dryft zegara noda jest estymowany na podstawie wielu synchronizacji.
9. Każdy zapisany plik da się jednoznacznie zwalidować offline.
10. Stary i nowy format protokołu są rozróżniane wersją.
11. Są testy jednostkowe firmware i hosta oraz test integracyjny z symulowanym
    jitterem, opóźnieniem, retransmisją, luką i resetem.
12. Nie wykonano żadnego wdrożenia ani operacji na żywym sensorze.

## Wymagane artefakty

Przed implementacją przygotuj krótki dokument projektowy zawierający:

- diagram przepływu timestampu od DRDY do HDF5;
- definicję czasu próbki po filtrze;
- format nowych struktur protokołu;
- model synchronizacji i propagacji niepewności;
- zachowanie przy wszystkich wymienionych błędach;
- wpływ na RAM, flash, pasmo RS485 i CPU;
- plan kompatybilności oraz migracji schematu HDF5.

Następnie:

1. Zaimplementuj zmiany małymi, przeglądalnymi commitami.
2. Dodaj testy przed próbą pracy z fizycznym urządzeniem.
3. Uruchom testy host-side oraz firmware host tests.
4. Zbuduj firmware, ale go nie wgrywaj.
5. Przedstaw diff, wyniki testów, nierozstrzygnięte ryzyka oraz oddzielny plan
   kontrolowanego testu laboratoryjnego.

Nie skracaj rozwiązania do timestampowania odbioru pakietu, okresowego
`GetStats` ani założenia idealnego ODR. Jeśli istniejący pin DRDY nie jest
fizycznie dostępny, zatrzymaj implementację i przedstaw tę przeszkodę wraz
z bezpiecznym wariantem opartym o INT1 jako rozwiązaniem o niższej jakości.
