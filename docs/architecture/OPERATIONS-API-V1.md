# Operations API v1 — kontrakt domenowy

- Status: implementacja warstwy domenowej w toku
- Ostatnia aktualizacja: 2026-10-13
- Zakres: lokalne sterowanie host v2, per channel i per node

Operations API jest jedyną drogą sterowania aktywnym hostem. `hostctl`, przyszły
dashboard i automatyzacja są klientami tego samego API. Adapter API nie otwiera
RS485 i nie posiada stanu kanału; przekazuje typowane polecenie do workera,
który jest jedynym właścicielem portu, `ChannelAgent` i schedulera.

## Tożsamość i retry

Każde żądanie ma niezerowy `operation_id` `u64`, target i pełną treść operacji.
Retry zachowuje cały request. Ten sam ID i identyczna treść zwracają istniejący
stan. Ten sam ID z inną treścią daje konflikt i nigdy nie wykonuje polecenia.

Host utrzymuje bounded ledger aktywnych i ostatnich zakończonych operacji.
Pełny audit trwały zostanie dodany jako osobny port storage. Zapełnienie ledgera
aktywnymi operacjami daje jawny backpressure; aktywna operacja nie może zostać
wyrzucona w celu przyjęcia nowej.

## Targety

- `Channel(channel_id)` — przejścia trybu i commissioning całej magistrali;
- `Node(channel_id, node_address)` — konfiguracja i operacje konkretnego node;
- `ReadStatus` może wskazywać channel albo node.

Adresy trwałe node to `1..253`. Adres 254 nie jest targetem operatorskim; służy
wyłącznie jako tymczasowe źródło odpowiedzi Commissioning Service.

## Tryb kanału

Control plane rozróżnia:

```text
Recording
  -> EnteringMaintenance
  -> Maintenance
  -> ResumingRecording
  -> Recording
```

`Faulted` i `Offline` są osobnymi stanami obserwowanymi. Przejście do Maintenance
nie jest zmianą flagi. Musi zatrzymać przydzielanie nowych slotów, zakończyć lub
jawnie rozstrzygnąć aktywną transakcję, trwale zapisać odebrane dane, zatrzymać
node i dopiero wtedy udostępnić operacje serwisowe. Powrót wymaga handshake,
sprawdzenia konfiguracji, recovery cursorów oraz ponownego admission control.

Każdy node ma niezależny `SafeStateConverger`. Nie ufa etykiecie stanu hosta:
po restarcie procesu lub niepewnym timeoutcie zaczyna od `GetState`. Normalna
ścieżka do serwisu to `Recording/Stopping → Stop → Idle → EnterMaintenance`.
Jeśli operacja grupowa nie może doprowadzić wszystkich node do Maintenance,
host zmienia cel na stabilne `Idle`: node już serwisowane otrzymują
`ExitMaintenance`, a nadal nagrywające `Stop`.

Po wyczerpaniu ograniczonej liczby zwykłych prób host wysyła ratunkowy
`RestartNode`, czeka na nowy handshake/boot epoch i ponownie odczytuje stan.
Wyczerpanie również prób restartu kończy się stabilnym `Offline` oraz wynikiem
per node. `EnteringMaintenance` i `ResumingRecording` nigdy nie są trwałym
stanem końcowym operacji.

Warstwa wykonawcza tej polityki jest już gotowa jako nieblokujący
`SafeStateWireOperation`. Każda akcja ma deadline właściciela operacji. Po jego
przekroczeniu logiczne polecenie może zostać porzucone dopiero po timeoutcie
wire i zakończeniu kwarantanny spóźnionej odpowiedzi. Transfer próbki nie może
być anulowany tym mechanizmem. Niepewny wynik `RestartNode` nie powoduje od razu
drugiego restartu: host najpierw wykonuje pełny handshake i wymaga zmiany
`boot_epoch`. Plan startowy workera zachowuje do tego pełne wymagania hardware,
board revision, usług, cech i ODR.

`SafeStateGroupOperation` składa te executory sekwencyjnie dla jednej magistrali.
W danej chwili aktywny jest najwyżej jeden node, zgodnie z fizycznym arbitrażem
half-duplex. Osiągnięcie celu i `Offline` są jawnymi wynikami per node. Wejście
do Maintenance nie może jednak użyć jednej grupy z celem Maintenance: worker
najpierw uruchomi grupę z celem `Idle`, wykona końcowy drain zatrzymanych
journali i dopiero uruchomi drugą grupę z celem `Maintenance`.

Pierwsza wersja celowo dopuszcza Configuration i Commissioning w Maintenance.
Configuration pozostaje też dostępne w `Faulted` jako service-safe naprawa przed
restartem. Dzięki temu ruch serwisowy nie powoduje pływania `samples/s` w
Production. Późniejsze read-only sondy mogą otrzymać osobny budżet, jeśli
pomiary wykażą, że jest to potrzebne.

## Operacje pierwszego zakresu

| Operacja | Target | Dozwolony tryb |
| --- | --- | --- |
| ReadStatus | channel lub node | każdy |
| EnterMaintenance | channel | Recording |
| ResumeRecording | channel | Maintenance |
| GetConfiguration | node | Maintenance lub Faulted |
| ValidateConfiguration | node | Maintenance lub Faulted |
| ApplyConfiguration | node | Maintenance lub Faulted |
| ResetConfiguration | node | Maintenance lub Faulted |
| ScanUnassigned | channel | Maintenance |
| AssignAddress | channel + exact hardware ID | Maintenance |

Apply/Reset wymagają `expected_revision`. Commissioning wymaga nonce rundy,
bounded liczby slotów oraz wyłączności kanału. Po przypisaniu host wykonuje
handshake nowego adresu i porównuje hardware ID przed zmianą inventory.

## Bounded command path

Każdy worker otrzymuje osobną ograniczoną skrzynkę poleceń. Odczytuje najwyżej
jedno polecenie na bounded iterację. Pełna kolejka daje `QueueFull`; zerwane
połączenie daje `Disconnected`. Polecenia nie trafiają do globalnej kolejki
próbek i nie obchodzą arbitra transakcji kanału.

## Wynik per target

Stan operacji przechodzi dozwoloną sekwencję:

```text
Accepted -> Running -> Succeeded
                    -> Failed
Accepted -> Rejected
         -> Failed
```

Operacja grupowa będzie kompozycją operacji na konkretnych targetach. Każdy
target zachowuje własny wynik; częściowego sukcesu nie przedstawiamy jako
globalnego sukcesu. Rollback grupowy nie jest domyślnie obiecywany.

## Granice adapterów

Format lokalnego IPC zostanie wybrany po podłączeniu command path do workera.
Nie może zmienić typów domenowych ani reguł admission. Dashboard powstaje po
ustabilizowaniu API i będzie czytał status oraz składał polecenia bez dostępu do
portów urządzeń, Capture WAL lub wewnętrznych kolejek schedulera.

## Kolejna bramka

1. podłączyć gotowe inbox, ledger i `SafeStateGroupOperation` do właściciela
   kanału;
2. zaimplementować sekwencję quiesce → Idle → końcowy drain → Maintenance oraz
   pełne recovery przed wznowieniem schedulera;
3. mapować Configuration/Commissioning na istniejący `ChannelAgent` i wyniki;
4. dodać lokalne IPC i adapter `hostctl`;
5. wykonać HIL firmware 0.5.8, następnie operacje grupowe i audit trwały.
