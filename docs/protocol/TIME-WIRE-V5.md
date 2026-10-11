# Time Service wire contract — application protocol 5

- Status: codeki Rust/C++, wspólny golden vector, endpoint firmware 0.5.6,
  hostowy exchange, scheduler, persistence i HIL jednego node gotowe
- Service ID: `3`
- Message ID: `1` (`OBSERVE_CLOCK`)

## Request

Request nie ma payloadu. Adres node i `transaction_id` znajdują się w nagłówku
framing v3. Każda retransmisja jest nową próbą i dostaje nowe `transaction_id`
oraz nowe lokalne `t1`.

## Success response

Wszystkie liczby są little-endian.

| Offset | Rozmiar | Pole |
| ---: | ---: | --- |
| 0 | 1 | `ResponseStatus::Ok` |
| 1 | 8 | `boot_epoch` |
| 9 | 8 | `t2_node_receive_us` |
| 17 | 8 | `t3_node_send_us` |

Łączny payload success ma 25 bajtów. `boot_epoch` nie może być zerem, a `t3`
nie może być mniejsze od `t2`. Odpowiedź błędu ma dokładnie jeden bajt statusu.
Inna długość jest błędem kontraktu, a nie rozszerzeniem tolerowanym przez host.

`t1` i `t4` nie przechodzą przez wire. Host przechowuje je przy aktywnej próbie
i tworzy `ClockObservation` dopiero po korelacji całej odpowiedzi przez
Transaction Core. Znaczenie granic timestampów i model opisuje
[TIME-SERVICE.md](../architecture/TIME-SERVICE.md).

Firmware 0.5.6 reklamuje Time Service dopiero po spięciu dispatchera, który
chwyta `t2` przy przyjęciu requestu. Odpowiedź może bezpiecznie czekać w bounded
outboxie: endpoint uzupełnia `t3` bezpośrednio przed `start_tx`, a nie podczas
budowania odpowiedzi. Ponowiona próba startu TX aktualizuje `t3`.

Fizyczny HIL 2026-10-10 wykonał osiem kolejnych exchange przez RS485 115200 bez
błędu dekodowania i bez timeoutu. Stały `boot_epoch` został zweryfikowany przez
hosta. Zmierzony path delay wyniósł 12,7–17,8 ms, a konserwatywna niepewność
pojedynczej obserwacji 6,4–8,9 ms. Krótki przebieg nie wyznacza jeszcze dryfu
oscylatora ani docelowej częstotliwości synchronizacji.

## Koszt

Payload jest krótszy od typowego chunku Measurement i ma stały koszt. Nie
zawiera UTC, fitted offsetu, ODR ani liczników diagnostycznych. Te dane należą
do modelu hosta albo osobnych services.
