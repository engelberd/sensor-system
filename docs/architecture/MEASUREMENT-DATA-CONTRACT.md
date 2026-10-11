# Minimalny kontrakt danych pomiarowych

Status: część zamrożonego `architecture-v1` baseline.

Kontrakt opisuje semantykę źródłowych danych. Nie narzuca wire format, HDF5,
kompresji ani reprezentacji klas w Pythonie i C++.

## 1. Tożsamość próbki

Logiczna tożsamość próbki to:

```text
(hardware_id, boot_epoch, sample_seq)
```

Minimalna zawartość pomiarowa:

```text
sample_seq
raw_x
raw_y
raw_z
```

`raw_x`, `raw_y` i `raw_z` są znakowanymi liczbami całkowitymi bez konwersji do
jednostek fizycznych. Dla strumienia bez przetwarzania są kodami sensora. Jeżeli
node wykonuje zadeklarowany filtr i decymację, są dokładnymi wynikami tego
potoku w jednostkach kodu sensora. Konfiguracja musi jednoznacznie odróżniać te
dwa przypadki. Konwersja do `m/s²`, kalibracja i resampling w systemie
analitycznym nie zmieniają źródłowego zapisu.

`sample_seq` jest wymagany semantycznie, ale nie musi być fizycznie powtarzany
przy każdej próbce w pliku. Ciągły segment może zawierać:

```text
boot_epoch
first_sample_seq
sample_count
raw_xyz[sample_count][3]
```

Numer próbki o indeksie `i` wynosi wtedy `first_sample_seq + i`. Luka, restart
albo nieciągłość konfiguracji kończy segment. Taki układ zachowuje tożsamość i
upraszcza kompresję tablicy XYZ.

Natywny kod ADC ADXL355 jest 20-bitową liczbą ze znakiem. Wynik filtra FIR może
wyjść poza natywny zakres, dlatego kontrakt rozróżnia:

- `Adxl355SampleCode` — walidowany signed-20 na granicy sterownika;
- `SampleAxis` — walidowany signed-24 wynik źródłowego potoku node;
- wire — trzy bajty na oś, bez floata i bez skalowania;
- storage — prosty kontener `int32` na oś, zachowujący dokładną wartość wire.

Szerokość kontenera storage nie oznacza dodatkowej rozdzielczości pomiaru.
Saturacja podczas filtrowania lub kodowania jest zdarzeniem jakościowym i nie
może następować po cichu.

## 2. Ciągłość i luki

Dokładna luka jest źródłowym faktem, nie komunikatem debugowym:

```text
Gap
  boot_epoch
  first_missing_seq
  missing_count
  reason
```

`reason` może pozostać `Unknown`, jeśli system zna zakres, ale nie potrafi
wiarygodnie wskazać przyczyny. Rozróżniamy co najmniej utratę w sensorze,
przepełnienie journala i nieznaną nieciągłość.

Jeżeli system potrafi dowieść utraty ciągłości, ale nie zna wiarygodnej liczby
brakujących próbek wyjściowych, zapisuje osobny quality event:

```text
ContinuityBreak
  boot_epoch
  at_cursor
  reason
  timing evidence, jeśli dostępne
```

`ContinuityBreak` rozpoczyna nowy ciągły segment. Nie jest gapem długości jeden
i nie zawiera estymowanego `missing_count`. Wewnętrznie zajmuje jeden element
cursor space wyłącznie po to, aby podlegać retencji, retransmisji i durable
commit. Szczegóły zamraża
[ADR-0010](../adr/0010-ordered-quality-events-and-service-observability.md).

Recorder:

- nie tworzy syntetycznych próbek;
- nie interpoluje luki;
- nie przesuwa próbek, aby ukryć brak;
- rozpoczyna nowy ciągły segment po luce albo `ContinuityBreak`;
- zachowuje wystarczające dane, aby system analityczny mógł odrzucić fragment,
  pozostawić maskę braku albo świadomie wykonać interpolację.

Interpolowana wartość należy do Derived data i zawsze musi zachować maskę lub
provenance odróżniające ją od rzeczywistego pomiaru.

## 3. Minimalne dane czasu

Nie zapisujemy wymyślonego czasu wynikającego z nazwy pliku lub granicy pełnej
minuty. Zachowujemy:

- timestamp urządzenia powiązany z rzeczywistą próbką lub segmentem;
- `sample_seq`, którego dotyczy timing anchor;
- `boot_epoch` i identyfikator segmentu zegara;
- obserwacje synchronizacji potrzebne do mapowania device time na host/UTC;
- oszacowaną niepewność albo stan `Unknown/Invalid`.

Timestamp UTC nie musi być zapisany dla każdej próbki. Może być rekonstruowany z
rzadkich timing anchors i kolejności próbek. Dokładna częstotliwość anchorów oraz
algorytm estymacji powstaną z modułem clock model.

Plik publikuje co najmniej:

- oszacowany czas rzeczywistej pierwszej próbki;
- oszacowany czas rzeczywistej ostatniej próbki;
- nominalny output ODR z konfiguracji;
- zaobserwowany/effective ODR, jeśli istnieją wystarczające dowody czasu;
- jakość lub niepewność oszacowania.

Filename i docelowa długość okna nie są dowodem czasu próbki.

## 4. Okna plików

Pliki są przybliżonymi oknami czasu akwizycji, aby ograniczać rozmiar, czas
walidacji i blast radius uszkodzenia.

Polityka:

```text
target_window_duration_s = 600  # wartość domyślna, konfigurowalna
```

- okno zaczyna się od pierwszej rzeczywiście zapisanej próbki;
- nie jest wyrównywane do pełnej minuty, sekundy ani granicy UTC;
- recorder zamyka je na granicy rzeczywistej próbki po osiągnięciu docelowego
  czasu akwizycji;
- rzeczywisty czas i liczba próbek mogą nieznacznie różnić się od celu;
- metadata przechowują realnie oszacowane czasy pierwszej i ostatniej próbki;
- zmiana długości okna jest ustawieniem systemu/instalacji, nie stałą w kodzie;
- krótki plik po starcie, zatrzymaniu lub recovery jest legalny i nie jest
  sztucznie dopełniany.

Granica okna nie zużywa pozycji strumienia. Każdy zamknięty plik publikuje
manifest z zakresem półotwartym:

```text
(hardware_id, boot_epoch, window_ordinal, first_cursor, end_exclusive)
```

Sąsiednie okna tego samego boot epoch są składane wyłącznie wtedy, gdy
`previous.end_exclusive == next.first_cursor` i ordinal jest kolejny. Wartość
większa oznacza nieopisaną utratę, a mniejsza duplikat lub nakładanie. Obie
sytuacje są błędem walidacji. Jawny `Gap` pozostaje rekordem w cursor space, więc
plik po luce nadal może być bezstratnie dołączony, a analiza widzi dokładny
brakujący zakres. `ContinuityBreak` zachowuje swoją pozycję i semantykę.

Recorder nie dzieli transferowego chunku podczas rotacji. Najpierw utrwala cały
uporządkowany rekord, następnie przesuwa durable cursor, a manifest zamkniętego
okna publikuje dopiero po tej granicy trwałości. Awaria storage pozostawia plan
rotacji niezatwierdzony i nie może przesunąć początku kolejnego pliku.

Pliki jednego sensora nie muszą zaczynać się w tej samej chwili co pliki innych
sensorów. Późniejsza analiza używa czasu próbek i segmentów, nie nazw plików.

## 5. ODR i decymacja

ADXL355 udostępnia następujące nominalne sensor ODR:

```text
4000, 2000, 1000, 500, 250, 125, 62.5, 31.25,
15.625, 7.8125, 3.90625 Hz
```

Pełny zakres musi być reprezentowalny w konfiguracji i protokole, nawet jeśli
admission control odrzuci ustawienie niemożliwe do obsłużenia w Production.
Pierwsze wspierane docelowe output ODR po decymacji node to:

```text
250 Hz
125 Hz
62.5 Hz
```

Wartość wybiera operator lub konfiguracja instalacji na podstawie liczby node,
długości przewodu, baudrate i potrzeb pomiaru. Pierwsza implementacja nie zmienia
ODR automatycznie podczas recording.

Admission control sprawdza wybraną konfigurację i odmawia startu, gdy dane,
synchronizacja i margines retry nie mieszczą się w budżecie channel.

ODR reprezentuje `SampleRate` jako dodatnią liczbę całkowitą nanohertz. Pozwala
to dokładnie zapisać wszystkie ODR ADXL355, ich częstotliwości graniczne LPF
oraz dokładne wyniki obsługiwanej decymacji bez używania floata. Operacja, której
wynik nie mieści się dokładnie w tej reprezentacji, zwraca błąd. Wyliczenie
okresu zwraca wynik tylko wtedy, gdy okres jest dokładną całkowitą liczbą
nanosekund.

Źródłowy zapis zachowuje:

- nominalny output ODR;
- sensor ODR, filtr sensora, profil filtra cyfrowego i współczynnik decymacji;
- effective ODR wyznaczony z dowodów czasu, jeśli jest dostępny.

## 6. Transfer nie jest modelem danych pomiarowych

`TransferChunk` lub wire batch jest porcją komunikacji. Może zmienić rozmiar,
zostać retransmitowany albo zostać podzielony przez inny transport. Nie jest
utrwalanym pojęciem analitycznym.

Transaction ID, packet sequence, retry count i release response należą do runtime
i diagnostyki transportu. Nie są wymaganymi polami źródłowego strumienia
pomiarowego ani Archive.

Durable cursor i recovery metadata muszą zostać zapisane crash-safe, aby chronić
commit. Są wewnętrznym stanem recordera, a nie pomiarem przekazywanym do
zewnętrznej analizy.

`CaptureRepository` przyjmuje uporządkowane próbki, luki, timing anchors,
konfigurację i quality events. Może zapisywać je fizycznie w chunks dla
wydajności, ale chunk boundary nie zmienia semantyki danych.

## 7. Decyzje odłożone do właściwych modułów

Na tym etapie nie zamrażamy:

- HDF5 dataset layout ani innego formatu pliku;
- algorytmu i poziomu kompresji;
- liczby próbek w storage chunk;
- liczby próbek w wire transfer;
- częstotliwości timing anchors;
- szczegółów modelu zegara;
- nazw plików;
- fizycznego formatu i kompresji produktów Archive;
- zachowania UI przy luce.

Każda z tych decyzji musi zachować semantykę zdefiniowaną powyżej.

Polityka godzinnych produktów, watermarku oraz niezależnej publikacji jest
zamrożona w
[ARCHIVE-AND-PUBLICATION-POLICY.md](ARCHIVE-AND-PUBLICATION-POLICY.md).
