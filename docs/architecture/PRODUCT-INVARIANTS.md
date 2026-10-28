# Niezmienniki produktu Sensor System

Status: obowiązujące wymagania niezależne od implementacji.

Ten dokument opisuje, co system ma gwarantować. Nie wybiera języka, systemu
plików, biblioteki UART, modelu procesów ani konkretnego medium transmisji.

## 1. Cel produktu

System zbiera surowe pomiary czujników w możliwie najlepszej jakości, bez cichej
utraty danych i z dowodami pozwalającymi możliwie dokładnie odtworzyć czas każdej
próbki. Analiza sygnału, resampling, FFT i interpretacja pomiarów należą do
systemów zewnętrznych lub odtwarzalnych produktów pochodnych.

Źródłowy zapis nie może być zastępowany przez dane przeliczone dla wygody
bieżącej analizy.

## 2. Minimalny źródłowy zapis pomiaru

Źródło prawdy zachowuje:

- surowe wartości osi sensora bez konwersji do jednostek fizycznych;
- nieprzerwaną tożsamość strumienia: hardware/node, boot epoch i sample sequence;
- konfigurację potrzebną do interpretacji wartości, wraz z revision i granicą
  sekwencji, od której obowiązuje;
- surowe dowody czasu urządzenia i obserwacje synchronizacji potrzebne do
  późniejszego wyznaczenia czasu;
- luki, utratę w sensorze, restart, zmianę konfiguracji i unieważnienie czasu
  jako jawne quality events;
- informacje o kalibracji, jeśli zostanie zastosowana, wraz z revision i zakresem
  obowiązywania.

Nie wymagamy pełnego timestampu UTC przy każdej próbce, jeżeli można go
jednoznacznie odtworzyć z segmentu czasu, anchorów i kolejności próbek. Nie
powtarzamy dużych metadanych w każdej próbce tylko dla wygody czytnika.

Szczegółową minimalną semantykę próbki, luki, czasu, ODR i okna pliku definiuje
[MEASUREMENT-DATA-CONTRACT.md](MEASUREMENT-DATA-CONTRACT.md).

## 3. Czas i ODR

Źródłowym dowodem czasu są timestampy urządzenia związane z momentem pozyskania
próbki oraz obserwacje synchronizacji zegara node z hostem. Czas odbioru ramki
przez host nie jest czasem pomiaru.

System może publikować oszacowany timestamp UTC, efektywny ODR i niepewność jako
widok lub dane pochodne. Musi jednak zachować dane wejściowe, aby algorytm
estymacji można było poprawić bez ponownego wykonania pomiaru.

Każde oszacowanie czasu ma:

- identyfikator boot epoch i segmentu zegara;
- jawną niepewność lub stan `Unknown/Invalid`;
- informację o metodzie wyznaczenia;
- brak sztucznego wygładzania lub resamplingu źródłowych wartości.

Podstawowym wymaganiem jest najlepsze możliwe wyrównanie timestampów między
node. Fizycznie fazowe, jednoczesne próbkowanie nie jest obecnie wymaganiem. Jeśli
pojawi się później, będzie wymagało osobnego kontraktu acquisition i możliwe, że
wspólnego sygnału sprzętowego.

## 4. Dostarczenie bez cichej utraty

Każda próbka pozostaje własnością node, dopóki host nie potwierdzi jej trwałego
zapisu. Obowiązuje niezmiennik:

```text
released_cursor <= durable_cursor <= received_cursor
```

Każdy cursor jest granicą wyłączną: wartość `N` opisuje ciągły prefiks `[0, N)`.

Małe, przejściowe problemy są obsługiwane przez:

- wykrycie uszkodzonej lub niepełnej wiadomości;
- bounded timeout i retry;
- ponowne żądanie danych od znanego cursora;
- idempotentne przyjęcie duplikatu;
- przesunięcie durable cursor dopiero po flush/fsync równoważnym kontraktowi storage;
- scheduler uwzględniający backlog i czas do przepełnienia.

Node utrzymuje jeden ograniczony `MeasurementJournal` dla niepotwierdzonych
próbek i dokładnych luk.
Delivery odczytuje journal, lecz go nie posiada. Release nie może przekroczyć
durable cursor ani zakresu faktycznie dostarczonego hostowi w autoryzowanej
sesji.

Retencja jest ograniczona dostępną pamięcią node. System nie obiecuje braku strat
przy awarii dłuższej niż policzony czas retencji. Musi natomiast:

- raportować pojemność, backlog i szacowany `time_to_overflow`;
- odmówić konfiguracji, której średni napływ przekracza bezpieczny transfer;
- zapisać dokładny utracony zakres jako quality event po przepełnieniu;
- nigdy nie ukrywać utraty przez przesunięcie cursora lub zmianę boot epoch.

Docelowy czas retencji będzie parametrem wyznaczonym z dostępnej pamięci,
formatu próbki i output ODR, a następnie zweryfikowany testem na firmware.

### Dostępność kolejnych okien

Ochrona pojedynczego niejednoznacznego fragmentu nie może bezterminowo zatrzymać
przyszłych poprawnych próbek. Po ograniczonej próbie retransmisji i recovery
host zapisuje `Gap` albo `ContinuityBreak`, po czym wznawia Recording. Node,
który nie odpowiada, przechodzi do `Offline` dopiero po próbach wznowienia i
pozostaje objęty okresowym bounded probe. Zdrowe node na tej samej magistrali
nie mogą zostać przez to zagłodzone.

`RecoveryRequired` jest zdarzeniem uruchamiającym samonaprawę, nigdy trwałą
kwarantanną bez następnego deadline. Szczegóły zamraża ADR-0011.

## 5. Topologia i wymienny transport

Model produktu rozróżnia:

| Pojęcie | Znaczenie |
| --- | --- |
| `Host` | komputer koordynujący zbieranie |
| `TransportDevice` | fizyczny adapter, np. konwerter USB lub radio |
| `Channel` | niezależna domena planowania, własności i budżetu komunikacji |
| `Rs485Line` | fizyczna magistrala będąca jedną implementacją channel |
| `Node` | logiczne urządzenie pomiarowe obsługiwane w channel |
| `HardwareId` | niezmienna tożsamość konkretnego urządzenia |
| `NodeAddress` | adres routingu ważny w obrębie konkretnego channel |

Wielokanałowy konwerter USB jest jednym `TransportDevice` udostępniającym wiele
niezależnych channels. Awaria urządzenia może więc dotknąć grupy kanałów, ale
każdy channel nadal ma osobnego właściciela komunikacji i scheduler.

Logika `NodeSession`, cursorów, trwałego zapisu i quality events nie zależy od
RS485. Adapter transportu musi dostarczyć kontrakt host-controlled exchange:

```text
exchange(target, request, deadline) -> response/result
```

RS485 realizuje go przez half-duplex request/response. Inny adapter może użyć
USB, TCP, radia albo innego medium, jeśli zachowa:

- identyfikację peer;
- ograniczony rozmiar wiadomości;
- wykrywanie błędów i duplikatów;
- deadline/cancel;
- brak niejawnego równoległego właściciela channel;
- semantykę `NodeLink` wymaganą przez `NodeSession`.

Nie zakładamy, że każdy przyszły transport jest strumieniem bajtów ani że używa
adresu UART. Framing strumieniowy i turnaround są własnością adaptera RS485.
Application messages i reguły transferu pozostają niezależne.

## 6. Wiele node i polityka odpytywania

Host inicjuje komunikację produkcyjną. Jeden scheduler posiada channel i wybiera
kolejny node. Nie ma równoległych nadawców konkurujących o half-duplex RS485.

Scheduler:

- obsługuje wiele node na jednym channel;
- gwarantuje brak zagłodzenia sprawnych node;
- nadaje priorytet najmniejszemu `time_to_overflow`;
- uwzględnia output ODR, backlog, czas od ostatniej obsługi i koszt retry;
- ogranicza maksymalny czas jednego slotu;
- rezerwuje budżet na synchronizację oraz kompaktowe health;
- zmniejsza częstotliwość odpytywania node offline;
- raportuje wykorzystanie channel i zapas przepustowości.

Nie ustalamy jednej maksymalnej liczby node. Legalność konfiguracji zależy od
sumy produkowanych danych, narzutu medium i wymaganego marginesu recovery.

## 7. Operations API

Operations API jest interfejsem sterowania hostem dla CLI, UI i automatyzacji.
Nie jest protokołem host-node. Przyjmuje między innymi:

- start/stop recording;
- zmianę mode channel;
- konfigurację i commissioning node;
- odczyt snapshotów;
- rozpoczęcie update;
- operacje grupowe na wybranych channels.

Każda operacja ma `operation_id`, status, wynik per target i audit. API przekazuje
command właściwemu właścicielowi; samo nie otwiera portu ani nie wykonuje
recording.

Pierwsza wersja jest lokalna: działa na tym samym hoście i jest dostępna przez
lokalne IPC lub interfejs związany z loopback. UI uruchomione na tym hoście jest
klientem tego API. Dostęp zdalny, jeśli będzie potrzebny, powstanie jako
uwierzytelniony i szyfrowany adapter/gateway bez zmiany domeny.

## 8. Maintenance i aktualizacje

Czas serwisowy może zatrzymać pomiary. Nie wymagamy ciągłej dostępności podczas
konfiguracji, commissioning ani firmware update.

Wejście do Maintenance nadal jest kontrolowaną transakcją:

1. zatrzymanie nowych transferów;
2. dokończenie lub jawne anulowanie bieżącego slotu;
3. trwały flush i zapis cursorów;
4. zatrzymanie albo świadome pozostawienie acquisition na node;
5. wykonanie operacji serwisowej;
6. weryfikacja stanu, wersji i konfiguracji;
7. jawne ponowne uruchomienie recording.

Aktualizacja wielu urządzeń może zatrzymać całą wybraną instalację i przebiegać
sekwencyjnie. Prostota, możliwość wznowienia po utracie zasilania, walidacja
board revision, weryfikacja obrazu oraz rollback są ważniejsze niż dostępność.

## 9. Otwartość bez abstrakcji na zapas

Otwartość oznacza stabilne granice i wymienne adaptery. Nie oznacza jednej
uniwersalnej klasy transportu, sensora ani storage z dziesiątkami opcjonalnych
metod.

Nowy adapter jest dopuszczalny, gdy istniejący test kontraktowy `NodeLink` i
scenariusze failure/retry/commit mogą zostać uruchomione bez zmiany rdzenia.
Jeżeli nowe medium ma inne gwarancje, najpierw opisujemy je jako capability lub
nowy wąski port, zamiast dodawać warunki typu `if wireless` do `NodeSession`.
