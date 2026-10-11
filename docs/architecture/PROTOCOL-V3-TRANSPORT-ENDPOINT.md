# ProtocolV3TransportEndpoint

- Status: runtime i testy jednostkowe gotowe; fizyczny HIL jednej linii zaliczony
- Ostatnia aktualizacja: 2026-09-30

## Odpowiedzialność

`TransportEndpoint<Port, QueueCapacity>` jest jedynym właścicielem warstwy
protocol 3 dla jednego portu node. Łączy istniejące, niezależne elementy:

```text
Port RX -> StreamDecoder -> ApplicationDispatcher -> ResponseOutbox
         -> framing encoder -> dokładnie jeden aktywny Port TX
```

Nie steruje sensorem, nie planuje hosta, nie wykonuje retry requestu i nie
zwalnia próbek samodzielnie. Kod znajduje się w
`node/protocol_v3/transport_endpoint.h`.

## Ograniczenia pracy

- wszystkie bufory i kolejki mają rozmiar znany podczas kompilacji;
- nie ma alokacji dynamicznej;
- jedno `update()` czyta najwyżej `max_rx_bytes_per_update`;
- podczas aktywnego half-duplex TX endpoint nie czyta RX;
- na porcie może istnieć dokładnie jeden aktywny TX;
- odpowiedzi czekają w `ResponseOutbox<N>`;
- liczniki diagnostyczne są saturujące i nie sterują domeną.

Domyślna polityka wynosi 256 bajtów RX na update i 100 ms deadline TX. Są to
parametry konstruktora, nie niezmienniki protokołu. Maksymalna ramka przy
115200 bit/s zajmuje około 46 ms na przewodzie, więc 100 ms daje początkowy
margines na nadawanie i obsługę portu. Wartość wymaga dalszych pomiarów pod
obciążeniem wielu node.

## TX i awarie

Po przyjęciu response do outboxa próbki mogą zostać oznaczone jako `delivered`,
ale pozostają w journalu aż do późniejszego release hosta. Dlatego błąd kodowania,
timeout TX albo utrata odpowiedzi na przewodzie nie usuwa danych. Host ponawia
pull od nieprzesuniętego durable cursora.

Jeżeli `start_tx` chwilowo odrzuci start, ramka pozostaje w kolejce i następne
`update()` ponawia próbę. Po przekroczeniu `tx_timeout_us` endpoint wywołuje
`Port::abort_tx`, zwalnia half-duplex TX i czeka na ponowiony request hosta.
`Rs485Port::abort_tx` czeka na opróżnienie FIFO, wyłącza driver i
czyści echo RX.

## Obsługa RX

`StreamDecoder` zachowuje częściową ramkę pomiędzy wywołaniami. Błędne COBS,
length, version lub CRC zwiększa licznik i nie blokuje następnej poprawnej ramki.
Request obcego node jest zużywany, lecz nie generuje odpowiedzi. Gdy kolejka
odpowiedzi jest pełna, nowy request nie uruchamia dispatchu; host odzyska go przez
timeout i retry.

## Stan HIL

Testy host-side obejmują fragmentację, budżet RX, sklejone requesty, blokadę RX
podczas TX, chwilowy błąd startu TX, timeout z abortem oraz odzyskanie po
uszkodzonej ramce. Build board rev 2 kompiluje rozszerzony `Rs485Port`.

Bounded ingress core 0 → core 1, właściciel journala oraz propagacja krytycznego
faultu są gotowe w czystej kompozycji. Runtime protocol 5 przeszedł fizyczny
handshake, trial confirm oraz pełny cykl Measurement na board rev 2. Obecny
adapter nadaje deterministycznie przez UART na rdzeniu komunikacyjnym;
optymalizacja DMA pozostaje osobnym zadaniem i nie zmienia kontraktu `start_tx`.
