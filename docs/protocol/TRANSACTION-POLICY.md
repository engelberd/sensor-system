# Application protocol 5 — polityka transakcji

- Status: obowiązujący kontrakt hosta; parametry czasowe wymagają pomiarów HIL
- Date: 2026-09-29
- Framing: 3

## 1. Zakres

Transakcja to jedna adresowana para:

```text
host REQUEST -> dokładnie jeden node RESPONSE
```

Framing odpowiada za granice, CRC i routing ramki. Transaction Core odpowiada za
korelację, deadline oraz bezpieczne odzyskanie magistrali po timeout. Service
odpowiada za znaczenie requestu, idempotencję i uzgodnienie zmienionego stanu.
Scheduler określa budżet retry oraz wybiera kolejną pracę.

Na jednym channel istnieje najwyżej jedna aktywna transakcja. Broadcast jest
operacją fire-and-forget i nie wchodzi do mechanizmu request/response.

## 2. Identyfikatory prób i operacji

Każda próba wysłana na przewód otrzymuje nowy `transaction_id u32`. Odpowiedź
musi powtórzyć odwrócony route, service i transaction ID requestu. Oczekiwany
response message jest ustalony przed wysłaniem requestu; domyślnie jest równy
request message, a service może jawnie zdefiniować osobny typ odpowiedzi. Retry
tego samego logicznego requestu jest nową próbą i otrzymuje nowy transaction ID.

`transaction_id` nie jest gwarancją exactly-once ani trwałym operation ID.
Operacja, która może zmienić stan i wymaga deduplikacji ponad retry lub restart,
przenosi własny `operation_id`, `expected_revision` albo monotoniczny cursor w
payloadzie danego service.

Generator ID zaczyna od nieprzewidywalnego lub trwale rozróżnialnego seeda dla
nowej instancji channel i zwiększa wartość z zawinięciem `u32`. Jedna aktywna
transakcja, guard time i handshake po restarcie zapobiegają przypisaniu starej
odpowiedzi po zawinięciu.

## 3. Deadline i odpowiedź spóźniona

Deadline obejmuje:

- wysłanie całego requestu;
- turnaround adaptera i transceivera;
- maksymalny deklarowany czas obsługi requestu przez node;
- wysłanie maksymalnej dozwolonej odpowiedzi;
- zmierzony jitter adaptera i systemu hosta.

Odpowiedź jest terminowa, gdy kompletna, poprawna ramka została odebrana nie
później niż w deadline. Fragment ramki przed deadline nie przedłuża transakcji.

Po deadline host:

1. oznacza próbę jako `TimedOut`;
2. pozostaje w RX i rozpoczyna quarantine/guard time;
3. odrzuca każdą odpowiedź starej transakcji jako `Late`;
4. nie rozpoczyna nowej transmisji podczas quarantine;
5. po jego zakończeniu przekazuje schedulerowi `RetrySafe` albo
   `ReconcileState`.

Odpowiedź spóźniona nigdy nie zmienia wyniku zakończonej próby. Jest liczona w
diagnostyce. Dzięki temu wynik nie zależy od kolejności obsługi timera i kolejki
serial po przekroczeniu deadline.

Każdy message contract określa maksymalny czas rozpoczęcia odpowiedzi przez
sprawny node. Node, który nie może dotrzymać tego limitu, nie rozpoczyna późnej
transmisji; zwraca `BUSY` w czasie kontraktu albo milczy. Bez tej reguły żaden
skończony guard time hosta nie może zagwarantować braku kolizji ze skrajnie
spóźnioną odpowiedzią.

Guard time musi być co najmniej sumą maksymalnego czasu ramki wire i zmierzonego
marginesu adaptera. Przy 115200 8N1 maksymalna ramka 530 B trwa około 46 ms, więc
wartość startowa przed HIL wynosi 50 ms. Jest to koszt ponoszony tylko po
timeout, nie po poprawnej transakcji.

## 4. Klasy timeout recovery

### `RetrySafe`

Powtórzenie logicznego requestu nie może pogorszyć stanu. Przykłady:

- identity, capabilities, health i odczyt state;
- time sync jako nowa obserwacja;
- `PULL_SAMPLES` od tego samego cursora;
- monotoniczny `RELEASE_SAMPLES` dla tego samego `boot_epoch`.

Scheduler może zaplanować bounded retry z nowym transaction ID. Limit prób i
deadline recovery wynikają z retencji oraz trybu channel.

Implementacja measurement wykonuje mały budżet prób wewnątrz jednego slotu
magistrali. Po jego wyczerpaniu zwraca niezmienioną `NodeSession` do runtime.
Runtime stosuje bounded backoff i wybiera inny node; pojedynczy uszkodzony node
nie może przejąć całego half-duplex channel. Pomyślna retransmisja nadal kończy
ten sam logiczny pull od tego samego cursora.

### `ReconcileState`

Request mógł zostać wykonany, mimo że response zginął. Host nie wykonuje ślepego
retry. Najpierw odczytuje autorytatywny stan i porównuje revision, operation ID,
cursor albo lifecycle state. Dotyczy to między innymi konfiguracji, zmiany
adresu/baudrate, restartu i przejść trybu.

Service musi opisać konkretną transakcję uzgadniającą. Brak takiego opisu blokuje
nadanie message ID operacji zmieniającej stan.

## 5. Klasyfikacja odebranej ramki

Poprawna ramka response może zostać sklasyfikowana jako:

| Wynik | Znaczenie | Skutek |
| --- | --- | --- |
| `Accepted` | dokładny route/service/message/ID przed deadline | kończy próbę |
| `Unrelated` | nie pasuje do aktywnej próby | odrzuć i zlicz |
| `Late` | pasuje do próby po deadline | odrzuć i zlicz |
| `Unsolicited` | brak aktywnej próby | odrzuć i zlicz |

Node nie wysyła spontanicznych application frames. Przyszłe eventy push
wymagałyby osobnego, wersjonowanego modelu dostępu do magistrali.

## 6. Maszyna stanów hosta

```text
Idle
  -> AwaitingResponse(request, deadline)
      -> Idle                         [Accepted]
      -> Quarantine(until)            [timeout]
  -> Quarantine(until)
      -> Quarantine                   [Late/Unrelated]
      -> Idle                         [guard elapsed]
```

Rozpoczęcie requestu poza `Idle` jest błędem własności channel. Codec nie
wykonuje retry i nie zmienia tej maszyny poza dostarczeniem zweryfikowanej
ramki.

## 7. Obserwowalność

Channel utrzymuje oddzielne liczniki: accepted, timeout, retry, reconciliation,
late, unrelated, unsolicited, malformed COBS, CRC error, UART error i czas
quarantine. Liczniki nie zmieniają decyzji protokołu i mają ograniczony koszt.

## 8. Parametry do ustalenia pomiarem

Mechanizm jest zamrożony, a poniższe wartości pozostają konfiguracją profilu:

- processing bound każdego message;
- jitter USB/RS485 adaptera;
- turnaround TX/RX;
- domyślny timeout klasy wiadomości;
- ostateczny guard time;
- retry budget i backoff;
- próg uznania rosnącej liczby late responses za awarię node lub adaptera.
