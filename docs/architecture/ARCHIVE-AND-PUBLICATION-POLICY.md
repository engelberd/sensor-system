# Polityka Archive i publikacji

- Status: decyzja produktowa zamrożona; format binarny pozostaje warunkowy
- Ostatnia aktualizacja: 2026-10-08

## Cel

Archive jest zweryfikowanym, skompresowanym produktem przeznaczonym do
długoterminowego przechowywania i wysyłania na zewnętrzny serwer. Nie leży na
gorącej ścieżce recordera. Awaria kompresji, sieci, serwera albo strony nie może
zatrzymać pobierania próbek ani przesunąć durable cursora node.

## Jednostka archiwizacji

Domyślna jednostka to jeden sensor i jedna godzina UTC. Granica godziny jest
partycją danych, a nie deklaracją, że próbka została pobrana dokładnie o pełnej
godzinie. Produkt zachowuje rzeczywisty czas pierwszej i ostatniej próbki,
źródłowe timing anchors, niepewność modelu czasu, cursory oraz jawne luki.

Capture nadal używa krótszych, konfigurowalnych okien, domyślnie około 10 minut
od pierwszej zapisanej próbki. Archive może więc złożyć fragmenty kilku Capture,
a pojedynczy Capture może dostarczyć dane do dwóch sąsiednich godzin. Granice
plików Capture i porcje transmisji RS485 nie są granicami pomiaru.

Jeżeli nie ma wiarygodnego mapowania na UTC, system nie przypisuje danych do
zmyślonej godziny. Tworzy produkt oznaczony jako `time_unresolved`, identyfikowany
przez hardware ID, boot epoch i zakres cursorów. Po odzyskaniu dowodów czasu
może powstać nowa, jawnie powiązana rewizja produktu godzinowego.

## Kiedy godzina jest gotowa

Archiwizator sprawdza pracę okresowo, ale zamyka produkt dopiero, gdy:

1. zakończyła się dana godzina UTC;
2. wszystkie wejściowe Capture są zapieczętowane i zweryfikowane;
3. durable watermark sensora przeszedł koniec godziny;
4. cały zakres jest pokryty próbkami, jawnymi `Gap` albo
   `ContinuityBreak`;
5. model czasu ma jakość wystarczającą do przypisania próbek do UTC.

Nie stosujemy sztywnego opóźnienia jako substytutu watermarku. Domyślny
harmonogram może obudzić archiwizator kilka minut po pełnej godzinie i ponawiać
pracę, lecz zaległy node opóźnia swój produkt zamiast powodować cichą utratę.
Pozostałe sensory mogą zostać zarchiwizowane niezależnie.

## Niezmienność i korekty

Zatwierdzony Archive jest niezmienny. Ma stabilny `product_id`, wersję formatu,
zakres czasu i cursorów, liczbę próbek i luk oraz hash zawartości. Rzadkie późne
uzupełnienie nie nadpisuje opublikowanego pliku: tworzy kolejną rewizję z polem
`supersedes`, a serwer przełącza aktywną rewizję idempotentnie.

Archive nie interpoluje brakujących próbek. Strona i analiza mogą osobno
zdecydować o wizualizacji albo estymacji, korzystając z jawnych informacji o
lukach.

## Publikacja na serwer

Kompakcja i wysyłka są dwiema operacjami:

```text
sealed Capture -> validate -> compact -> verify Archive -> publication outbox
publication outbox -> upload -> server acknowledgement -> replicated
```

Publisher czyta wyłącznie zweryfikowane Archive. Używa `product_id` i hasha jako
klucza idempotencji, ma ograniczoną kolejkę, retry z backoff i trwały stan
wysyłki. Timeout odpowiedzi może spowodować ponowienie tej samej publikacji, ale
nie duplikat danych na serwerze. Potwierdzenie serwera oznacza, że serwer
zweryfikował cały produkt, nie tylko odebrał żądanie.

Strona nie czyta Capture bezpośrednio. Serwer aktualizuje swój indeks i dane
strony po zaakceptowaniu Archive. Docelowa świeżość danych wynosi około jednej
godziny plus czas domknięcia, kompresji i wysyłki. Podgląd bardziej bieżący,
jeśli kiedyś będzie potrzebny, powinien być osobnym strumieniem telemetrycznym,
bez zmiany gwarancji Archive.

## Retencja i zasoby

- Capture można usunąć dopiero po walidacji Archive i osiągnięciu wymaganej
  polityką liczby potwierdzonych kopii.
- Brak sieci nie zatrzymuje recordera; zwiększa lokalną kolejkę publikacji i
  jest widoczny w health.
- Brak miejsca uruchamia zdefiniowaną politykę alarmu/backpressure/stop, nigdy
  ciche usuwanie nieopublikowanych danych.
- Compactor domyślnie używa jednego workera, maksymalnie konfigurowalnych dwóch,
  i przetwarza dane blokami ze stałym limitem RAM.
- Wiele sensorów nie powoduje automatycznego uruchomienia workera dla każdego z
  nich; zadania przechodzą przez ograniczoną kolejkę.

## Konfiguracja

Poniższe wartości są polityką, a nie stałymi zaszytymi w algorytmie:

- `archive_partition_duration`: domyślnie 1 h;
- częstotliwość sprawdzania gotowych partycji;
- minimalna jakość czasu wymagana dla partycji UTC;
- liczba workerów i rozmiar bloku;
- retry/backoff publikacji;
- wymagana liczba replik przed usunięciem Capture;
- lokalna retencja Archive.

Domyślna polityka wynosi 48 h bufora Capture i 30 dni lokalnego Archive od
potwierdzonej replikacji. Jej trwałą maszynę stanu opisuje
[RETENTION-CHECKPOINT.md](RETENTION-CHECKPOINT.md).

Format fizyczny pozostaje do potwierdzenia na rzeczywistych danych. Aktualnym
kandydatem jest blokowy int32 byte-shuffle + zstd z inkrementalnym hashem i
walidacją w stałej pamięci.
