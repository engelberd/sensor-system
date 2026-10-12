# Host Doctor v2

## Cel i granica odpowiedzialności

`sensor-doctor` jest natywnym narzędziem diagnostycznym hosta v2. `./hostctl
doctor` uruchamia je bez zależności od starego środowiska Python. Doctor
obserwuje konfigurację, system operacyjny, Runtime Status, storage i — tylko na
wyraźne żądanie — urządzenia RS485. Nie jest właścicielem runtime ani kanału.

Domyślny status znajduje się w
`/run/sensor-system/host-v2-status.json`.

## Tryby

```text
./hostctl doctor
./hostctl doctor --offline
./hostctl doctor --deep
./hostctl doctor --json
```

- tryb domyślny wykonuje bezpieczne kontrole read-only podczas pracy;
- `--offline` sprawdza scaloną konfigurację, urządzenia i storage, pomijając
  systemd, Runtime Status oraz synchronizację zegara;
- `--deep` wymaga nieaktywnej usługi i dowodu, że żaden skonfigurowany port nie
  jest otwarty przez inny proces. Dopiero wtedy wykonuje discovery po RS485,
  próbę `write + fsync + rename + directory fsync`, walidację zapieczętowanych
  prefiksów Capture i pełny odczyt Time WAL. Niezapieczętowany WAL z rekordami
  pozostaje widocznym ostrzeżeniem; pusty plik zawierający sam nagłówek nie jest
  utratą próbek;
- `--offline` i `--deep` wzajemnie się wykluczają.

Doctor nie zatrzymuje recordera i nie przejmuje zajętego portu. Głęboki test
odmawia pracy, jeżeli `sensor-host-v2.service` jest aktywny albo `/proc`
wskazuje właściciela któregokolwiek endpointu.

## Kontrole domyślne

Narzędzie sprawdza:

- pełną semantykę profilu v2 i fingerprint scalonego dokumentu;
- wyłączone oraz aktywne kanały, stabilne aliasy urządzeń, typ urządzenia
  znakowego i prawa bieżącego użytkownika;
- katalogi Capture, checkpoint i Time WAL, prawa zapisu, typ filesystemu,
  wolne miejsce i rezerwę;
- stan jednostki systemd i synchronizację czasu systemowego;
- rozmiar, JSON i schema Runtime Status;
- zgodność systemu, fingerprintu, wersji hosta i protokołów;
- Linux `boot_id`, świeżość monotoniczną, lifecycle procesu i health;
- obecność kanałów i node, identity, board revision, output ODR;
- porządek `released <= durable <= received <= produced`;
- krytyczne liczniki transportu, storage i backpressure.

Kontrole są projekcją faktów. Nie modyfikują konfiguracji ani cursorów.

## Wynik i automatyzacja

| Kod | Znaczenie |
| --- | --- |
| `0` | wszystkie wykonane kontrole są poprawne |
| `1` | co najmniej jedno ostrzeżenie, brak błędu blokującego |
| `2` | błąd blokujący bezpieczny start albo wiarygodny nadzór |
| `64` | błędne argumenty CLI |
| `69` | `hostctl` nie znalazł zbudowanego binarium |

`--json` publikuje schema `1`, tryb, wynik zbiorczy, kod wyjścia, podsumowanie
i uporządkowaną listę findings. Dashboard może użyć raportu bez parsowania
tekstu przeznaczonego dla operatora.

Ścieżki i limity można przekazać jawnie:

```text
./hostctl doctor \
  --config host/system_config.json \
  --status-file /run/sensor-system/host-v2-status.json \
  --service sensor-host-v2.service \
  --status-max-age 30
```

## Interpretacja chwilowego błędu startu

`doctor --deep` wykonuje jedno kompletne discovery zgodne z produkcyjnym
kontraktem. `NodeDeadline` pozostaje błędem i jest raportowany bez ukrywania go
automatyczną pętlą retry. Polityka uruchomienia recordera z chwilowo nieobecnym
node jest osobnym kontraktem Etapu 3; doctor dostarcza dowód potrzebny do jej
oceny.
