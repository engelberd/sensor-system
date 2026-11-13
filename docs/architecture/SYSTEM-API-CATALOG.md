# Katalog API i integracji Sensor System

- Status dokumentu: kanoniczny katalog powierzchni integracyjnych
- Ostatnia aktualizacja: 2026-11-12
- Cel: przebudowa dashboardu, integracje lokalne i przyszłe platformy zewnętrzne

Ten dokument odpowiada na trzy pytania: jakie interfejsy system udostępnia
dzisiaj, jak dojrzała jest każda funkcja oraz czego brakuje, aby traktować ją
jako stabilne API dla nowego dashboardu lub innej platformy. Szczegółowe
kontrakty pozostają w dokumentach podlinkowanych w tabelach; tutaj znajduje się
ich wspólny indeks i stan wykonania.

## Statusy

| Status | Znaczenie |
| --- | --- |
| `produkcyjne` | działa w runtime i ma potwierdzenie na rzeczywistym systemie |
| `gotowe lokalnie` | kod i testy integracyjne są gotowe, ale bramka HIL lub wdrożeniowa pozostaje otwarta |
| `częściowe` | działa tylko część kontraktu albo interfejs jest związany z obecnym UI |
| `projekt` | istnieje kontrakt lub decyzja, ale brak kompletnego adaptera |
| `brak` | potrzebna powierzchnia nie ma jeszcze kontraktu ani implementacji |

Status dotyczy całej ścieżki do konsumenta. Sam działający codec albo funkcja
domenowa nie oznacza jeszcze gotowego API HTTP.

## Model integracji

```text
dashboard / integracja / hostctl
              |
       Operator HTTP (Python)       przyszły sensor-api
              |                         |
              +---- Operations IPC -----+----> worker kanału ----> RS485/node
              |
              +---- Runtime Status JSON
              +---- Service Control IPC ----> systemd
              +---- journald

narzędzia danych ----> Analysis HDF5 / Capture WAL / Telemetry WAL
```

Najważniejsza granica: Operations IPC jest jedynym control plane działającego
hosta. Dashboard ani przyszła brama sieciowa nie mogą otwierać RS485, zmieniać
cursorów, czytać aktywnego Capture WAL ani odtwarzać lifecycle po swojej
stronie.

## Powierzchnie API

| Powierzchnia | Transport i lokalizacja | Przeznaczenie | Status | Stabilność dla nowych klientów |
| --- | --- | --- | --- | --- |
| Operations API v1 | Unix datagram, `/run/sensor-system/operations-v1.sock`, JSON do 4096 B | status i asynchroniczne polecenia channel/node | `produkcyjne` dla większości operacji | tak, jako wewnętrzny kontrakt domenowy |
| Runtime Status v1 | atomowy JSON, `/run/sensor-system/host-v2-status.json` | bieżący snapshot procesu, storage, kanałów i node | `produkcyjne` | tak, po zachowaniu walidacji świeżości |
| Service Control v1 | Unix stream, `/run/sensor-system-control/service-control-v1.sock`, JSON do 2048 B | status/start/stop/restart usług systemd | `gotowe lokalnie` | tylko przez zaufaną lokalną bramę |
| Operator HTTP | HTTP/JSON w `host/dashboard` | adapter bieżącego dashboardu | `częściowe` | nie jako publiczne API; payloady obserwacyjne są związane z UI |
| `hostctl` | CLI korzystające z tych samych kontraktów | obsługa lokalna i diagnostyka | `produkcyjne` | dobre dla operatora i skryptów lokalnych, nie dla aplikacji webowej |
| Analysis HDF5 v6 | pliki `var/analysis/YYYY-MM-DD/CHANNEL/*.h5` | kompletne okna do analizy offline, w tym TimeSync | `gotowe lokalnie`; v5 działa na rpi-sanok2 | tak dla MATLAB/Python, nie jako API czasu rzeczywistego |
| Capture/Time/Telemetry WAL | lokalne pliki runtime | źródło trwałości i recovery | `produkcyjne` | nie; to prywatny format runtime |
| Operations Audit WAL | `.operations-audit-v1/channel-NN.wal` | trwały audyt poleceń | zapis `produkcyjny`, odczyt API `brak` | nie bez read modelu |
| Protokół node | RS485, framing 3 i application protocol 6 | komunikacja host–firmware | `produkcyjne` | nie udostępniać dashboardowi ani platformom zewnętrznym |

Kontrakty źródłowe: [Operations API v1](OPERATIONS-API-V1.md),
[Operator HTTP API v1](OPERATOR-HTTP-API-V1.md),
[Runtime Status v1](RUNTIME-STATUS-V1.md) i
[Analysis HDF5 v6](ANALYSIS-HDF5-V6.md).

## Operations API v1

Każda mutacja ma niezerowy `operation_id` `u64`. Powtórzenie identycznego
requestu z tym samym ID jest idempotentne; ponowne użycie ID z inną treścią
daje konflikt. Operacje przechodzą przez `accepted/running` do
`succeeded/rejected/failed`, a wynik odczytuje się osobnym żądaniem.

| Funkcja | Target | Stan implementacji | Potwierdzenie i brakująca bramka |
| --- | --- | --- | --- |
| status kanału i aktywnej operacji | channel | `produkcyjne` | używane przez hostctl i dashboard |
| `enter_maintenance` | channel | `produkcyjne` | pełny worker-owned quiesce, Stop, drain i Maintenance |
| `resume_recording` | channel | `produkcyjne` | handshake, recovery, admission i ponowna aktywacja |
| `get_configuration` | node | `produkcyjne` | testy oraz HIL Raspberry Pi |
| `validate_configuration` | node | `gotowe lokalnie` | pełny executor i testy; brak osobnego udokumentowanego odbioru HIL |
| `apply_configuration` | node | `produkcyjne` | HIL, optimistic revision, admission i trwała historia konfiguracji |
| `reset_configuration` | node | `produkcyjne` | HIL potwierdził profil 125 Hz |
| `set_telemetry_interval` | node | `gotowe lokalnie` | działa w IPC i workerze; HTTP używa go jako części formularza konfiguracji, nie reklamuje jako osobnej funkcji UI |
| `restart_node` | node | `produkcyjne` | HIL potwierdził restart, zmianę epoki i Resume |
| `scan_unassigned` | channel | `częściowe` | pusty skan potwierdzony HIL; skan rzeczywistego nieprzypisanego node pozostaje otwarty |
| `assign_address` | channel + hardware ID | `gotowe lokalnie` | implementacja i workflow profilu gotowe; brak HIL z fabrycznie nieprzypisanym node |
| operacje grupowe | wiele targetów | `brak` | potrzebny kontrakt wyników per target i partial success |

IPC udostępnia trzy akcje: `status`, `submit` i `get`. Nie jest interfejsem
sieciowym i celowo nie zawiera uwierzytelniania; ochroną są uprawnienia lokalnego
socketu i granica procesu.

## Obecne HTTP

HTTP jest dziś cienkim adapterem dashboardu, nie kompletną bramą integracyjną.
Identyfikatory operacji na granicy HTTP są dziesiętnymi stringami, żeby
JavaScript nie utracił precyzji `u64`. Kontrakt opisuje mutujące żądania jako
JSON z nagłówkiem `X-Sensor-System-Request: dashboard-v2`. Implementacja
egzekwuje własny nagłówek i limit ciała, ale nie sprawdza jeszcze jawnie
`Content-Type: application/json`; jest to brak kontraktowy do zamknięcia.

### Endpointy operatorskie

| Metoda i ścieżka | Funkcja | Status |
| --- | --- | --- |
| `GET /api/v2/operator/capabilities` | katalog operacji i ograniczeń formularza | `gotowe lokalnie` |
| `GET /api/v2/service` | stan hosta i timera Analysis | `gotowe lokalnie` |
| `POST /api/v2/service/{start\|stop\|restart}` | sterowanie usługami przez allowlisted helper | `gotowe lokalnie`, pełna bramka wdrożeniowa otwarta |
| `GET /api/v2/channels/{id}/operations/status` | tryb kanału i aktywna operacja | `produkcyjne` |
| `POST /api/v2/channels/{id}/operations` | złożenie pojedynczej operacji | `produkcyjne` dla operacji odebranych HIL, pozostałe zgodnie z tabelą wyżej |
| `GET /api/v2/channels/{id}/operations/{operation_id}` | faza i wynik operacji | `produkcyjne` |
| `POST /api/v2/channels/{id}/commission` | Assign, zapis profilu i restart hosta | `gotowe lokalnie`, HIL nieprzypisanego node otwarty |
| `POST /api/v2/channels/{id}/nodes/{address}/reset-metrics-view` | lokalny baseline liczników UI | `gotowe lokalnie`; funkcja prezentacyjna, nie domenowa |
| `POST /api/v2/channels/{id}/nodes/{address}/rescue-restart` | awaryjny restart przy niedostępnym workerze | `gotowe lokalnie`; uprzywilejowany fallback, nie zwykłe API |

### Endpointy obserwacyjne bieżącego dashboardu

`GET /api/dashboard`, `/api/overview`, `/api/channels`, `/api/events`,
`/api/logs`, `/api/config` i `/api/health` są zaimplementowanymi projekcjami
widoku. Ich kształt jest jednak powiązany z obecnym HTML-em i nie ma osobnego
wersjonowanego JSON Schema. Mogą posłużyć jako materiał do migracji, ale nowy
dashboard nie powinien uznawać ich bez zmian za stabilne API produktu.

`/api/data`, wyszukiwanie i pobieranie danych oraz `/api/live/*` są w aktywnym
adapterze wyłączone. Obecny dashboard nie udostępnia bezpiecznego data browsera
ani podglądu surowych próbek.

### Ograniczenia bezpieczeństwa HTTP

- brak aplikacyjnego logowania, tokenów i RBAC;
- własny nagłówek POST ogranicza prosty cross-origin form POST, ale nie jest
  uwierzytelnieniem;
- brak jawnej walidacji `Content-Type` pomimo wymagania opisanego w kontrakcie;
- założeniem jest prywatna sieć administracyjna i kontrolowany bind;
- brak publicznego kontraktu TLS, CORS, rate limitu i rotacji poświadczeń;
- endpoint rescue oraz zmiana profilu nie powinny być dostępne bez osobnej roli
  administratora.

Z tego powodu obecnego serwera Python nie należy wystawiać bezpośrednio do
Internetu ani traktować jako API dla niezaufanych klientów.

## Dane i obserwowalność

| Potrzeba klienta | Co mamy | Czego brakuje |
| --- | --- | --- |
| bieżący stan | Runtime Status v1, odświeżany około 1 s | wersjonowane JSON Schema i sieciowy endpoint niezależny od widoku UI |
| metryki live | liczniki i cursory w statusie | stabilne nazwy serii, historia i exporter Prometheus/OpenTelemetry |
| alarmy i zdarzenia | health reasons, `last_error`, bounded journald | trwały read model, filtrowanie, paging i potwierdzanie alarmów |
| temperatura | ostatnia telemetria w statusie i Telemetry WAL | API historii i agregacji |
| surowe próbki live | journal node i Capture wewnątrz runtime | bezpieczny bounded adapter; nie może blokować recordera |
| pliki analityczne | HDF5 v6 w `var/analysis` | katalog produktów, walidacja, zakres czasu i kontrolowane pobieranie |
| audyt poleceń | CRC WAL per channel | API tylko do odczytu i retencja audytu |
| diagnostyka | `hostctl doctor`, status, journald | asynchroniczne uruchomienie, zapis i pobranie wyniku przez API |

## Przydatność dla innych platform

| Platforma lub klient | Stan dzisiaj | Zalecany adapter |
| --- | --- | --- |
| nowy dashboard web/mobile | możliwy tylko przez obecny adapter | wersjonowane REST/JSON + SSE, oparte na Status i Operations IPC |
| MATLAB/Python/Jupyter | gotowe offline | bezpośredni odczyt Analysis HDF5 v6 |
| Grafana | brak stabilnego źródła historii | exporter Prometheus albo OpenTelemetry Collector |
| Node-RED/Home Assistant/MQTT | brak bezpośredniej integracji | osobna brama MQTT z allowlistą komend i korelacją `operation_id` |
| SCADA/OPC UA | brak | adapter tylko do read modelu oraz jawnie dopuszczonych komend |
| centralny serwer wielu lokalizacji | brak | uwierzytelniona brama outbound, kolejka zdarzeń i identyfikator instalacji |

Platformy zewnętrzne nie powinny komunikować się bezpośrednio z node ani
montować katalogu Capture. Każdy adapter sterujący musi zachować idempotency,
asynchroniczny wynik i dokładnie te same reguły admission co Operations API.

## Braki i kolejność prac

### P0 — przed przebudową dashboardu

1. Zamrozić zewnętrzne REST API jako osobny kontrakt, niezależny od modeli
   obecnego HTML-a.
2. Dodać OpenAPI oraz JSON Schema dla statusu, błędów, capabilities i operacji;
   testować je kontraktowo w CI.
3. Ustalić uwierzytelnienie i role co najmniej `viewer`, `operator`, `admin`;
   commissioning, rescue i sterowanie usługą wymagają `admin`.
4. Ustalić politykę bind/TLS/reverse proxy, CORS, rate limitu oraz sekretów.
5. Zdefiniować politykę kompatybilności: wersja URL dla breaking changes,
   addytywne pola w obrębie wersji i okres wycofania.

### P1 — użyteczny dashboard operacyjny

1. Read model historii zdarzeń, telemetrii i metryk bez czytania aktywnego WAL.
2. Asynchroniczny `doctor` z ID zadania i trwałym wynikiem.
3. Katalog zakończonych produktów Analysis HDF5: czas, channel/node, schema,
   walidacja, rozmiar i kontrolowane pobieranie.
4. SSE dla statusu i postępu operacji; polling pozostaje bezpiecznym fallbackiem.
5. API odczytu operations audit oraz spójny katalog kodów błędów.
6. Wyliczony horyzont storage i jawne progi ostrzegawcze.

### P2 — integracje i większa skala

1. Operacje grupowe z wynikiem per target i bez fałszywego globalnego sukcesu.
2. Exporter Prometheus/OpenTelemetry i opcjonalna brama MQTT.
3. Bezpieczny bounded podgląd próbek lub downsample, niezależny od ścieżki
   trwałego zapisu.
4. Wieloinstalacyjna brama outbound, jeśli powstanie centralny panel.
5. Webhooki albo trwały outbox zdarzeń dla integracji automatycznych.

## Zalecana granica docelowa

Przebudowa dashboardu nie powinna powielać logiki obecnego serwera Python.
Docelowo warto dodać proces `sensor-api`, który:

- czyta Runtime Status v1 i przyszłe read modele;
- przekazuje polecenia wyłącznie do Operations IPC;
- używa Service Control IPC tylko dla uprawnionej roli;
- publikuje REST/JSON i SSE według OpenAPI;
- nie otwiera RS485 i nie czyta aktywnego Capture WAL;
- może obsłużyć nowy dashboard, aplikację mobilną i adaptery innych platform z
  jednego kontraktu.

Obecny dashboard może być pierwszym klientem tej bramy. Taki podział pozwala
zmienić framework UI bez zmiany lifecycle, storage i protokołu urządzeń.

## Zasada aktualizacji katalogu

Każda nowa powierzchnia integracyjna albo zmiana statusu funkcji aktualizuje ten
dokument oraz właściwy kontrakt szczegółowy. Funkcja przechodzi do
`produkcyjne` dopiero po testach kontraktowych, integracyjnych i wymaganym HIL;
sama obecność endpointu w kodzie nie wystarcza.
