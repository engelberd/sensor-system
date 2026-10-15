# Operations API v1 — kontrakt domenowy

- Status: pierwszy pion lokalnego API gotowy
- Ostatnia aktualizacja: 2026-10-15
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
`MaintenanceEntryPlan` pilnuje tej kolejności jako osobna, czysta polityka.
Między osiągnięciem `Idle` i drainem wymaga ponownego uzgodnienia strumieni,
ponieważ awaryjny restart node mógł zmienić `boot_epoch`; worker nie może wtedy
kontynuować starej sesji ani dopisać danych do poprzedniego strumienia.

## Integralność danych podczas serwisu

W zwykłym `Recording` brak próbki jest zdarzeniem integralności. Host ponawia
transfer po krótkich błędach komunikacji i korzysta z retencji journala node,
dopóki nie grozi jej przepełnienie. Każda rzeczywista luka pozostaje jawna w
Capture.

Jawna operacja operatora `EnterMaintenance`, restart lub odłączenie sprzętu
ustanawia granicę serwisową. Worker nadal wykonuje Stop i ograniczony końcowy
drain, ale produkt nie obiecuje bezstratności podczas ręcznej ingerencji i nie
blokuje bez końca dostępu serwisowego dla kilku próbek. Nierozliczony fragment
jest zapisywany jako gap albo continuity break wraz z powodem serwisowym. Taka
polityka nie osłabia normalnego nagrywania: luka bez jawnej operacji pozostaje
błędem wymagającym diagnostyki.

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
| RestartNode | node | Maintenance lub Faulted |
| ScanUnassigned | channel | Maintenance |
| AssignAddress | channel + exact hardware ID | Maintenance |

Apply/Reset wymagają `expected_revision`. Commissioning wymaga nonce rundy,
bounded liczby slotów oraz wyłączności kanału. Operations API v1 przyjmuje
`1/2/4/8/16` slotów, aby pełny wynik mieścił się w ograniczonej odpowiedzi IPC;
sam wire protocol zachowuje zakres do 256 slotów. Po przypisaniu host wykonuje
handshake nowego adresu i porównuje hardware ID. Udany wynik nie zmienia
automatycznie trwałego profilu instalacji.

## Bounded command path

Każdy worker otrzymuje osobną ograniczoną skrzynkę poleceń. Odczytuje najwyżej
jedno polecenie na bounded iterację. Pełna kolejka daje `QueueFull`; zerwane
połączenie daje `Disconnected`. Polecenia nie trafiają do globalnej kolejki
próbek i nie obchodzą arbitra transakcji kanału.

Ta granica jest podłączona do produkcyjnej kompozycji: `HostCoordinator`
zachowuje osobny uchwyt poleceń i snapshot statusu dla każdego uruchomionego
kanału, a ledger należy wyłącznie do jego workera. `ReadStatus` przechodzi już
całą drogę inbox → admission → ledger → snapshot. Błędny `channel_id` jest
odrzucany przed kolejką. Pojemności mailboxa i ledgeru pochodzą z
`runtime_v2`, więc nie są ukrytymi stałymi.

`EnterMaintenance` ma podłączony wykonawca tego samego workera i arbitra RS485:
quiesce → sekwencyjne `Idle` → kontrola `boot_epoch` → końcowy drain →
sekwencyjne `Maintenance`. Nie przyjmuje skrótu omijającego trwały zapis.
Pozostałe mutujące polecenie, dla którego wykonawca nie został jeszcze
podłączony, kończy się jawnym `Failed` z `ExecutorUnavailable`; nie zatrzymuje
workera ani akwizycji.

Deadline bariery i drainu oraz budżety orderly/restart są częścią
`runtime_v2`. Drain po przekroczeniu deadline odrzuca tylko niewysłane jeszcze
sondy; aktywne wire/storage nadal musi się rozliczyć. Zmiana `boot_epoch`
podczas wymuszonego recovery kończy operację w `Faulted` z
`StreamReconciliationRequired`, ponieważ stara sesja Capture nie może zostać
przemianowana na nowy strumień. Pełne recovery w procesie należy do ścieżki
`ResumeRecording`.

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

## Lokalny adapter IPC

Pierwszy adapter działa przez lokalne datagramowe gniazdo Unix
`/run/sensor-system/operations-v1.sock`. Każdy datagram jest jedną ograniczoną
wiadomością JSON schema 1; request i response mają limit 4096 B, a koordynator
obsługuje najwyżej cztery żądania na jeden obrót. Klient nie może utrzymać
blokującego połączenia ani wejść na drogę próbek.

Na Linuksie klient tworzy krótkotrwałe gniazdo odpowiedzi obok gniazda serwera
w `/run/sensor-system`, z uprawnieniami `0600`, i usuwa je po zakończeniu
żądania. Jest ono dzięki temu widoczne dla usługi uruchomionej z
`PrivateTmp=true`; użycie publicznego `/tmp` rozdzielałoby klienta i serwer na
dwie przestrzenie nazw.

Adapter udostępnia odczyt trybu kanału, `EnterMaintenance`, `ResumeRecording`,
pełny pierwszy zakres Configuration, `RestartNode`, `ScanUnassigned` i
`AssignAddress`. Operacje per node działają wyłącznie w Maintenance/Faulted,
a commissioning wyłącznie w Maintenance. Wszystkie zwracają typowany wynik
zapisany w tym samym bounded ledgerze. `submit`
przekazuje pełne `operation_id` do istniejącego
bounded mailboxa. `get` odczytuje ten sam worker-owned ledger. Powtórzenie
identycznego żądania jest idempotentne, a ponowne użycie ID z inną treścią
kończy się konfliktem w domenie. IPC nie otwiera RS485, nie posiada lifecycle i
nie tworzy drugiego modelu stanu.

Wykonawca Configuration ma osobny konfigurowalny deadline. Po jego
przekroczeniu nie porzuca transakcji w locie ani kwarantanny spóźnionej
odpowiedzi; kończy operację dopiero po bezpiecznym oddaniu jedynego arbitra
RS485. Nieznany node daje jawny błąd i nie zatrzymuje workera.

Scan odpytuje sloty sekwencyjnie przez tego samego arbitra RS485, zapisuje
ostatni kompletny wynik tylko w pamięci procesu i rozróżnia pusty slot od błędu
całej rundy. Assign wymaga hardware ID oraz nonce z ostatniego udanego skanu,
odrzuca adres zajęty przez skonfigurowany node, a sukces publikuje dopiero po
handshake pod nowym adresem. `RestartNode` kończy się po potwierdzonym wysłaniu
polecenia; `ResumeRecording` wykonuje później świeży handshake, wykrywa nowy
`boot_epoch` i przeprowadza trwały rollover Capture oraz Time.

`ApplyConfiguration` i `ResetConfiguration` wymagają `expected_revision` oraz
Maintenance. Jeszcze przed mutacją host waliduje pełną wartość, sprawdza maskę
ODR z capabilities i przelicza admission dla wszystkich node na kanale. Wynik
odrzucony przez budżet nie trafia na wire. Po odpowiedzi `Applied` worker:

1. dopisuje trwały snapshot konfiguracji i `effective_sample_seq`;
2. rekonfiguruje quiescent scheduler;
3. atomowo zastępuje zatwierdzony plan startowy i wymagany ODR handshake.

Awaria hosta po trwałej mutacji node, lecz przed aktualizacją pamięci procesu,
jest rozstrzygana przez `GetConfig` podczas następnego bootstrapu. Host przyjmuje
rzeczywistą konfigurację node jako wejście do admission; nie odtwarza ślepo
wartości z profilu instalacji. `ResetConfiguration` ustawia produktowy profil
pomiarowy 125 Hz, 2 g, HP=0, Balanced, watermark 30 i zerowe offsety. Nie zmienia
adresu ani parametrów łącza.

Dashboard powstaje po ustabilizowaniu pozostałych operacji i będzie czytał
status oraz składał polecenia bez dostępu do portów urządzeń, Capture WAL lub
wewnętrznych kolejek schedulera.

## Powrót do Recording

`ResumeRecordingPlan` jest czystą polityką kolejności niezależną od wire i
storage. Każde wznowienie zaczyna się od ponownej weryfikacji wszystkich node.
Jeżeli tożsamość strumienia się nie zmieniła, plan przechodzi bezpośrednio do
ponownego admission control. Nowy `boot_epoch` bezwarunkowo kieruje najpierw do
stream recovery; nie można wtedy uruchomić node ani schedulera na starej sesji.
Po recovery plan nadal wymaga admission, aktywacji wszystkich node i dopiero na
końcu otwiera scheduler Measurement/TimeSync. Błąd kończy się stabilnym
`Faulted`, a nieosiągalny node stabilnym `Offline`; stan `ResumingRecording` nie
jest wynikiem końcowym.

Polityka i nieblokujący adapter wire są podłączone do produkcyjnego workera.
Każdy node jest sprawdzany sekwencyjnie przez pełny handshake, `GetState` i
`GetStreamState` z jednym konfigurowalnym deadline. `GetStreamState` ma własny
klucz bezpiecznego porzucenia i, tak jak pozostałe control work, czeka na
zakończenie aktywnej próby oraz kwarantanny spóźnionej odpowiedzi. Przy tej
samej epoce worker ponownie buduje `NodeSession` wyłącznie z cursora trwałego w
Capture, wymaga zgodnego kursora schedulera, uruchamia node sekwencyjnie i na
końcu otwiera Measurement/TimeSync.

Admission używa zatwierdzonego planu aktualizowanego po każdej udanej mutacji.
Bootstrap i Resume ponownie odczytują lub weryfikują faktyczny ODR oraz
capabilities przed aktywacją Measurement.

Pion Recording → Maintenance → Recording przechodzi przez rzeczywisty framing,
codeki, symulowany RS485 i produkcyjny worker w macierzy 1/4/8 channels. Zmiana
`boot_epoch` wykonuje przed aktywacją trwały rollover Capture i Time oraz
wymianę sesji. Ten sam pion jest dostępny przez lokalne IPC i `hostctl`.

## Kolejna bramka

1. wykonać HIL Scan/Assign z rzeczywistym nieprzypisanym node i zatwierdzić
   procedurę aktualizacji profilu instalacji;
2. dodać trwały audit poleceń operatorskich;
3. dodać operacje grupowe na stabilnym kontrakcie wyników per target.

HIL na Raspberry Pi z firmware 0.5.8 potwierdził Get/Apply/Reset, pusty
ScanUnassigned, RestartNode, zmianę `boot_epoch` i późniejszy ResumeRecording.
Reset zwrócił produktowy profil 125 Hz. Po restarcie pojawił się jeden
przejściowy timeout i jeden decode error; kolejne transakcje i Measurement były
stabilne, bez gapów.
