# Configuration i Commissioning Service — application protocol 5

- Status: kontrakt implementowany
- Wersja schematu konfiguracji: 1
- Transport: framing 3, request/response inicjowane wyłącznie przez host

## Granice odpowiedzialności

Configuration Service (`service=5`) jest właścicielem ustawień toru
pomiarowego. Commissioning Service (`service=8`) jest właścicielem pierwszego
przypisania adresu node. Adres, baudrate i ustawienia pomiaru nie tworzą jednej
mutacji.

Baudrate pozostaje obecnie stałe `115200`. Jego przyszła zmiana wymaga osobnej
procedury migracji łącza z potwierdzeniem na starej i nowej prędkości. Zwykłe
`ApplyConfig` nie może odciąć urządzenia od magistrali.

## Configuration Service

Wartość konfiguracji ma 12 bajtów:

| Offset | Rozmiar | Pole |
| ---: | ---: | --- |
| 0 | 1 | schema, obecnie `1` |
| 1 | 1 | output rate code po całym przetwarzaniu |
| 2 | 1 | zakres ADXL355: `2`, `4` albo `8 g` |
| 3 | 1 | HPF corner: `0..7` |
| 4 | 1 | profil filtra: light/balanced/aggressive |
| 5 | 1 | FIFO watermark: `3..96`, wielokrotność 3 |
| 6 | 2 | offset X, signed little-endian `i16` |
| 8 | 2 | offset Y, signed little-endian `i16` |
| 10 | 2 | offset Z, signed little-endian `i16` |

Host wyraża żądany **output ODR**. Node zwraca też observed sensor ODR i
decimation factor. Dla obecnego pipeline współczynnik wynosi 2, a wspierane
output ODR to `2000, 1000, 500, 250, 125, 62.5 Hz`. Pełny katalog kodów nadal
obejmuje wszystkie częstotliwości ADXL355; nieobsługiwana kombinacja daje
wynik walidacji zamiast cichego zaokrąglenia.

| Message | ID | Request | Odpowiedź |
| --- | ---: | --- | --- |
| GetConfig | 1 | puste | revision + desired + observed ODR/decimation |
| ValidateConfig | 2 | pełna wartość | validation code + konkretne pole |
| ApplyConfig | 3 | operation ID + expected revision + pełna wartość | wynik + nowy observed snapshot + effective sample sequence |
| ResetConfig | 4 | operation ID + expected revision | taki sam wynik jak ApplyConfig |

`GetConfig` i `ValidateConfig` nie mają skutków ubocznych. `ApplyConfig` i
`ResetConfig` są dozwolone wyłącznie w Maintenance. `expected_revision`
realizuje optimistic concurrency: stary odczyt nie może nadpisać nowszej
konfiguracji. Retry zachowuje `operation_id`, ale każda próba wire otrzymuje
nowy `transaction_id`.

Core 1 dekoduje protokół i utrzymuje bounded cache wyników. Jedynym wykonawcą
operacji ADXL, flash i zmiany `config_revision` jest core 0. Kolejność mutacji:

1. ponowna walidacja i kontrola trybu/revision;
2. zastosowanie kompletnego ustawienia w sensorze;
3. trwały zapis redundantnej konfiguracji;
4. publikacja nowej revision dla kolejnych próbek.

Błąd zapisu uruchamia próbę przywrócenia poprzedniego ustawienia sensora.
Niepewny rollback ma własny wynik i nie może zostać przedstawiony jako sukces.
Reset zachowuje node address i parametry łącza; resetuje tylko ustawienia
pomiarowe.

## Adresowanie commissioning

Framing 3 używa następujących adresów:

- `0`: host;
- `1..253`: trwałe adresy node;
- `254`: tymczasowe źródło odpowiedzi niezaadresowanego node;
- `255`: broadcast request.

Niezaadresowany node przechowuje wewnętrznie `node_id=0`, ale na wire odpowiada
ze źródła `254`. Zwykły Identity Service nie akceptuje 0 ani 254 jako trwałej
tożsamości.

## Bezkolizyjne discovery

`ProbeSlot` (`message=1`) zawiera:

- niezerowy `round_nonce` `u64`;
- `slot_count` jako potęgę dwóch w zakresie `1..256`;
- `slot_index < slot_count`.

Każdy niezaadresowany node liczy:

```text
slot = CRC32C(round_nonce_le || hardware_id) mod slot_count
```

Host odpytuje sloty kolejno. Odpowiada tylko node pasujący do bieżącego slotu.
Zero urządzeń daje timeout. Dwa urządzenia w tym samym slocie powodują brak
poprawnej ramki; host powtarza rundę z nowym nonce lub większą liczbą slotów.
Node nigdy nie nadaje bez requestu hosta i nie używa lokalnego timera do
samodzielnego zajmowania magistrali.

Odpowiedź ProbeSlot zawiera nonce, hardware ID, board revision, firmware oraz
wersję framing/application. Host akceptuje wyłącznie dokładnie skorelowaną
odpowiedź ze źródła 254.

## Przypisanie adresu

`AssignAddress` (`message=2`) jest broadcastem zawierającym `operation_id`,
nonce rundy, dokładne hardware ID i nowy adres `1..253`. Wszystkie pozostałe
node milczą. Pasujący node zapisuje adres w redundantnej konfiguracji i w
metadanych bootloadera na core 0. Powtórzenie requestu z tym samym operation ID
zwraca wynik z cache; inne parametry z tym samym ID dają Conflict.

Restart jest uzbrajany dopiero po potwierdzeniu przez transport, że odpowiedź
AssignAddress została fizycznie nadana. Po restarcie host wykonuje zwykły
Identity/Capabilities handshake pod nowym adresem i porównuje hardware ID.
Brak odpowiedzi na przypisanie jest stanem niepewnym: host najpierw sprawdza
docelowy adres, a dopiero potem ponawia commissioning.

Commissioning działa tylko dla `node_id=0`. Zaadresowany node ignoruje
broadcasty service 8. Operacja wymaga wyłącznego okna serwisowego na kanale;
nie jest planowana pomiędzy produkcyjnymi pullami próbek.

## Otwarte rozszerzenia

- osobna, odzyskiwalna migracja baudrate;
- polityka hosta dobierająca początkowy i maksymalny `slot_count`;
- pełny HIL kilku jednocześnie niezaadresowanych node;
- API operatorskie orkiestrujące rundę, przypisanie i końcową weryfikację.
