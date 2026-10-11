# Lifecycle Service protocol 5

- Status: codec host/node, dispatcher, bounded handler i kompozycja `main_v2`
  gotowe; update, rollback i confirm A/B sprawdzone fizycznie przez RS485
- Ostatnia aktualizacja: 2026-10-06

## Cel

Lifecycle Service przenosi `GetState`, `Start`, `Stop`, wejście/wyjście z
Maintenance oraz zmianę profilu obserwowalności. Warstwa protocol 5 nie wykonuje
sensora bezpośrednio. Core 1 przekazuje stałe polecenie przez `OperationMailbox`,
a jedynym właścicielem przejść jest `SensorRuntime` na core 0.

Operacje systemowe `PrepareFirmwareUpdate` i `ConfirmTrialBoot` korzystają z
tego samego kontraktu korelacji, lecz trafiają do osobnego mailboxa core 0.
Pierwsza uzbraja wejście do bootloadera dopiero po wysłaniu odpowiedzi. Druga
zapisuje potwierdzenie aktualnie wykonywanego slotu trial przed odpowiedzią.

Każde polecenie zawiera niezerowe `operation_id`. Host ponawia ten sam zamiar z
tym samym ID. Zmiana treści polecenia przy zachowaniu ID zwraca `Conflict`.

## Wynik przy opóźnieniu

Są dwa różne przypadki:

- application `ResponseStatus::Busy` oznacza, że polecenie nie zostało przyjęte;
- lifecycle `SensorRuntimeResultCode::InProgress` oznacza, że polecenie zostało
  przyjęte, ale jego stan trzeba ponownie uzgodnić.

Handler dopuszcza jedną aktywną operację. Ponowienie aktywnego polecenia nie
dodaje drugiej kopii do mailboxa. Terminalny wynik jest przechowywany w cache o
stałej pojemności, więc zgubienie odpowiedzi nie wykonuje `Start` lub `Stop`
ponownie.

`Stop` może otrzymać z core 0 wynik `InProgress/Stopping`. Taki wynik nie jest
terminalnie cache'owany. Następne zapytanie z tym samym `operation_id` ponownie
wywołuje idempotentny `Stop`, aż runtime zwróci `Ok/Idle` albo fault.

Nieoczekiwany wynik mailboxa jest błędem integralności. Handler przechodzi w
stan faulted i odpowiada `InternalError`, zamiast przypisać wynik do niewłaściwej
operacji.

## Format

Message ID wybiera operację. Payload zwykłej operacji zawiera `operation_id`
u32 little-endian. `SetObservabilityProfile` dodaje jeden bajt profilu.

Poprawna odpowiedź ma 9 bajtów:

```text
response_status:u8
operation_id:u32 LE
runtime_result_code:u8
runtime_state:u8
observability_profile:u8
sensor_status:u8
```

Kodeki C++ i Rust sprawdzają długość, wartości enumów oraz niezerowe ID.

## Granice retencji

Cache wyników jest ograniczony i cykliczny. Host powinien używać rosnących ID w
obrębie jednego boot epoch i uzgodnić wynik przed długą serią kolejnych operacji.
Rozmiar cache zostanie dobrany w profilu produkcyjnym, a następnie zweryfikowany
w HIL. Zmiana pojemności nie zmienia kontraktu usługi.

## Akcje po transmisji

Dispatcher i `TransportEndpoint` przenoszą oddzielny, typowany token
post-transmit. Token nie jest uznawany przy zbudowaniu odpowiedzi, dodaniu jej do
outboxa ani rozpoczęciu TX. Handler otrzymuje sukces dopiero po fizycznym
`tx_completed`. Błąd kodowania i timeout TX rozliczają token jako niewysłany.

Mechanizm jest gotową granicą dla wejścia do bootloadera. Sama operacja update
jest oddzielona od `SensorRuntime`: ma osobny mailbox i
`SystemOperationRuntime` na core 0. `PrepareFirmwareUpdate` jest przyjmowane
wyłącznie w `Maintenance`. Poprawny wynik otrzymuje token post-transmit; dopiero
jego zatwierdzenie ustawia `DeferredSystemAction::EnterBootloader`.

Nieudana transmisja pozostawia token do bezpiecznego retry z tym samym
`operation_id`. `Idle`, `Recording`, `Stopping` i `Faulted` nie uzbrajają akcji
bootloadera.

`ConfirmTrialBoot` nie tworzy akcji post-transmit. Zapis metadanych A/B kończy
się przed wynikiem `Ok`. Ponowienie po zgubieniu odpowiedzi jest idempotentne:
ten sam bieżący slot, który jest już aktywny, ponownie zwraca sukces bez
zwiększenia licznika potwierdzeń. Inny slot, obraz bez trial albo uszkodzone
metadane nie mogą zostać zatwierdzone.

## Granica odpowiedzialności handlera

Wspólny handler posiada korelację `operation_id`, jedną aktywną operację,
idempotency i ograniczony cache wyników. Dwie obecne operacje systemowe są małym
wyjątkiem kierowanym do osobnego mailboxa. Nie dokładamy tu kolejnych polityk
update ani konfiguracji. Przed trzecią operacją systemową powstaje osobny
handler/router system operations; mechanizm korelacji może pozostać wspólnym,
małym komponentem. Dzięki temu Lifecycle nie stanie się właścicielem całego
Maintenance.
