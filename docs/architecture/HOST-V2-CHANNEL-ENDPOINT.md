# Host v2 Channel Endpoint

- Status: adapter, fizyczny HIL jednej linii i symulacja wielu node gotowe;
  produkcyjny Capture i HIL wielu node pozostają otwarte
- Ostatnia aktualizacja: 2026-10-08

## Jedna linia, jeden właściciel

`ChannelEndpoint` posiada dokładnie jeden `ChannelAgent`, jeden `ChannelPort`
i jeden zegar monotoniczny. Tylko endpoint może połączyć decyzję schedulera z
fizycznym I/O. Jeden krok wykonuje najwyżej jedno obserwowalne działanie, dlatego
na half-duplex RS485 nie powstają ukryte równoległe requesty.

```text
ChannelEndpoint
  |-- ChannelAgent       wykonanie jednej transakcji, retry i korelacja
  |-- ChannelPort        bajty, COBS, CRC, częściowy write, drain TX
  `-- MonotonicClock     timeout liczony po zakończeniu TX
```

`ChannelPort` używa stałych buforów RX i TX. Nadmiarowe ramki pozostają w
buforze do następnego kroku. Błąd ramki nie niszczy kolejnej ramki po delimiterze.
Port szeregowy jest otwierany jawnie jako 8N1 bez flow control. Zależność
`serialport` ma wyłączoną domyślną funkcję `libudev`, więc recorder na Raspberry
Pi nie wymaga jej tylko do enumeracji urządzeń.

`write_all()` obsługuje częściowe zapisy. Sukces TX następuje dopiero po
`flush()`; implementacja POSIX `serialport` używa `tcdrain`. Dopiero wtedy
`ChannelAgent::transmit_completed()` uruchamia timeout odpowiedzi. Błąd zapisu,
również po częściowym TX, jest traktowany jako niepewny wynik i przechodzi przez
reconciliation.

## Droga próbki

```text
PULL_SAMPLES
  -> zweryfikowana ramka
  -> MeasurementTransfer / NodeSession
  -> pełny SampleChunk (XYZ24 + config_revision + timing evidence)
  -> DurableMeasurementSink
  -> DurableReceipt
  -> confirm_durable
  -> dopiero następny request może podnieść release_before
```

`MeasurementTransfer` zachowuje cały wire `SampleChunk`; metadane czasu i
konfiguracji nie są redukowane do samych osi. `ChannelAgent` zatrzymuje
zakończony transfer w pojedynczym handoffie. Dopóki właściciel zapisu go nie
odbierze, agent nie rozpoczyna kolejnej pracy na kanale.

`DurableMeasurementSink` definiuje mocny kontrakt: `Ok(())` oznacza zapis,
który przeżyje restart zgodnie z polityką storage. Błąd sinka pozostawia chunk,
gap albo continuity break w stanie pending i nie przesuwa durable ani release.
Konkretna implementacja pliku oraz polityka około dziesięciominutowych okien są
następnym pionem warstwy danych.

## Granica następnego etapu

`ChannelScheduler`, `ChannelRuntime`, `MeasurementCoordinator` i
`ChannelRecordingLoop` składają już docelową odpowiedzialność pętli Measurement
dla wielu node. Agent wykonuje wybraną pracę i nadal jako jedyny posiada
`TransactionCore`; runtime posiada kolejność, deadline, retry i offline backoff.
Przed dołączeniem Time Service, Health lub Configuration trzeba dodać ich jawne
kandydatury pracy i wykonawców, bez przenoszenia polityki do agenta.

Zakończony transfer Measurement chwilowo blokuje kanał do chwili odebrania go
przez warstwę trwałości. Chroni to cursor przed wyprzedzeniem danych. Docelowy
Capture nie może wykonywać `fsync` w pętli kanału: handoff trafia do ograniczonej
kolejki storage, a scheduler wznawia dany node dopiero po `DurableReceipt`.

Kolejka prac hosta i wektor próbek korzystają z heapu, ale przyjęcie pracy oraz
liczba próbek są ograniczone odpowiednio przez `ChannelPolicy` i maksymalny
payload. Stałopojemnościowe są bufory wire. Pomiary RSS i alokacji należą do
bramki obciążeniowej 1–16 channels.

## Otwarte elementy

- HIL wielu node i pomiar wpływu bounded storage handoff na kanał;
- konkretny sink plikowy z recovery po restarcie i realnym `fsync`;
- time sync oraz budżet jego ramek;
- composition root i nadzór 1–16 endpointów;
- pomiar opóźnień, wykorzystania magistrali i dobranie pojemności.
