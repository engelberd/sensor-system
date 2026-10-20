# Telemetry Service — application protocol 5

- Status: kontrakt, node, wire i trwały runtime hosta gotowe; pozostał HIL i prezentacja operatorska
- Service ID: `9`
- Message ID: `1` (`GET_SNAPSHOT`)

## Cel i granica odpowiedzialności

Telemetry przenosi rzadkie, wolnozmienne obserwacje fizyczne node. Pierwszym
polem jest wewnętrzna temperatura ADXL355. W przyszłości ten sam service może
otrzymać temperaturę innego sensora, napięcie zasilania albo inne wolne pomiary.

Telemetry nie przenosi lifecycle, konfiguracji, cursorów, gapów, liczników
błędów ani wynikowego Health. Błąd odświeżenia snapshotu jest faktem dla
Diagnostics/Health, a nie dodatkowym polem pomiarowym udającym poprawną wartość.

## Własność i gotowość odpowiedzi

Core 0 jest jedynym właścicielem dostępu do sensora. Odświeża mały, statyczny
cache poza krytyczną obsługą FIFO. Pierwszy odczyt następuje po poprawnej
inicjalizacji sensora, a następne początkowo nie częściej niż raz na minutę.

Host pobiera snapshot domyślnie co 10 minut; interwał jest konfigurowalny i
może zostać skrócony do 5 minut. Polling node na wspólnym channel jest
przesunięty w fazie. Request protocol 5 nigdy nie uruchamia synchronicznego
odczytu SPI: core 1 zwraca już opublikowany cache, dlatego koszt i czas
transakcji są stałe.

Jeżeli po boot nie powstał jeszcze poprawny snapshot, handler odpowiada
`BUSY`. Nie publikuje zera ani wartości zastępczej. Po błędzie kolejnego
odświeżenia zachowuje ostatni poprawny snapshot. Host rozpoznaje jego wiek po
czasie urządzenia i może oznaczyć go jako stale; recording XYZ trwa dalej.

## `GET_SNAPSHOT`

Request nie ma payloadu. Adres node i `transaction_id` znajdują się w nagłówku
framing v3. Operacja jest `RetrySafe`, ponieważ nie zmienia stanu node.

Success response schema 1 ma dokładnie 14 bajtów. Wszystkie liczby są
little-endian.

| Offset | Rozmiar | Pole |
| ---: | ---: | --- |
| 0 | 1 | `ResponseStatus::Ok` |
| 1 | 1 | `telemetry_schema = 1` |
| 2 | 8 | `captured_device_time_us` |
| 10 | 2 | `valid_fields` |
| 12 | 2 | `adxl355_temperature_raw` |

`captured_device_time_us` jest czasem zakończenia przygotowania całego
snapshotu na Pico. Wszystkie pola jednego snapshotu dzielą ten czas. Dla
telemetrii odświeżanej co 5–10 minut osobne timestampy pól nie poprawiłyby
użytecznej dokładności, a zwiększałyby payload i stan.

W schema 1 bit 0 `valid_fields` oznacza poprawną temperaturę ADXL355. Pozostałe
bity muszą być zerem. Success wymaga ustawionego bitu 0 i poprawnego surowego
kodu. Odpowiedź błędu zawiera dokładnie jeden bajt statusu.

Wire zachowuje wyłącznie 12-bitowy kod źródłowy w `u16`. Celsius jest wartością
pochodną obliczaną przez host według wersjonowanego modelu ADXL355. Firmware nie
przesyła binarnego `float`.

`boot_epoch` nie jest powtarzany w snapshotcie. Host bierze go z aktywnej,
zweryfikowanej sesji Identity. Trwała tożsamość obserwacji to co najmniej
`hardware_id + boot_epoch + captured_device_time_us`.

Host zapisuje każdy nowy snapshot do osobnego Telemetry WAL. Jeden rekord ma
16 B: `captured_device_time_us`, surowy kod temperatury, pole zarezerwowane i
CRC32C. Strumień jest identyfikowany przez hardware ID, adres node oraz
`boot_epoch`; ponowne pobranie tego samego cache jest idempotentne. I/O należy
do bounded storage workera i nie może wykonywać się w pętli RS485.

Wszystkie kanały korzystają z jednego właściciela Telemetry WAL. Każdy worker
kanału ma jedynie klon ograniczonego submittera. Odpowiedź jest rozliczona jako
ukończona dopiero po przyjęciu rekordu do kolejki; backpressure zwalnia RS485 i
planuje retry. Błąd trwałego zapisu jest obserwowany przez główny nadzór i
powoduje kontrolowany stop. Po zmianie `boot_epoch` rollover Telemetry, Time i
Capture kończy się przed wznowieniem nagrywania.

## Poprawność odczytu ADXL355

Sterownik jest granicą zaufania dla wartości sprzętowej. Odczyt temperatury:

1. jawnie włącza sensor temperatury i weryfikuje `POWER_CTL` po resecie;
2. składa kod 12-bitowy z `TEMP2/TEMP1`;
3. odrzuca wartości poza szerokim fizycznie sensownym zakresem;
4. wymaga dwóch zgodnych odczytów z maksymalnie trzech prób;
5. zwraca `InvalidSample` zamiast liczby po nieudanej walidacji.

Zapobiega to publikacji `raw = 0`, które według typowego wzoru ADXL355 dawało
około 233 °C. Cache jest aktualizowany wyłącznie wynikiem `SensorStatus::Ok`.

## Rozszerzanie

Schema 1 pozostaje mała i stała. Dodanie nowych pól wymaga nowego schema z
jawnym układem i capability, a nie niejawnego wydłużenia odpowiedzi. Nie
budujemy obecnie dynamicznego rejestru sensorów, TLV ani alokacji na heapie.

Następne możliwe pola to temperatura MCU, temperatura zewnętrzna lub napięcie
zasilania. Trafią tu tylko wtedy, gdy istnieje wiarygodne źródło sprzętowe i
konkretny przypadek użycia.
