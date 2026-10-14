# Kontrakt operatorski `hostctl` dla hosta v2

- Status: obowiązujący
- Ostatnia aktualizacja: 2026-10-14

`hostctl` jest cienkim, lokalnym adapterem do binariów Rust hosta v2,
narzędzi danych v2 oraz systemd. Nie zawiera logiki akwizycji i nie otwiera
RS485 poza jawnymi poleceniami `discover`, `doctor --deep`, `node` i `update`.

## Dostępny interfejs

| Obszar | Polecenia |
| --- | --- |
| Build i profil | `build`, `validate`, `paths [--init]` |
| Recorder | `discover`, `run` |
| Diagnostyka | `doctor [--offline\|--deep\|--json]` |
| Operations IPC | `channel status\|maintenance\|resume --channel ID` |
| Dane | `capture validate`, `capture export` |
| Node protocol 5 | `node identify\|handshake\|diagnostics\|time-probe\|safe-stop\|restart\|prepare-update\|confirm-trial` |
| Firmware | `update` |
| systemd | `install-services`, `rollback-services`, `start`, `stop`, `restart`, `status`, `logs` |

Wspólne wartości domyślne to `host/system_config.json`, katalog repozytorium
jako kotwica ścieżek, `/run/sensor-system/host-v2-status.json` oraz
`/run/sensor-system/operations-v1.sock`. Można je zmienić zmiennymi
`SENSOR_HOST_CONFIG`, `SENSOR_HOST_STATUS_FILE` i
`SENSOR_HOST_OPERATIONS_SOCKET` albo jawnymi opcjami binariów.

`channel maintenance` i `channel resume` wysyłają typowane żądanie do
działającego procesu i czekają na wynik terminalny workera. Opcjonalny
`--operation-id` służy do ponowienia dokładnie tej samej operacji. CLI nie
otwiera portu RS485.

## Aktualizacja node

`hostctl update` wymaga jawnej tożsamości sprzętu, rewizji płytki i oczekiwanej
wersji:

```bash
./hostctl stop
./hostctl update \
  --port /dev/sensor-system-line-b \
  --node 1 \
  --hardware-id e6ab60a60e1506b3 \
  --board-revision 2 \
  --expected-firmware 0.5.7
```

Polecenie kolejno wykonuje `PrepareFirmwareUpdate` w application protocol 5,
upload do nieaktywnego slotu A/B i `ConfirmTrialBoot` dla dokładnej wersji.
Jeszcze przed otwarciem portu porównuje wersję i rewizję płytki z manifestem.
Nie używa legacy command `0x03` ani legacy inventory. Aktywna usługa systemd
blokuje operację. Po przerwanym uploadzie operator może wznowić tę samą,
sprawdzoną sesję przez `--resume-bootloader`; opcja pomija wyłącznie pierwszy
krok.

Transport bootloadera jest obecnie ostatnim, celowo odizolowanym elementem
Python używanym przez V2. Odpowiada tylko za bootloader wire i wybór slotu.
Wejście do bootloadera oraz weryfikacja aplikacji należą do protocol 5.

## Wycofany interfejs

Komendy `ping`, `config`, `lab`, `recorder`, `archive`, `verify`,
`supervisor`, `console`, `dashboard`, `operator`, `boot` i `py` należą do
starego hosta i zwracają błąd. Ich implementacja pozostaje tymczasowo w `host/`
jako materiał migracyjny, źródło testów danych i bootloader uploader. Nie jest
specyfikacją V2.

Brakujące funkcje mają otrzymać nowe odpowiedzialności zamiast aliasów do
legacy. Codeki i runtime Configuration/Commissioning są już gotowe, lecz nie są
jeszcze publicznymi poleceniami `hostctl` bez Operations API:

- pozostałe Operations API per node: konfiguracja, commissioning i update;
- dashboard czytający Runtime Status v1;
- godzinny compactor Capture → Archive i publisher;
- natywny uploader bootloadera, po ustabilizowaniu kontraktu A/B.

Do czasu implementacji tych elementów `hostctl` ma jawnie odmówić operacji,
zamiast uruchamiać niezgodny model stanu.
