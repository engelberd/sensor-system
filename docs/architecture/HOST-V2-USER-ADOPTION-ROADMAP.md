# Roadmapa operacyjnego wdrożenia hosta v2

Status: plan przyjęty do realizacji krok po kroku. Dokument opisuje drogę od
działającego rdzenia rejestratora do systemu, który operator może bezpiecznie
uruchomić na Raspberry Pi na test 24/7 i nadzorować zdalnie.

## 1. Punkt wyjścia

Rdzeń akwizycji nie jest już głównym ryzykiem. Sześć fizycznych linii RS485
przeszło 60-sekundowy przebieg oraz 30-sekundowy przebieg po restarcie procesu.
W obu testach Capture i Time WAL zachowały ciągłość, nie wystąpiły gapy,
timeouty, błędy dekodowania, retry ani backpressure. Node A, B, C, D, G i H
pracują z firmware `0.5.6`; E i F nie są jeszcze podłączone.

Gotowe są:

- `sensor-host validate`, `discover` i `run`;
- wielokanałowy runtime z bounded queues i kontrolowanym shutdownem;
- Capture WAL, Time WAL, recovery, walidacja i eksport offline;
- izolacja awarii kanału, ponowne dołączanie node i polityka TimeSync;
- lokalny profil systemu Raspberry Pi ze stabilnymi nazwami portów;
- przykład usługi systemd dla rejestratora.

Warstwa operatora nadal pochodzi w większości ze starego hosta. Natywny
`hostctl doctor` czyta już profil i Runtime Status v1, natomiast dashboard nadal
czyta stare pliki statusu i HDF5. Usługa v2 nie jest jeszcze zainstalowana i
uruchamiana automatycznie.

## 2. Docelowe doświadczenie operatora

Po ukończeniu tej roadmapy typowa obsługa ma wyglądać tak:

```text
./hostctl doctor
./hostctl status
./hostctl start
./hostctl stop
./hostctl restart
./hostctl logs
```

Po restarcie Raspberry Pi rejestrator uruchamia się automatycznie, jeżeli jego
konfiguracja i storage są bezpieczne. Brak pojedynczego node nie zatrzymuje
pozostałych kanałów. Operator widzi w dashboardzie stan usługi, stan każdego
kanału i node, tempo próbek, błędy, ostatnią synchronizację czasu, zajętość
storage oraz ostatnie zdarzenia. Pierwsza wersja dashboardu steruje całym
rejestratorem: Start, Stop i Restart. Operacje per node i per kanał powstaną
później na właściwym Operations API.

Zdalny dostęp odbywa się przez prywatną sieć administracyjną, obecnie
Tailscale/SSH. Dashboard nie będzie wystawiony bez uwierzytelnienia do
publicznego Internetu.

## 3. Granice odpowiedzialności

Warstwa operacyjna nie może wejść na drogę krytyczną próbek:

| Element | Odpowiedzialność |
| --- | --- |
| `sensor-host` | RS485, transakcje, trwałość próbek i czasu, health domenowy |
| status runtime | kompaktowy snapshot bieżącego stanu zapisany atomowo |
| `hostctl` | lokalne polecenia operatora, diagnostyka i czytelne exit codes |
| systemd | start przy boot, restart procesu, sygnały i kolejność zależności |
| dashboard | odczyt statusu i bezpieczne żądanie operacji operatorskich |
| journald | log procesu i historia zdarzeń operacyjnych |
| narzędzia offline | walidacja Capture, eksport, raporty i analiza |

Dashboard nie odpytuje RS485 i nie otwiera plików aktywnego WAL na żywo.
Odczytuje opublikowany snapshot. `doctor` również nie przejmuje portów od
działającego rejestratora; głęboki test magistrali jest dozwolony po zatrzymaniu
usługi albo przez przyszłe Operations API.

## 4. Kolejność implementacji

### Etap 1 — kontrakt statusu hosta v2 — ukończony

Najpierw `sensor-host` zacznie publikować wersjonowany snapshot JSON w
`/run/sensor-system/host-v2-status.json`. Zapis będzie atomowy, ograniczony
rozmiarem i niezależny od częstotliwości próbek.

Snapshot obejmie co najmniej:

- wersję schematu, wersję hosta, PID, Linux `boot_id` i identyfikator procesu;
- digest konfiguracji i czas aktualizacji według zegara UTC oraz monotonicznego;
- stan procesu: starting, recording, stopping, degraded, faulted;
- stan każdego kanału i node, wykrytą tożsamość oraz oczekiwany output ODR;
- liczniki próbek, gapów, timeoutów, błędów ramek, retry i reconnect;
- backpressure storage i synchronizacji czasu;
- durable/released cursor oraz świeżość ostatnich danych;
- pojemność i wolne miejsce Capture, Time WAL i przyszłego Archive;
- przyczynę ostatniego błędu oraz informację o kontrolowanym shutdownie.

Status jest obserwacją. Nie staje się drugim właścicielem stanu kanału ani
kursora. Aktualizacja nie może blokować workera RS485 ani storage writera.

Kryterium ukończenia: test jednostkowy schematu i zapisu atomowego, poprawny
snapshot podczas startu, pracy, degradacji i shutdownu oraz brak mierzalnej
zmiany tempa akwizycji.

Implementacja publikuje schema 1 atomowym `rename`, eksportuje health, storage,
metryki kanałów, identity oraz cursory i świeżość per node. Błąd publikacji po
starcie uruchamia kontrolowany Stop. Testy jednostkowe i pełny workspace Rust
przechodzą.

Gate na Raspberry Pi 5 z liniami A, B, C, D, G i H przeszedł 60 sekund oraz
powtórny 30-sekundowy przebieg po poprawce projekcji shutdownu. Oba zakończyły
się czystym Stop i walidacją wszystkich sześciu drzew Capture, bez gapów,
timeoutów, błędów dekodowania, retry, reconnect i backpressure. Końcowy status
ma sześć kanałów `stopped`, sześć node, sześć trwałych strumieni czasu, spójne
cursory i nie pozostawia pliku tymczasowego. Narzut pozostał na poziomie
baseline: maksymalne RSS `2592 KiB`, a CPU wyniosło `0.54 s` dla 60 sekund i
`0.35 s` dla 30 sekund pracy.

Pierwsze podejście do przebiegu wykryło pojedynczy `NodeDeadline` kanału A po
udanym discovery; kolejny discovery i oba przebiegi produkcyjne były czyste.
Runtime Status poprawnie zachował tę próbę jako `faulted`. Odporność startu na
chwilowo nieobecny node pozostaje jawnym wymaganiem Etapu 3 i kwalifikacji z
Etapu 7.

### Etap 2 — natywny `hostctl doctor` dla v2 — ukończony

`./hostctl doctor` rozpozna profil v2 i wykona dwie klasy kontroli.

Kontrole bezpieczne podczas pracy:

- składnia i semantyka konfiguracji, unikalność kanałów, node i portów;
- obecność urządzeń oraz stabilnych aliasów udev;
- prawa użytkownika usługi do portów i katalogów;
- dostępność i typ filesystemu, wolne miejsce oraz próg rezerwy;
- stan usług systemd i świeżość snapshotu;
- synchronizacja czasu systemowego;
- zgodność wersji programu, schematu statusu i profilu;
- health kanałów na podstawie snapshotu.

Kontrole głębokie po zatrzymaniu rejestratora:

- discovery Identity/Capabilities po RS485;
- zgodność hardware ID, board revision, firmware, protokołu i output ODR;
- walidacja ostatnich segmentów Capture i Time WAL;
- test zapisu, `fsync` i atomowego rename na docelowym storage.

Polecenie otrzyma tryby `--json`, `--offline` i `--deep`. Wyjście tekstowe ma
być użyteczne dla człowieka, JSON dla dashboardu i automatyzacji. Exit code
rozróżni wynik OK, ostrzeżenie oraz błąd blokujący start. Głęboki test odmówi
otwarcia zajętego portu.

Przy tej zmianie `hostctl` przestanie wymagać starego środowiska Python przed
rozpoznaniem polecenia. Natywne komendy v2 będą działały nawet bez legacy venv.

Kryterium ukończenia: `hostctl doctor` działa na Raspberry Pi z sześcioma
obecnymi liniami, nie oczekuje niekonfigurowanych jeszcze E/F, wykrywa celowo
zepsuty port, brak praw, stary status i mało miejsca, nie przerywając
rejestracji.

Implementacja jest oddzielnym modułem domenowym i binarium `sensor-doctor`.
`hostctl` wybiera je przed kontrolą legacy Python venv. Dostępne są stabilne
findings, JSON schema 1 i kody wyjścia `0/1/2`. Tryb domyślny nie otwiera
RS485; `--deep` sprawdza stan usługi oraz właścicieli deskryptorów w `/proc`
przed discovery.

Gate na Raspberry Pi 5 potwierdził tryb offline i odczyt świeżego statusu w
trakcie 45-sekundowego nagrania na sześciu liniach. Doctor nie zwiększył
liczników błędów, timeoutów, retry ani backpressure, a recorder zakończył się
czystym Stop. Równoległy `--deep` odmówił discovery i wskazał PID właściciela
wszystkich sześciu portów. Po zatrzymaniu deep discovery zweryfikowało sześć
node, test `fsync/rename` przeszedł, 12 strumieni Capture objęło 91 okien i
942219 próbek, a 12 Time WAL zawierało 648 poprawnych rekordów. Pusty,
niezapieczętowany WAL z poprzedniego testu został poprawnie rozpoznany jako sam
nagłówek bez próbek.

Kontrolowane profile potwierdziły kod `2` i właściwy finding dla brakującego
portu, braku praw oraz rezerwy większej od wolnego miejsca. Stary status jest
błędem dla fazy aktywnej i ostrzeżeniem dla zatrzymanego procesu. Pozostałe
ostrzeżenia instalacji są zgodne z roadmapą: usługa systemd nie jest jeszcze
zainstalowana, a katalog checkpoint powstanie przy uruchomieniu retencji.

### Etap 3 — automatyczne procesy na Raspberry Pi — w realizacji

Powstanie instalowalna konfiguracja systemd dla:

1. `sensor-host-v2.service` — rejestrator;
2. `sensor-host-dashboard-v2.service` — dashboard;
3. opcjonalnego małego pomocnika sterowania z wąskimi uprawnieniami.

Pierwszy przyrost jest gotowy dla recordera: `hostctl install-services` buduje
i waliduje release, instaluje artefakt identyfikowany wersją i SHA-256, zachowuje
symlinki `current/previous` i generuje jednostkę z rzeczywistych ścieżek.
`hostctl start`, `stop`, `restart`, `status` i `logs` mają jeden adapter do
systemd. Jednostka klasyfikuje stałe błędy konfiguracji/storage osobno od
przejściowego błędu bootstrapu, ogranicza restart i zachowuje 120 sekund na
kontrolowany shutdown. Szablon oraz instalator przeszły lokalny test renderu,
checksumy i atomowej publikacji; bramka parsera systemd i lifecycle pozostaje
do wykonania na Raspberry Pi.

Instalator/deployer ustawi użytkownika i grupy, katalogi, prawa, konfigurację,
limity journald oraz zależność od właściwego mountpointu danych. `ExecStartPre`
wykona szybką walidację, ale nie będzie otwierał magistral drugi raz. SIGTERM
uruchomi kontrolowany Stop i domknięcie WAL.

Polityka restartu rozróżni awarię procesu od trwałego błędu konfiguracji lub
storage. Ma zapobiec pętli szybkich restartów. Brak jednego node pozwoli
uruchomić system w stanie degraded. Brak bezpiecznego storage, błędna
konfiguracja lub nieobsługiwany format zatrzyma start fail-closed.

Wdrożenie ma być powtarzalne i zawierać:

- build lub dostarczenie wersjonowanego artefaktu z checksumą;
- walidację profilu przed przełączeniem;
- atomowe przełączenie wersji i prosty rollback programu;
- zachowanie konfiguracji, Capture i Time WAL przy aktualizacji;
- `hostctl install-services`, `start`, `stop`, `restart`, `status` i `logs`;
- automatyczny start po boot i poprawny shutdown systemu.

Kryterium ukończenia: zimny boot uruchamia system bez sesji SSH, restart procesu
i całego Raspberry Pi zachowuje ciągłość jawną w WAL, a `systemctl stop` zawsze
kończy się kontrolowanym shutdownem w wyznaczonym czasie.

### Etap 4 — dashboard v2 w trybie obserwacji

Na początek wykorzystamy istniejący interfejs strony, lecz dodamy osobny adapter
danych v2. Adapter czyta wyłącznie wersjonowany snapshot i bezpiecznie
przefiltrowane wpisy journald. Stary model supervisor/worker, pliki poleceń i
HDF5 nie będą źródłem danych dla v2.

Pierwszy ekran pokaże:

- stan usługi, czas pracy, wersję programu i świeżość statusu;
- kafel każdego kanału/node z ODR, samples/s, cursorami i ostatnimi danymi;
- gapy, błędy, timeouty, retry, reconnect i backpressure;
- jakość oraz wiek ostatniej synchronizacji czasu;
- rozmiar danych, wolne miejsce i oszacowany czas do zapełnienia;
- ostatnie ważne zdarzenia oraz wynik ostatniego `doctor`;
- listę zakończonych okien Capture z wynikiem walidacji.

Podgląd wykresu surowych próbek i przegląd Archive pozostaną oznaczone jako
niegotowe, dopóki nie powstanie bezpieczny adapter do Capture/Archive. Awaria
dashboardu nie może wpłynąć na recorder.

Kryterium ukończenia: dashboard odświeża dane bez odpytywania RS485 i bez wzrostu
backpressure, poprawnie pokazuje status stale/offline, a zatrzymanie dashboardu
nie zmienia akwizycji.

### Etap 5 — minimalne bezpieczne sterowanie

Pierwsza wersja sterowania udostępni wyłącznie operacje całego rejestratora:

- Start;
- Stop z kontrolowanym domknięciem;
- Restart;
- uruchomienie `doctor` i odczyt wyniku.

Dashboard pozostanie procesem bez uprawnień roota. Polecenia przejdą przez
wąską lokalną granicę sterowania do systemd. Każde żądanie otrzyma operation ID,
będzie idempotentne, zapisane w audycie i zwróci wynik terminalny. Dashboard nie
będzie budował poleceń powłoki z danych HTTP.

Dostęp do operacji będzie chroniony uwierzytelnieniem w prywatnej sieci,
autoryzacją oraz ochroną żądań POST. Interfejs będzie domyślnie nasłuchiwał na
localhost albo adresie Tailscale, nie na wszystkich interfejsach publicznych.

Kryterium ukończenia: wielokrotne kliknięcie nie duplikuje operacji, Stop czeka
na domknięcie, każda operacja ma audit trail, a użytkownik bez uprawnienia nie
może zmienić stanu usługi.

### Etap 6 — zabezpieczenia pracy 24/7

Przed długim testem potrzebne są jeszcze elementy, których nie zastąpi sam
dashboard:

- pomiar rzeczywistego przyrostu bajtów na godzinę dla aktualnego profilu;
- konfigurowalne progi ostrzeżenia i krytycznej rezerwy storage;
- fail-closed przed utratą możliwości trwałego zapisu;
- limity i rotacja journald, aby logi nie zapełniły systemu;
- monitor świeżości statusu oraz zewnętrzny test żywotności usługi;
- sprawdzenie NTP/chrony i pracy wyłącznie w UTC dla czasu kalendarzowego;
- pakiet diagnostyczny bez próbek i sekretów: wersje, config digest, status,
  wynik doctor, metryki i ograniczony fragment logów;
- raport po każdym kontrolowanym i niekontrolowanym restarcie;
- jawny horyzont retencji do czasu uruchomienia Archive i dysku M.2.

Na obecnej karcie/systemowym storage test ma mieć określony limit czasu i
rezerwę. Rzeczywisty horyzont wyliczymy z pomiaru bajtów na godzinę, zamiast
opierać go na samym formacie próbki. Automatyczne usuwanie Capture nie zostanie
włączone, dopóki checkpoint pokrycia Archive nie dowiedzie, że dane zostały
bezpiecznie zarchiwizowane.

Kryterium ukończenia: system wcześnie sygnalizuje brak miejsca, nie usuwa jedynej
kopii danych, a support bundle wystarcza do zdalnej diagnozy typowej awarii.

### Etap 7 — kwalifikacja do testu ciągłego

Testy będą rosnąć stopniowo: 1 godzina, 6 godzin, 24 godziny, a po analizie
wyników 72 godziny. Każdy etap rozpoczyna `hostctl doctor` i kończy walidacja
Capture/Time WAL oraz raport zasobów.

W kwalifikacji wykonamy również kontrolowane błędy:

- odłączenie i powrót jednego node;
- odłączenie całego konwertera USB-RS485;
- brak node podczas boot i jego późniejszy powrót;
- SIGTERM, awaria procesu i restart systemu;
- opóźniony storage oraz osiągnięcie progu małej ilości miejsca;
- chwilowy brak synchronizacji czasu systemowego;
- zatrzymanie i restart dashboardu;
- po przygotowaniu osobnej procedury: utrata zasilania.

Warunki zaliczenia 24 godzin:

- brak ukrytych braków próbek; każda luka ma rekord przyczyny i zakres;
- cursory received, durable i released pozostają spójne;
- brak trwałego backpressure i brak rosnącej kolejki;
- stabilne RSS, CPU, I/O wait i latency `fsync`;
- oczekiwane samples/s na każdym aktywnym node;
- status nie staje się stale, a dashboard nie wpływa na transfer;
- powrót pojedynczego node nie restartuje zdrowych kanałów;
- po restarcie recovery nie duplikuje ani nie ukrywa danych;
- wszystkie zakończone okna przechodzą walidację.

Po zaliczeniu 24 godzin Raspberry Pi może zostać pozostawione jako stanowisko
zdalnego rozwoju. Test 72-godzinny potwierdzi trend zasobów i politykę storage.

## 5. Kolejny etap po uruchomieniu 24/7

Te elementy są częścią planu produktu, ale nie blokują pierwszego nadzorowanego
testu 24/7:

- pełne Operations API z Maintenance i operacjami per node/per kanał;
- dynamiczne dodawanie konfiguracji E/F i docelowy rejestr tożsamości;
- bezpieczny podgląd wybranych próbek na żywo;
- godzinny compactor Capture → Archive;
- uploader Archive na zewnętrzny serwer i stan publikacji w dashboardzie;
- miesięczna retencja po potwierdzeniu checkpointów pokrycia;
- migracja danych na M.2 SSD i ponowny benchmark;
- alerty o awarii, braku miejsca i długim offline;
- test wielu node na jednej magistrali i pełny gate ośmiu kanałów.

## 6. Decyzja o najbliższej pracy

Etapy 1 i 2 są zamknięte. Najbliższą implementacją jest **Etap 3: automatyczne
procesy na Raspberry Pi**. Systemd użyje istniejących kontraktów statusu i
doctor, bez dodawania drugiego modelu stanu. Po ustabilizowaniu lifecycle usługi
podłączamy dashboard w trybie obserwacji.
