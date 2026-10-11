# MeasurementServiceHandler firmware v2

- Status: handler, dispatcher i bounded response outbox zaimplementowane host-side
- Ostatnia aktualizacja: 2026-09-30

## Rola

Nazwa pochodzi z `Measurement Service` application protocol 5. Handler nie
wykonuje pomiaru sensora. Jest adapterem pomiędzy payloadem tej usługi a
`DeliverySession`:

```text
application payload <-> MeasurementServiceHandler <-> DeliverySession
```

Implementacja znajduje się w `node/protocol_v3/measurement_handler.h`. Obsługuje
wyłącznie `GET_STREAM_STATE`, `PULL_SAMPLES` i `RELEASE_SAMPLES`. Nie zna COBS,
CRC, adresowania framingu, UART, harmonogramu hosta ani storage.

## Przepływ requestu

Handler:

1. wybiera message ID;
2. używa codeca Measurement do walidacji i dekodowania payloadu;
3. tłumaczy request na domenowy command;
4. wywołuje `DeliverySession`;
5. mapuje każdą typowaną decyzję na wynik Measurement albo wspólny
   `ResponseStatus`;
6. koduje ograniczony payload odpowiedzi.

Nieprawidłowa długość i wartości strukturalne dają `MalformedRequest`, cursor
przed stanem node daje `InvalidState`, aktywne przygotowanie daje `Busy`, a błąd
wewnętrznego niezmiennika daje `InternalError`.

## Brak pełnej kopii chunku

Firmware używa `SampleChunkView` i callbacku odczytującego kolejne rekordy z
maksymalnie dwóch spanów journala. Codec zapisuje signed XYZ24 bezpośrednio do
docelowego payloadu. Nie powstaje pośrednia tablica `RawSample[48]`.

Hostowy decoder nadal zwraca posiadany `SampleChunk`, ponieważ odebrany payload
nie może zależeć od później nadpisanego bufora transportu. Obie strony używają
tego samego układu wire; test porównuje wynik view byte-for-byte z istniejącym
golden vectorem.

## Granica zakończenia delivery

Dla `SAMPLE_CHUNK`, `GAP` i `CONTINUITY_BREAK` handler zwraca razem z payloadem:

```text
completion_required = true
delivery_token       = token przygotowanego zakresu
```

W tej chwili `delivered` nadal nie jest przesunięty. Właściciel ograniczonej
kolejki transportu musi wykonać dokładnie jedno z:

```text
enqueue zaakceptowany -> complete(token)
enqueue odrzucony     -> abort(token)
```

Błąd kodowania powoduje natychmiastowy abort w handlerze. `DispatchResult`
przenosi token bez zmiany stanu, a `ResponseOutbox::accept_request` atomowo
wykonuje dispatch i próbę umieszczenia odpowiedzi w ograniczonej kolejce.
Pełna kolejka nie uruchamia handlera, więc nie powstaje token wymagający cleanup.

## Dispatcher i właściciel kolejki

`ApplicationDispatcher` kieruje service ID `2` do handlera tylko wtedy, gdy
capabilities deklarują Measurement. Brak wymaganego handlera daje fail-closed
`InternalError`. Wynik zawiera opcjonalny `ResponseCompletion`.

`ResponseOutbox<N>` ma stałą pojemność i jednego właściciela. Przyjmuje request,
wywołuje dispatcher, sprawdza ramkę i rozstrzyga token:

```text
wolny slot + poprawna odpowiedź -> complete -> zapisz Frame w slocie
niepoprawna odpowiedź           -> abort
pełna kolejka                    -> nie uruchamiaj dispatchu
obcy adres                       -> nie twórz odpowiedzi
```

Kolejny etap to `ProtocolV3TransportEndpoint`: inkrementalny decoder framingu,
`ResponseOutbox`, encoder framingu oraz pojedynczy aktywny TX na porcie RS485.
Nie będzie używał starego grant/burst/commit.
