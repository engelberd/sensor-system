# Framing protocol 3 — analiza przed decyzją

- Status: analiza zakończona; decyzję zamraża ADR-0008, specyfikacja jest osobno
- Date: 2026-09-29
- Zakres: framing application protocol 5 na asynchronicznym UART/RS485

Dokument zachowuje analizę wariantów. Obowiązujący format definiują
[ADR-0008](../adr/0008-framing-v3.md) i
[FRAMING-V3-SPEC.md](FRAMING-V3-SPEC.md).

## 1. Warunki naszego systemu

- UART 8N1, 115200 bit/s, czyli maksymalnie 11520 bajtów/s na przewodzie;
- half-duplex RS485, około 400–800 m;
- jeden host jest jedynym inicjatorem transmisji;
- wiele node na jednej magistrali, ale tylko jeden nadajnik w danej chwili;
- host korzysta obecnie z wieloportowych konwerterów USB–RS485;
- przyszły transport może nie być UART, dlatego framing nie może zawierać
  polityki schedulera ani retencji;
- błędna ramka ma zostać wykryta, odrzucona i odzyskana przez retry; nie może
  prowadzić do częściowego wykonania command;
- dekoder musi używać stałego, policzonego limitu pamięci.

115200 bit/s na 800 m nie jest gwarantowane przez sam standard RS485. Wynik
zależy od kabla, terminacji, topologii, długości stubów, slew rate, biasingu,
izolacji i różnic potencjału mas. Framing chroni przed zaakceptowaniem uszkodzonych
bajtów, ale nie poprawia fizycznego marginesu sygnału.

## 2. Wnioski ze źródeł

### RS485

RS485 określa warstwę elektryczną, a nie format protokołu. Dokumenty projektowe
TI i Analog Devices podkreślają zależność długość–data rate, terminację na obu
końcach magistrali, małe stuby, failsafe biasing i ochronę przed różnicami
potencjału. Reguły orientacyjne różnią się między dokumentami i nie zastępują
pomiaru na docelowym kablu.

Wniosek dla nas: 115200 pozostaje wymaganiem produktu, ale profil 400 m i 800 m
musi otrzymać osobną bramę HIL z pomiarem CRC errors, timeoutów i zapasu sygnału.

### Modbus RTU

Modbus RTU rozpoznaje granice za pomocą ciszy na magistrali: co najmniej 3.5
czasu znaku między ramkami oraz ograniczenia przerwy wewnątrz ramki. Dla
prędkości powyżej 19200 bit/s dokument zaleca stałe czasy 750 µs i 1.75 ms.

Konwerter USB i sterownik mogą przekazywać aplikacji kilka porcji UART jako jeden
bufor albo opóźnić małą porcję o timer latency. Proces hosta nie widzi więc
wiernie przerw występujących na przewodzie. Możemy używać ciszy do turnaround i
ochrony kierunku DE, ale nie jako jedynej granicy parsera.

### PPP/HDLC-like framing

RFC 1662 używa jawnego bajtu flagi, octet stuffing i FCS. Zapewnia
transparentność danych i odzyskanie granicy. Jego narzut zależy jednak od
zawartości: każdy zastrzeżony bajt jest rozszerzany, a pesymistycznie ciało może
urosnąć prawie dwukrotnie. To komplikuje najgorszy budżet czasu magistrali.

### COBS

COBS usuwa `0x00` z zakodowanego ciała, dzięki czemu zero jest jednoznacznym
delimiterem. Publikacja Cheshire'a i Baker podaje pesymistyczny narzut nie
większy niż jeden bajt na 254 bajty danych. Po uszkodzeniu, wstawieniu lub
usunięciu bajtu odbiornik odrzuca bieżącego kandydata i odzyskuje granicę na
następnym delimiterze.

COBS nie wykrywa błędów. Całe odkodowane ciało nadal wymaga CRC.

### Dobór CRC

CRC trzeba dobierać do maksymalnej długości chronionego słowa i oczekiwanej
klasy błędów. Analizy Philipa Koopmana pokazują, że popularność wielomianu nie
jest dowodem jego jakości dla danego rozmiaru wiadomości.

CRC-16 daje tylko 16 bitów check value. Przy wieloletniej, ciągłej pracy i
możliwych burst errors wolimy czterobajtowy CRC dla całej ramki. Dwa dodatkowe
bajty są małym kosztem wobec 9 bajtów XYZ na każdą próbkę.

CRC-32C ma dobrze opisaną specyfikację i zastosowanie w protokołach, ale RP2350
sprzętowo przyspiesza CRC-32/IEEE oraz CRC-16/CCITT, a nie CRC-32C. Przy zaledwie
11520 bajtach/s obliczenie programowe CRC-32C powinno być tanie, lecz przed ADR
porównamy na RP2350:

- CRC-32C w małej implementacji tablicowej lub bez tablicy;
- CRC-32/IEEE przez dostępny mechanizm sprzętowy lub software;
- flash, RAM, czas CPU i właściwości detekcji dla wybranego maksimum ramki.

CRC nie daje ochrony kryptograficznej. System nie traktuje go jako uwierzytelnienia.

## 3. Cel framingu i granica odpowiedzialności

Framing ma rozwiązać dokładnie sześć problemów:

1. **Granica:** po starcie w środku strumienia albo utracie bajtu znaleźć początek
   następnej ramki.
2. **Integralność:** nie przekazać wyżej zmienionego headeru ani payloadu.
3. **Limit:** nigdy nie czekać na rozmiar większy od stałego maksimum i nie
   alokować pamięci według niezaufanych danych.
4. **Routing:** rozpoznać source, destination oraz rodzaj wiadomości przed
   interpretacją payloadu.
5. **Korelacja:** połączyć RESPONSE z konkretnym REQUEST i odrzucić spóźnioną
   odpowiedź.
6. **Koszt:** mieć policzalny najgorszy rozmiar oraz szybkie kodowanie na RP2350.

Kryterium sukcesu można zapisać jednym zdaniem:

> Z dowolnie pofragmentowanego strumienia bajtów decoder emituje wyłącznie
> kompletną ramkę z poprawnym CRC albo jawny błąd, używa stałej pamięci i po
> uszkodzeniu odzyskuje granicę najpóźniej na następnym delimiterze.

Framing celowo nie odpowiada za:

- kolejność i kompletność próbek;
- retry i czas retencji;
- trwały commit;
- scheduler wielu node;
- synchronizację zegarów;
- semantykę start/stop/config/update.

Te gwarancje należą do application protocol i domeny. Dzięki temu framing
pozostaje mały i możliwy do pełnego przetestowania.

## 4. Porównanie wariantów

| Wariant | Granica | Najgorszy narzut | Recovery | Ocena dla nas |
| --- | --- | ---: | --- | --- |
| Cisza jak Modbus RTU | timeout | mały, ale każde okno wymaga ciszy | zależy od dokładnego timingu | słabe przez USB i koszt wielu krótkich transakcji |
| Sync marker + length + CRC | magic + długość | stały | marker może wystąpić w payloadzie; potrzebna ostrożna resynchronizacja | możliwe, ale custom parser jest trudniejszy do dowiedzenia |
| PPP/HDLC octet stuffing | `0x7e` | do około 100% w pesymistycznych danych | dobra na następnej fladze | dojrzałe, lecz zbyt zmienny worst case |
| COBS + `0x00` + CRC | jednoznaczny delimiter | do około 0.4% + delimiter | dobra na następnym zerze | najlepszy kandydat |

SLIP nie jest kandydatem, ponieważ sam nie zapewnia wymaganej kontroli
integralności. Kopiowanie pełnego Modbus nie rozwiązuje naszej semantyki
strumienia, commit i synchronizacji czasu.

## 5. Rekomendowany kandydat do prototypu

```text
wire frame = COBS(raw frame) || 0x00

raw frame:
  framing_version   u8
  role              u8       REQUEST albo RESPONSE
  destination       u8       0=host, 1..253=node, 254=commissioning, 255=broadcast
  source            u8
  service           u8       discovery/transfer/time/config/diagnostics/...
  message           u8       typ w zakresie service
  transaction_id    u32 LE    response powtarza ID requestu
  payload            N bytes
  frame_crc          u32 LE    nad całym headerem i payloadem
```

Surowy nagłówek może mieć tylko 10 bajtów. `payload_length` jest zbędne, ponieważ
COBS delimiter jednoznacznie określa długość odkodowanego ciała, a CRC wykrywa
ucięcie lub sklejenie danych. Usunięcie redundantnego pola upraszcza parser i
oszczędza dwa bajty w każdej ramce. COBS koduje header, payload i CRC razem.
Delimiter nie jest objęty CRC.

To jest kandydat, ponieważ:

- granica nie zależy od timingu widzianego przez Linux;
- `0x00` nigdy nie występuje w poprawnym ciele COBS;
- jeden uszkodzony pakiet nie przesuwa parsera bez ograniczenia;
- nie potrzebujemy sync marker ani osobnego header CRC;
- pesymistyczny rozmiar można policzyć przed wysłaniem;
- `service + message` utrzymuje moduły protokołu bez jednego rosnącego worka
  command IDs;
- 32-bit `transaction_id` ogranicza ryzyko korelacji starej odpowiedzi po wrap;
- sample sequence, boot epoch i operation ID pozostają w typowanych payloadach,
  ponieważ nie są własnością framingu.

Na tej magistrali node nie wysyła ramek spontanicznie. Każda odpowiedź jest
skutkiem requestu hosta i powtarza jego `transaction_id`. Broadcast request nie
otrzymuje odpowiedzi, co zapobiega kolizji wielu node.

## 6. Limity pamięci i recovery

Jeżeli `MAX_PAYLOAD` wynosi 512 bajtów, `MAX_RAW_FRAME` to
`10 header + 512 payload + 4 CRC = 526` bajtów. Bufor zakodowany COBS wymaga
maksymalnie około 530 bajtów wraz z delimiterem. Dla 1024 payloadu wartości
wynoszą odpowiednio 1038 i około 1044 bajtów.
Dokładny wzór i test graniczny będą częścią specyfikacji.

Decoder:

1. zapisuje bajty do stałego bufora aż do `0x00`;
2. po przekroczeniu `MAX_ENCODED_FRAME` przechodzi w `discard_until_delimiter`;
3. odrzuca pusty delimiter jako idle/noise counter;
4. wykonuje COBS decode do drugiego stałego bufora;
5. sprawdza minimalny rozmiar, framing version, role, adresy i CRC;
6. publikuje ramkę dopiero po przejściu wszystkich kontroli;
7. po timeout może wyzerować tylko niedokończone ciało; timeout nie wyznacza
   poprawnej granicy ramki.

Nie ma alokacji zależnej od wartości `payload_length` z przewodu.

## 7. Wpływ na przepustowość

Dla obecnego payloadu Timing v2 około 345 bajtów kandydat ma w przybliżeniu:

```text
345 payload + 10 header + 4 CRC = 359 raw bytes
COBS + delimiter                  ≈ 3 dodatkowe bajty
wire frame                        ≈ 362 bajty
```

To jest zbliżone do obecnych 360 bajtów. Sam framing nie da dużego wzrostu
przepustowości, ponieważ dominują próbki XYZ i metadata czasu. Zysk musi powstać
przede wszystkim w application payload, rozmiarze chunku i liczbie transakcji.

Nie należy usuwać CRC lub jednoznacznej granicy dla kilku bajtów oszczędności.

## 8. Testy wymagane przed ADR

### Testy algorytmiczne na hoście

- wszystkie możliwe podziały jednej ramki na fragmenty wejściowe;
- wiele ramek w jednym buforze;
- `0x00` w każdym miejscu raw payloadu przed COBS;
- uszkodzenie każdego pojedynczego bitu w reprezentatywnych ramkach;
- usunięcie i wstawienie każdego bajtu;
- uszkodzony delimiter i dwa sklejone pakiety;
- brak delimitera oraz przepełnienie bufora;
- zero-length i maksymalny payload;
- niezgodna wersja, role, length, address i CRC;
- identyczne golden vectors Rust/C++.

### Benchmark RP2350

- encode/decode COBS dla 0, typowego i maksymalnego payloadu;
- CRC-32C kontra CRC-32/IEEE;
- flash, stos/RAM i czas na core 1;
- praca przy jednoczesnej akwizycji i maksymalnym transferze.

### HIL RS485

- 400 m i 800 m na docelowym kablu;
- 1, nominalna i maksymalna liczba node;
- terminacja, bias, stubs i odłączanie node;
- ciągły 115200 przez wiele godzin;
- zakłócenia, utrata bajtów, turnaround DE i echo adaptera;
- osobne liczniki physical/UART error, malformed COBS, CRC error, timeout i retry.

## 9. Decyzje zamknięte

ADR-0008 zatwierdza COBS + `0x00`, CRC-32C, `MAX_PAYLOAD=512`, 10-bajtowy
nagłówek bez length i politykę timeout/turnaround poza framingiem. Błędy
application są typowanymi response danego service; framing raportuje wyłącznie
błędy envelope, routingu, COBS, limitu i CRC. Normatywny format znajduje się w
[FRAMING-V3-SPEC.md](FRAMING-V3-SPEC.md).

## 10. Migracja node bez dostępu USB

Obecny bootloader A/B używa osobnego update protocol `2`, własnego magic,
CRC-32 i chunków do 1024 bajtów. Zmiana framingu aplikacji z `2` na `3` nie
wymaga zmiany bootloadera. Upload zapisuje wyłącznie nieaktywny slot aplikacji.

Pierwszą migrację można wykonać następująco:

1. Stare narzędzie komunikuje się z działającą aplikacją przez framing 2.
2. Odczytuje i waliduje board revision oraz zatrzymuje recording.
3. Wysyła starą komendę `EnterBootloader` (`0x03`).
4. Po restarcie przechodzi na niezależny bootloader update protocol 2.
5. Wgrywa aplikację framing 3 do nieaktywnego slotu.
6. Bootloader uruchamia nowy slot jako trial, zachowując stary active slot.
7. Nowy host wykonuje handshake framing 3 i sprawdza minimalny health sensora.
8. Dopiero jawne `ConfirmTrialBoot` przez framing 3 zatwierdza nowy slot.

Jeżeli handshake albo health nie przejdzie, trial app musi sama wywołać restart
przed deadline. Przy następnym boot bootloader widzi niepotwierdzony, już
próbowany slot i wraca do starego active slotu z framingiem 2.

Implementacja protocol 5 spełnia punkt 8: obrazy slotów używają `main_v2.cpp`,
który po lokalnym health czeka na osobne `ConfirmTrialBoot` od hosta. Brak
potwierdzenia przez 30 s powoduje restart i decyzję rollback w bootloaderze.
Przed migracją produkcyjnego node nadal obowiązuje pełna brama HIL, łącznie z
wymuszonym brakiem confirm.

Narzędzie migracyjne musi tymczasowo znać trzy kontrakty, ale każdy w jednej
ściśle ograniczonej fazie:

- framing 2 wyłącznie do wejścia ze starej aplikacji do bootloadera;
- bootloader update protocol 2 do uploadu;
- framing 3 do weryfikacji i zatwierdzenia trial.

Nie oznacza to trwałego dual-stack w recorderze ani w nowej aplikacji. Po
zatwierdzeniu framing 3 wszystkie kolejne wejścia do bootloadera odbywają się
komendą framing 3, a sam bootloader może nadal używać update protocol 2.

Istnieje również ścieżka awaryjna: po fizycznym restarcie bootloader przez 5 s
przyjmuje tekstowe `UPDATE\r\n`, a następnie obsługuje update protocol 2. Bez USB
wymaga to możliwości zdalnego lub lokalnego odłączenia zasilania i wysłania
komendy w tym oknie. Tę ścieżkę trzeba sprawdzić na tym konkretnym node przed
migracją. Nie zastępuje ona host-confirmed trial.

## 11. Źródła

- Modbus Organization, [*MODBUS over Serial Line Specification and
  Implementation Guide V1.02*](https://www.modbus.org/docs/Modbus_over_serial_line_V1_02.pdf).
- W. Simpson, [IETF RFC 1662, *PPP in HDLC-like
  Framing*](https://www.rfc-editor.org/rfc/rfc1662.html).
- S. Cheshire, M. Baker, [*Consistent Overhead Byte
  Stuffing*](https://stuartcheshire.org/papers/COBSforToN.pdf), IEEE/ACM
  Transactions on Networking 7(2), 1999.
- Texas Instruments, [SLLA272D, *The RS-485 Design
  Guide*](https://www.ti.com/lit/an/slla272d/slla272d.pdf).
- Texas Instruments, [SNLA049B, *Ten Ways to Bulletproof RS-485
  Interfaces*](https://www.ti.com/lit/pdf/snla049).
- Analog Devices, [AN-960, *RS-485/RS-422 Circuit Implementation
  Guide*](https://www.analog.com/en/resources/app-notes/an-960.html).
- FTDI, [AN232B-04, *Data Throughput, Latency and
  Handshaking*](https://ftdichip.com/wp-content/uploads/2024/03/AN232B_04_DataThroughput_LatencyandHandshaking.pdf).
- P. Koopman, T. Chakravarty, [*Cyclic Redundancy Code Polynomial Selection for
  Embedded Networks*](https://users.ece.cmu.edu/~koopman/roses/dsn04/koopman04_crc_poly_embedded.pdf),
  DSN 2004.
- IETF [RFC 3385, *iSCSI CRC/Checksum
  Considerations*](https://www.rfc-editor.org/rfc/rfc3385.html).
- Raspberry Pi, [*RP2350
  Datasheet*](https://datasheets.raspberrypi.com/rp2350/rp2350-datasheet.pdf),
  DMA sniffer checksum modes.
