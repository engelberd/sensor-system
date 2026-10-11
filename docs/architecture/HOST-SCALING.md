# Skalowanie hosta do 1–16 channels

Status: implementacja w toku; moduły pojedynczego channel, typowana topologia,
równoległy read-only executor discovery, admission plan i produkcyjny bootstrap
całej instalacji zachowujący własność każdego portu są gotowe. Gotowe są też
fail-closed uzgodnienie durable Capture z journalem node, ograniczona aktywacja
Recording i assembler coordinatora zachowujący odzyskany cursor. Faza
`HostStartupBlueprint` łączy kompletne inventory, endpointy, admission i jedną
topologię Time WAL przed wykonaniem skutków ubocznych. `start_host_runtime`
otwiera jeden globalny `TimeRuntime`, składa wszystkie produkcyjne pętle i
uruchamia niezależne bounded workery dopiero po przygotowaniu całego zestawu.
Startup ma rollback częściowo uruchomionych node, a globalny shutdown dołącza
wszystkie kanały przed zamknięciem Time WAL. `HostHealthSnapshot` izoluje fault
jednej linii, a błąd integralności Time albo potwierdzony brak miejsca/read-only
na wspólnym filesystemie uruchamia globalny kontrolowany Stop. Pozostaje
podłączenie runtime do procesu `run`.
Test kompozycyjny ośmiu kanałów potwierdza wspólny Time runtime, oddzielne
Capture executory i pełną kolejność Stop/drain/join/Time shutdown na rzeczywistym
kodzie framingu. Test przekroczenia rezerwy wspólnego storage potwierdza
globalny Stop; proces `run`, fault podczas rzeczywistego zapisu i dłuższy soak
pozostają kolejną bramką przed Raspberry Pi.
Obecna implementacja channel to linia RS485 działająca z 115200 bit/s i
obsługująca jeden lub wiele node. Channels mogą pochodzić z jednego konwertera
wieloportowego, kilku adapterów albo przyszłego transportu.

## 1. Hierarchia systemu

```text
HostCoordinator
  ├── ChannelAgent(channel-01 / RS485 line) ── node 1..N
  ├── ChannelAgent(channel-02 / RS485 line) ── node 1..N
  ├── ...
  └── ChannelAgent(line-16) ── node 1..N
```

Każdy `ChannelAgent` ma własny transport binding, scheduler, profil pracy,
modele zegarów i sesje node. Niezależne linie RS485 nie dzielą przepustowości
magistrali. Mogą jednak dzielić jeden `TransportDevice`, a wszystkie channels
dzielą CPU, RAM, dysk, zegar hosta, zasilanie adapterów i system operacyjny.

`HostCoordinator` nie uczestniczy w obsłudze pojedynczych ramek. Odpowiada za:

- konfigurację i cykl życia 1–16 agentów;
- globalny admission control CPU, RAM, storage i wymaganej rezerwy dysku;
- rozłożenie startu, synchronizacji i flush w czasie;
- wspólny stan zdrowia hosta;
- zatrzymanie wszystkich linii przy globalnym błędzie storage;
- izolację awarii pojedynczego channel oraz rozpoznanie wspólnej awarii
  `TransportDevice` obsługującego grupę channels;
- kontrolowane operacje grupowe, np. start lub stop całej instalacji.

## 2. Dwa poziomy admission control

Start każdej linii wymaga dwóch niezależnych decyzji.

### Budżet channel

`ChannelAgent` sprawdza, czy suma ODR node, format ramek, synchronizacja,
telemetria i rezerwa retry mieszczą się w 115200 bit/s zgodnie z
[polityką RS485](RS485-COMMUNICATION-POLICY.md).

### Budżet hosta i transport device

`HostCoordinator` sprawdza łączny koszt aktywnych channels oraz ograniczenia
współdzielonego `TransportDevice`:

```text
total_sample_rate = suma próbek/s ze wszystkich linii
storage_rate      = dane + timing + metadata + narzut formatu
cpu_budget        = decode + CRC + timing + kompresja + UI
memory_budget     = bufory ograniczone dla wszystkich linii
```

Uruchomienie siedemnastej linii jest błędem konfiguracji. Uruchomienie linii,
która mieści się na RS485, ale przekracza zmierzony budżet dysku lub CPU hosta,
również jest odrzucane.

## 3. Skala orientacyjna

Szesnaście linii po 115200 bit/s daje łącznie 1,8432 Mbit/s na przewodach, ale
nie tworzy jednej szybszej magistrali. Scheduler linii działa niezależnie.

Przykład: 16 linii, każda z 4 node po 125 próbek/s, daje 8000 próbek/s. Same
wartości XYZ24 zajmują:

```text
8000 * 9 B = 72 000 B/s
             około 6,22 GB/dobę przed metadanymi i narzutem formatu
```

To nie jest duży transfer sekwencyjny dla SSD, ale `flush/fsync` po burst na
wielu kanałach może tworzyć skoki opóźnienia. Test musi mierzyć najgorszy czas
trwałego zapisu, a nie tylko średnie MB/s.

Docelowe obliczenia storage muszą uwzględniać:

- timing i quality events;
- temperaturę i konfigurację;
- indeksy HDF5 lub przyszłego journala;
- kompresję i jej koszt CPU;
- pliki tymczasowe, archive i okres ich współistnienia z Capture;
- wymaganą retencję oraz rezerwę na awarię procesu archive.

## 4. Model wykonania

Preferowany pierwszy wariant:

- jeden proces Rust `sensor-host` z `HostCoordinator`;
- jeden logiczny `ChannelAgent` na aktywny channel jako nadzorowane zadanie;
- osobne ograniczone kolejki, cancellation i health per channel;
- archive i UI poza drogą krytyczną.

Nie mnożymy runtime'ów i bibliotek dla 16 linii bez dowodu, że daje to potrzebną
izolację. Blokujące operacje I/O muszą mieć timeout, a panic zadania kanału jest
raportowany i nadzorowany. Jeżeli sterownik jednego urządzenia może zablokować
cały proces, następną granicą izolacji jest `TransportDevice`, nie pojedynczy
node. Każdy agent nadal ma limity pamięci i nie może buforować bez końca.

Nie należy centralizować wszystkich próbek w jednej kolejce RAM tylko po to, aby
uprościć zapis. Taki komponent stałby się wspólnym punktem awarii. Można natomiast
wspólnie koordynować:

- rezerwę i stan dysku;
- harmonogram kosztownych flush;
- limity równoległej kompresji;
- publikację statusu;
- przejście całego hosta do bezpiecznego zatrzymania.

Implementowany `ChannelSupervisorSet` zachowuje tę granicę: per-channel task
wykonuje framing, scheduling i Capture, natomiast supervisor przechowuje tylko
mailbox `Stop` oraz stały snapshot fazy i licznika kroków. Zatrzymanie workera
wywołuje należący do tasku protokół shutdown; nie porzuca jego zasobów jako
pozornie zakończonego nagrania.

`ProductionChannelTask` realizuje ten protokół w kolejności: quiesce aktywnego
work, retry-safe Stop node do `Idle`, wymuszony drain Measurement każdego node
aż do potwierdzonego `NoData`, rozliczenie durable receipts i zamknięcie
Capture executor. Zwykłe deadline wypełnienia chunku są podczas drain pomijane,
ponieważ produkcja jest już zamrożona; backoff błędów i osobny deadline całej
fazy pozostają obowiązkowe.

## 5. Izolacja awarii

| Awaria | Oczekiwane zachowanie |
| --- | --- |
| Timeout lub odłączenie jednej linii | Pozostałe linie nagrywają; agent linii wykonuje retry do granicy retencji |
| Restart jednego node | Nowy `boot_epoch`; inne node tej linii i inne linie działają dalej |
| Błąd pliku jednej linii | Agent nie wykonuje dalszego COMMIT; coordinator ocenia, czy storage jest lokalnie czy globalnie uszkodzony |
| Brak miejsca / read-only filesystem | Wszystkie linie bezpiecznie zatrzymują commit i zgłaszają globalny błąd storage |
| Awaria UI lub archive | Recording działa dalej |
| Przeciążenie CPU | Najpierw ograniczana jest diagnostyka i UI; jeżeli droga danych nie mieści się w budżecie, start zostaje odrzucony albo recording zatrzymany jawnie |
| Awaria jednego portu adaptera | Dotyczy przypisanego channel; binding nie może przenieść go przypadkiem na inny port |
| Awaria wieloportowego `TransportDevice` | Wszystkie jego channels są oznaczone wspólną przyczyną; pozostałe urządzenia transportowe działają dalej |

## 6. Tożsamość: channel nie jest transportem ani sensorem

Docelowy system rozdziela:

- `host_id` — trwała tożsamość hosta instalacji;
- `transport_device_id` — tożsamość fizycznego konwertera/radia;
- `channel_id` — trwała logiczna tożsamość channel w zakresie 1–16;
- `transport_binding` — rodzaj adaptera, urządzenie i endpoint/port przypisany do
  channel;
- `node_address` — adres routingu unikalny tylko w danym channel;
- `hardware_id` — trwała tożsamość node;
- `sensor_id` — trwała tożsamość sensora lub punktu pomiarowego;
- `display_label` — nazwa dla operatora, bez ograniczenia do `A..H`.

Minimalny klucz routingu to `(channel_id, node_address)`. Klucz pomiaru powinien
używać trwałej tożsamości sensora/node oraz `(boot_epoch, sample_seq)`, ponieważ
adres transportowy może zostać zmieniony.

Jeden plik Capture może nadal dotyczyć jednego sensora, nawet gdy jeden
`ChannelAgent` obsługuje wiele node. Agent wtedy kieruje partie do osobnych
`CaptureRepository` według tożsamości node. Nie trzeba mieszać wszystkich
sensorów linii w jednym pliku.

### Kontrakt topologii hosta v2

`HostInstallationProfile` wymaga jawnego bindingu transportu i inventory node.
Minimalny kanał ma postać:

```json
{
  "name": "line-a",
  "channel_id": 1,
  "enabled": true,
  "transport": {
    "kind": "serial-rs485",
    "device_id": "usb-rs485-1",
    "endpoint": "/dev/sensor-system-line-a",
    "baud_rate": 115200
  },
  "nodes": [{
    "id": 1,
    "hardware_id": "2f13349450fa4084",
    "board_revision": 2,
    "enabled": true,
    "output_rate_hz": 125
  }]
}
```

`device_id` może się powtarzać dla portów jednego konwertera wielokanałowego;
`endpoint`, `channel_id` i nazwa kanału muszą być unikalne. Adres node jest
unikalny w obrębie kanału, natomiast `hardware_id` w całej instalacji. Obsługiwane
są wszystkie dokładne output rates kontraktu ADXL355, w tym wartości ułamkowe.
Obecny tracked profil `rpi-sanok.json` zachowuje jeszcze układ legacy i nie jest
profilem startowym v2. Przed jego migracją commissioning musi zebrać prawdziwe
hardware ID wszystkich ośmiu node i potwierdzić ich fizyczne przypisanie do
portów; kod nie wypełnia braków domyślną ani wymyśloną tożsamością.

## 7. Ograniczenia obecnej implementacji

Obecny kod nie realizuje jeszcze modelu docelowego:

- `HostSystemConfig` akceptuje `channel_id` tylko `1..8`;
- `SensorIdentity` akceptuje tylko linie `1..8` i etykiety `A..H`;
- Capture v1 wymaga dokładnie jednego node w kanale;
- `CaptureV1WindowedWriter` przechowuje jedną `SensorIdentity` i jeden node;
- supervisor uruchamia jeden recorder na kanał, ale przekazuje mu identity
  pierwszego node;
- narzędzia serwisowe mogą mieć inne zasady własności portu niż recorder.

Nie należy naprawiać tego wyłącznie przez zmianę liczby 8 na 16. Potrzebna jest
wersjonowana zmiana kontraktu konfiguracji i modelu zapisu dla relacji
`host -> transport device -> channel -> node -> sensor`.

## 8. Synchronizacja przy wielu liniach

Wszystkie agenty używają tego samego monotonicznego zegara hosta jako punktu
odniesienia, ale każdy node ma osobny model zegara i osobny `boot_epoch`.

`HostCoordinator` rozkłada pomiary time sync w czasie, aby 16 linii nie wykonywało
jednocześnie kosztownej pracy i flush. Obserwacje zegarów trafiają do Capture,
więc późniejsza rekonstrukcja może uwzględnić ich niepewność.

Rozróżniamy dwa wymagania:

- **wyrównanie znaczników czasu** — oszacowanie UTC/host time każdej próbki z
  podaną niepewnością; możliwe przez wymianę time sync i modele zegarów;
- **jednoczesne próbkowanie fazowe** — fizyczne wykonanie pomiaru w tym samym
  momencie przez wiele node; samo odpytywanie RS485 tego nie gwarantuje i może
  wymagać wspólnego triggera, sygnału czasu lub dodatkowego sprzętu.

Zakres produktu obejmuje wyrównanie znaczników czasu z niepewnością. Jednoczesne
próbkowanie fazowe nie jest obecnym wymaganiem i nie jest obiecywane przez samą
architekturę komunikacji.

## 9. Operacje grupowe

Polecenie dla całego hosta nie powinno być udawane jako operacja atomowa, jeżeli
16 linii nie może wykonać jej w tym samym momencie.

Każda operacja grupowa ma:

- `operation_id`;
- listę docelowych linii;
- wynik per linia;
- deadline;
- politykę `all_or_stop` albo `best_effort`;
- zapis audytowy.

`start_all` może przygotować wszystkie linie, sprawdzić oba poziomy admission
control, a następnie uruchomić je z ustaloną epoką sesji. Nie oznacza to wspólnej
fizycznej chwili pierwszej próbki.

Firmware update powinien domyślnie przebiegać kolejno po liniach lub node, aby
nie wyłączyć całej instalacji naraz i nie przeciążyć hosta.

## 10. Test skalowania

Przed wdrożeniem należy uruchomić co najmniej następującą macierz:

1. 1, 4, 8 i 16 aktywnych linii.
2. Minimalna, nominalna i maksymalna liczba node na linię.
3. Nominalny ODR oraz kontrolowany backlog na każdej linii.
4. Jednoczesna synchronizacja, flush, rotacja okien i status UI.
5. Odłączenie jednej linii, restart node i powrót po czasie bliskim retencji.
6. Opóźniony dysk i kontrolowany błąd storage.
7. Archive i podgląd działające równolegle z recordingiem.
8. Wielogodzinny soak na pełnej długości przewodów.

Mierzymy per linię i globalnie: wykorzystanie magistrali, backlog, czas do
przepełnienia, RTT, retry, CRC errors, czas trwałego zapisu, CPU, pamięć, I/O wait,
rozmiar danych na dobę oraz niepewność synchronizacji.
