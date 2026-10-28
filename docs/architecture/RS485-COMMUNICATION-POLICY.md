# Polityka komunikacji RS485

Status: obowiązująca polityka; parametry budżetów wymagają walidacji pomiarami.
Dokument przyjmuje
115200 bit/s, magistralę half-duplex i wiele node na jednej linii jako stałe
ograniczenia produktu.

Host obsługuje równolegle od 1 do 16 takich niezależnych linii. Ten dokument
opisuje budżet i zachowanie jednej linii; globalne zasoby hosta opisuje
[HOST-SCALING.md](HOST-SCALING.md).

## 1. Cel

Magistrala ma w pierwszej kolejności chronić dane pomiarowe. Synchronizacja czasu,
telemetria i operacje sterujące korzystają z osobnych budżetów oraz nie mogą
niekontrolowanie zabierać przepustowości recordingowi.

„Brak utraty danych” oznacza tutaj:

- każdy błąd możliwy do wykrycia jest wykrywany;
- niepotwierdzone dane pozostają na node i mogą zostać wysłane ponownie;
- duplikaty po ponowieniu są rozpoznawane przez host;
- dane są potwierdzane dopiero po trwałym zapisie;
- konfiguracja, dla której średni napływ przekracza bezpieczną przepustowość,
  nie może zostać uruchomiona;
- przepełnienie dowolnego bufora jest jawnym zdarzeniem jakościowym.

Nie oznacza to matematycznej gwarancji, że przewód nigdy nie ulegnie awarii.
Awaria dłuższa od retencji node musi zostać zgłoszona jako nieodwracalna luka.

## 2. Obecny budżet przewodu

UART 8N1 przesyła 10 bitów na każdy bajt protokołu. Przy 115200 bit/s surowy
limit wynosi 11520 bajtów/s.

W protocol 5 jeden `PULL_SAMPLES` z pełnym chunkem 48 próbek zajmuje w
najgorszym przypadku około 42 B requestu i 516 B response razem z COBS oraz
delimiterem. Przy początkowym turnaround 1 ms transakcja trwa około 49,4 ms.
Absolutny limit wynosi około 971 próbek/s bez timeoutów, synchronizacji,
telemetrii, filesystemu i zapasu recovery.

| Chunk | Czas transakcji z 1 ms turnaround | Teoretyczna przepustowość |
| ---: | ---: | ---: |
| 8 | 18,0 ms | 444 próbek/s |
| 16 | 24,3 ms | 659 próbek/s |
| 32 | 36,9 ms | 868 próbek/s |
| 48 | 49,4 ms | 971 próbek/s |

Przykładowe obciążenie protocol 5 przy maksymalnych chunkach:

| Konfiguracja jednej magistrali | Próbki/s | Użycie absolutnego limitu |
| --- | ---: | ---: |
| 4 node × 125 próbek/s | 500 | około 52% |
| 5 node × 125 próbek/s | 625 | około 64% |
| 2 node × 250 próbek/s | 500 | około 52% |
| 3 node × 250 próbek/s | 750 | około 77% |

Wartości powyżej 100% są niemożliwe, a okolice 80% nie dają rozsądnego miejsca
na odzyskiwanie po błędach. Większy bufor jedynie opóźnia przepełnienie, jeżeli
średni napływ jest większy od średniego transferu.

Obecny `MeasurementJournal` node ma 2048 próbek i daje około:

| Output ODR node | Retencja kolejki |
| ---: | ---: |
| 62,5 próbek/s | 32,8 s |
| 125 próbek/s | 16,4 s |
| 250 próbek/s | 8,2 s |

To przybliżenie trzeba mierzyć od momentu, w którym host przestaje zwalniać
pakiety. Osobny bufor akwizycji nie zwiększa automatycznie retencji kolejki
transportowej. Wymagany czas przeżycia awarii powinien wyznaczyć docelową pamięć
node, zamiast wynikać przypadkiem z bieżących stałych w kodzie.

## 3. Budżet i admission control

Każda konfiguracja linii musi zostać policzona przed startem recordingu:

```text
required_bps = suma(output_samples_per_second(node) * wire_bits_per_sample)
             + sync_budget
             + control_budget
             + diagnostic_budget
```

Na początek przyjmujemy konserwatywnie:

- cel produkcyjny: maksymalnie 60% nominalnej przepustowości;
- twardy limit przed testami terenowymi: 70%;
- minimum 20% pozostawione na ponowienia i odzyskiwanie zaległości;
- pozostały budżet na synchronizację, sterowanie i nieuniknione przerwy.

Te liczby są polityką startową, nie stałą protokołu. Testy na przewodzie docelowej
długości wyznaczą ostateczny limit. Recorder ma odmówić startu, jeśli policzona
konfiguracja przekracza limit profilu. Nie wystarczy ostrzeżenie.

Konsekwencja: przy 115200 bit/s liczba node i ich łączny output ODR są wspólnym
parametrem magistrali. Jeżeli wymagane jest osiem node po 125 próbek/s, potrzebne
będzie co najmniej jedno z poniższych:

- podział na więcej niż jedną magistralę;
- niższy output ODR;
- istotnie oszczędniejszy format przewodowy;
- kompresja o potwierdzonym najgorszym przypadku;
- świadomie mniejszy margines niezawodności, czego nie zalecamy.

## 4. Profile pracy magistrali

Profil dotyczy całej fizycznej linii, nie pojedynczego node. W danej chwili linia
ma jednego właściciela i jeden aktywny profil.

### `PRODUCTION_RECORDING`

Priorytety w kolejności:

1. odzyskanie danych zagrożonych przepełnieniem bufora node;
2. regularny transfer danych pomiarowych;
3. synchronizacja czasu;
4. kompaktowy stan zdrowia;
5. opcjonalne operacje tylko z niewykorzystanego budżetu.

W tym profilu:

- szczegółowy debug po RS485 jest wyłączony;
- konfiguracja wpływająca na pomiar jest zablokowana albo zaplanowana na jawną
  granicę sekwencji;
- restart, self-test, zmiana adresu, zmiana baudrate i firmware update są
  zabronione;
- zdrowie node jest raportowane kompaktowo, najlepiej w podsumowaniu burst;
- host nigdy nie odpytuje wszystkich liczników tylko dlatego, że UI jest otwarte.

### `DIAGNOSTIC_RECORDING`

Recording i jego gwarancje pozostają aktywne, ale konfiguracja musi zmieścić
dodatkowy budżet diagnostyczny. Profil umożliwia:

- częstsze statystyki transportu i buforów;
- szczegółowe obserwacje synchronizacji;
- śledzenie czasów request/response i zajęcia magistrali;
- opcjonalny zapis poprawnych oraz odrzuconych nagłówków ramek;
- snapshoty diagnostyczne i kontrolowane testy błędów.

Diagnostyka ma limit bajtów na sekundę. Po wzroście backlogu jest automatycznie
ograniczana. Jeżeli użytkownik wymaga pełnej diagnostyki, a budżet się nie zgadza,
start profilu zostaje odrzucony zamiast poświęcenia próbek.

### `MAINTENANCE`

Wyłączny tryb serwisowy bez aktywnej sesji recordingu. Obsługuje:

- commissioning i zmianę node ID;
- zmianę baudrate;
- trwałą konfigurację;
- self-test;
- restart i wejście do bootloadera;
- aktualizację firmware;
- rozbudowane zrzuty diagnostyczne.

Wejście do tego profilu jest transakcją: zatrzymanie nowych grantów, dokończenie
lub anulowanie bieżącego burst, trwały flush, zapis ostatniego cursora, określenie
polityki akwizycji node, a następnie przekazanie linii adapterowi serwisowemu.

## 5. Jeden właściciel magistrali

Recorder, konfigurator i updater nie powinny niezależnie otwierać tego samego
portu. Docelowo jeden `ChannelAgent` jest właścicielem fizycznej linii przez cały
czas działania systemu.

`ChannelAgent` zawiera:

- `BusScheduler` — wybiera następny node i przydziela czas magistrali;
- `RecordingSession` — waliduje, zapisuje i potwierdza partie;
- `TimeSyncService` — utrzymuje mapowanie zegarów;
- `HealthService` — zbiera kompaktowy stan;
- `MaintenanceCoordinator` — bezpiecznie przełącza profil i wykonuje operacje
  wyłączne;
- `ChannelControl` — przyjmuje lokalne polecenia start, stop, konfiguracja i
  update oraz zwraca ich wynik.

Supervisor i UI wysyłają polecenia do `ChannelAgent` przez lokalny interfejs
sterujący. Nie dotykają portu szeregowego. Konkretna technologia IPC może zostać
wybrana później; kontrakt i własność są ważniejsze niż socket czy plik komend.

## 6. Klasyfikacja poleceń

Każda komenda musi mieć jawnie przypisaną klasę, zamiast polegać na wiedzy UI.

### Bezpieczne podczas produkcji

- odczyt identity, wersji i capabilities;
- time sync;
- odczyt kompaktowego health summary;
- operacje protokołu danych wymagane przez recording.

Muszą mieścić się w budżecie i nie mogą blokować akwizycji.

### Bezpieczne na granicy danych

Zmiana ODR, zakresu, filtrów lub offsetów może być dopuszczona wyłącznie jako
zaplanowana transakcja:

1. host wybiera granicę sekwencji;
2. node stosuje zmianę atomowo;
3. node zwiększa `config_revision` i podaje `effective_sample_seq`;
4. recorder zapisuje nową konfigurację razem z granicą jej obowiązywania;
5. niejednoznaczny wynik zatrzymuje sesję do uzgodnienia stanu.

Pierwsza implementacja może po prostu zabronić tych zmian podczas recordingu.

### Wyłącznie maintenance

- zmiana node ID lub baudrate;
- save/load/reset konfiguracji;
- commissioning;
- self-test wpływający na pomiar;
- restart, bootloader i firmware update;
- czyszczenie diagnostyki, jeżeli utrudnia audyt sesji.

## 7. Znaczenie start i stop

Trzeba rozdzielić cztery różne operacje:

- `start_recording` — host zaczyna trwały zapis i transfer;
- `stop_recording` — host kończy transfer po bezpiecznym flush;
- `start_acquisition` — node zaczyna pobierać próbki z sensora;
- `stop_acquisition` — node przestaje pobierać próbki.

Słowo „start” bez zakresu jest niedopuszczalne w protokole sterującym.

Przy starcie recordingu wymagana jest jawna polityka cursora:

- `resume` — odbierz wszystko od ostatniego trwałego commitu;
- `from_oldest_available` — rozpocznij od najstarszych danych node i zapisz lukę,
  jeśli wcześniejsze dane już nie istnieją;
- `discard_backlog` — świadomie zatwierdź zastane dane bez zapisu, wraz z wpisem
  audytowym. Nie może to być domyślne zachowanie produkcyjne.

Przy zatrzymaniu trzeba jawnie określić, czy node nadal akumuluje dane. Jeżeli
akwizycja trwa, retencja node jest skończona i system powinien pokazywać czas do
przepełnienia.

## 8. Scheduler wielu node

Prosty round-robin jest przewidywalny, ale nie wystarcza, gdy node mają różne ODR
lub zaległości. Zalecany jest deadline-aware weighted round-robin:

1. Dla każdego node host zna output ODR, ostatni stan kolejki i czas ostatniej
   obsługi.
2. Host szacuje `time_to_overflow`.
3. Node z najmniejszym czasem do przepełnienia otrzymuje pierwszeństwo.
4. W normalnym stanie używany jest weighted round-robin proporcjonalny do
   produkcji danych.
5. Pojedynczy node ma maksymalny czas zajęcia magistrali, aby nie zagłodzić
   pozostałych ani time sync.
6. Node offline jest sprawdzany rzadziej i nie może zużywać całego budżetu na
   timeouty.
7. Niespójność strumienia uruchamia bounded recovery w tym samym schedulerze;
   nie usuwa node bezterminowo z rotacji.
8. Probe offline jest pojedynczą krótką próbą. Pełny handshake wykonuje się
   dopiero po oznace życia, a próby wielu node są fazowane. Domyślny interwał
   produkcyjny wynosi 30 s i pozostaje konfigurowalny; przy timeout 250 ms
   ogranicza to koszt całkowicie niemego node do mniej niż 1% czasu kanału.

Adaptacyjny rozmiar burst:

- mała zaległość: mały burst i mniejsze opóźnienie innych node;
- duża zaległość: większy burst i mniej narzutu sterującego;
- zawsze obowiązuje maksymalny czas jednego slotu magistrali;
- limit jest liczony w bajtach lub mikrosekundach przewodu, nie tylko w liczbie
  pakietów.

Scheduler zapisuje metryki: przydzielone bajty, rzeczywisty czas, retry, RTT,
backlog, minimalny `time_to_overflow` i wykorzystanie linii.

## 9. Polityka transferu danych

Obecny grant/commit ma dobrą własność: node nie usuwa danych przed potwierdzeniem.
Zachowujemy tę semantykę, ale rozdzielamy trwały zapis hosta od zgody na
zwolnienie retencji node.

Docelowy cykl może wyglądać tak:

```text
PULL_SAMPLES(node, boot_epoch, from, release_before, sample_budget)
  -> SAMPLE_CHUNK(actual_range, stream_state, timing, raw_xyz)
```

`release_before` pochodzi z poprzedniej trwale zapisanej partii. Dzięki temu
oddzielna komenda release nie jest potrzebna w każdym cyklu. Po utracie
odpowiedzi host ponawia ten sam cursor; release jest idempotentny. Node nigdy nie
usuwa danych nowszych niż przekazany durable cursor. Jeden request powoduje
najwyżej jedną odpowiedź.

Szczegółowy kontrakt znajduje się w
[MEASUREMENT-SERVICE.md](../protocol/MEASUREMENT-SERVICE.md).

Każda partia musi zawierać lub jednoznacznie dziedziczyć:

- node identity i `boot_epoch`;
- zakres `first_sample_seq..last_sample_seq`;
- format i liczbę próbek;
- `config_revision`;
- informację o stanie zegara lub odniesienie do segmentu czasu;
- ochronę integralności całej ramki.

Obowiązujący limit framingu v3 to 512 bajtów payload. Kontrakt Measurement
Service mieści do 48 surowych próbek XYZ24 w odpowiedzi z pełnym nagłówkiem
czasu. Nawet bardzo oszczędny format nie usuwa potrzeby admission control:
1000 próbek/s samych osi XYZ24 to 9000 B/s przed framingiem, requestami i
marginesem na retransmisje.

Kompresja może być dodatkiem, lecz limit przepustowości musi być liczony dla jej
najgorszego przypadku. Szum pomiarowy nie daje gwarancji dobrego współczynnika
kompresji.

## 10. Integralność i odzyskiwanie

CRC wykrywa uszkodzenie; nie naprawia go. Framing v3 używa CRC32C dla całej
ramki i wspólnych wektorów testowych host/node. Odzyskanie danych zapewniają
identyfikatory próbek, retencja oraz idempotentne ponowienie odczytu.

Wymagane mechanizmy:

- jawny numer wersji protokołu i capabilities;
- resynchronizacja dekodera po śmieciach na linii;
- numery ramek i próbek;
- `boot_epoch` zmieniany przy restarcie node;
- idempotentny commit;
- deduplikacja w repozytorium;
- ograniczone retry z priorytetem zależnym od czasu do przepełnienia;
- liczniki CRC, timeout, retry, resetów i przepełnień;
- fail-closed przy nieznanym formacie lub niezgodnej konfiguracji.

## 11. Synchronizacja czasu

Node oznacza próbki czasem urządzenia związanym z DRDY. Czas odbioru ramki na
hoście nie jest czasem próbki.

Polityka synchronizacji:

1. Nowy `boot_epoch` tworzy nowy model zegara.
2. Po starcie wykonujemy serię pomiarów synchronizacji i odrzucamy obserwacje z
   dużym RTT lub niespójnymi znacznikami.
3. W stanie zablokowanym synchronizacja działa rzadziej, przykładowo co 30 s;
   częstotliwość końcową ustalają pomiary dryfu.
4. Po pogorszeniu jakości lub restarcie częstotliwość chwilowo rośnie.
5. Scheduler gwarantuje maksymalne opóźnienie time sync; długi burst nie może
   blokować jej bez limitu.
6. Obserwacje synchronizacji są zapisywane jako dane źródłowe. Przeliczenie UTC
   może zostać poprawione offline.
7. Jakość czasu jest polem danych, a nie tylko alarmem.

Pełny 40-bajtowy opis czasu nie musi być powtarzany przy każdych 32 próbkach,
jeżeli ramka jednoznacznie wskazuje `boot_epoch`, segment i zakres sekwencji.
Projekt oszczędniejszego formatu musi jednak umożliwić odzyskanie po utracie
dowolnej pojedynczej ramki.

## 12. Warstwa fizyczna jako część wymagania

115200 bit/s na 400–800 m jest wymagającym założeniem i musi mieć własny test
odbiorczy. Protokół nie zastąpi poprawnej magistrali. Dokumentacja instalacji
powinna określać co najmniej:

- typ i impedancję przewodu;
- terminację obu końców oraz bias w jednym kontrolowanym miejscu;
- topologię liniową i maksymalną długość odgałęzień;
- uziemienie, ekranowanie, izolację galwaniczną i ochronę przepięciową;
- liczbę transceiverów i ich obciążenie;
- turnaround nadajników oraz czasy propagacji;
- warunki EMC i wspólny zakres napięć;
- miejsca, w których wymagany jest repeater lub podział segmentu.

Odbiór powinien obejmować test na pełnej długości i pełnej liczbie node, przy
docelowym ODR, przez czas pozwalający ocenić rzadkie błędy. Mierzymy BER pośrednio
przez CRC/error counters, retry, najgorszy RTT, backlog oraz minimalny margines
retencji.

## 13. Pytania wymagające decyzji produktowej

Przed zamrożeniem protokołu trzeba ustalić:

1. Maksymalną liczbę node na jednej fizycznej linii.
2. Wymagany output ODR każdego node i czy może różnić się między node.
3. Maksymalny akceptowany czas opóźnienia danych na hoście.
4. Czas awarii przewodu lub hosta, który node ma przetrwać bez utraty danych.
5. Zachowanie node po `stop_recording`: dalsza akwizycja, pauza czy zatrzymanie.
6. Czy zmiany ODR/range/filter podczas sesji są wymagane produktowo.
7. Czy diagnostyka ma działać równolegle z pełnym recordingiem, czy dopuszczamy
   osobną konfigurację o niższym ODR.
8. Czy aktualizacja jednego node może zatrzymać całą linię i na jak długo.
9. Jakie uszkodzenia uznajemy za dopuszczalne: zero niewykrytych błędów w
   zadanym okresie, maksymalny czas odtworzenia, maksymalna liczba luk.

Odpowiedzi są wejściem do obliczenia buforów, rozmiaru ramki i harmonogramu. Bez
nich nie da się uczciwie obiecać „nigdy nie tracimy danych”.
