# Handoff następnej sesji na Raspberry Pi

Status na 2026-10-12. Ten dokument jest krótkim punktem wejścia do dalszej
pracy. Decyzje architektoniczne pozostają w ADR i dokumentach wskazanych niżej;
handoff nie zastępuje ich i nie tworzy drugiego planu produktu.

## Źródła prawdy

Przed zmianami przeczytaj:

1. `AGENTS.md`;
2. `docs/architecture/HOST-V2-USER-ADOPTION-ROADMAP.md`;
3. `docs/architecture/IMPLEMENTATION-ROADMAP.md`, szczególnie sekcję 9;
4. `docs/architecture/ACQUISITION-QUALITY-AND-DIAGNOSTICS.md`;
5. `host_v2/DEPLOY-RASPBERRY-PI.md` i `node/README-bootloader.md`.

Obowiązują nadal główne granice: jeden produkt na `main`, `./sgit` zamiast
`git`, operacje firmware po RS485 z kontrolą board revision, bounded bufory,
jedna aktywna transakcja na kanał, brak diagnostyki na hot path próbek oraz
brak zmian w nieśledzonych plikach `hardware/`.

## Stan repozytorium i Raspberry Pi

- Zatwierdzony commit: `0ae9757` (`fix(node): avoid system time header
  collision`).
- Poprzedni commit `6ee2364` dodaje read-only Diagnostics Service v3,
  `sensor-hil diagnostics` i firmware node `0.5.7`.
- Repozytorium Raspberry Pi: `/home/anone/pico-projects`.
- Raspberry Pi pobrało `0ae9757`.
- Świeży linuxowy build `sensor-hil` przeszedł.
- Świeży linuxowy build pełnej paczki node `0.5.7` board rev 2 przeszedł po
  zmianie nazwy `protocol_v3/time.h` na `time_service.h`.
- Paczka aktualizacyjna:
  `node/build/releases/sensor-system-node-v0.5.7/sensor-system-node-v0.5.7-update-package.json`.
- `sensor-host-v2.service` działa. Linie B, C, D, G i H nagrywają. Linia A jest
  odizolowana jako `faulted`, a jej port nie jest zajęty przez recorder.
- Read-only dashboard hosta v2 korzysta z Runtime Status v1 i bounded journald,
  nie otwiera RS485 ani aktywnego Capture WAL. Lokalna jednostka
  `sensor-system-dashboard.service` udostępnia go wyłącznie na adresie
  Tailscale Raspberry Pi `100.88.176.26:8090`; nie nasłuchuje na publicznym ani
  lokalnym interfejsie LAN.
- E i F nie są fizycznie podłączone.
- Na Pi istnieją lokalne, nieśledzone profile debug. Nie dodawaj ich do Git.

## Incydent linii A

Node A:

- port `/dev/sensor-system-line-a`;
- address `1`;
- hardware ID `8d626ee02dcea999`;
- board revision `2`;
- firmware `0.5.6`, protocol `3/5`;
- stan `Faulted/InvalidSample`;
- jedenaście próbek pozostających w RAM journalu zostało świadomie uznane za
  dane stanowiska testowego i może zostać porzucone.

Capture przed awarią został zweryfikowany. Nie było błędów RS485 ani storage.
Błąd ujawnił dopiero wymuszony drain FIFO podczas `Stop`. Zainstalowany firmware
0.5.6 odrzuca `prepare-update` w stanie `Faulted`; jest to ostatnia wersja bez
zdalnej ścieżki ratunkowej. Firmware 0.5.7 dopuszcza wejście do bootloadera oraz
jawny restart także z `Faulted`. Inwentaryzacja Raspberry Pi potwierdziła, że
USB noda nie jest do niego podłączone; widoczny jest tylko konwerter RS485.
Obecne A odzyska się więc przy najbliższym rzeczywistym zaniku zasilania.
Trwały rekord diagnostyczny pozostaje w flash.

Node B, C, D, G i H działają już na `0.5.8` i mają zdalne `RestartNode`,
Diagnostics oraz wejście do bootloadera z `Faulted`. Każdy przeszedł upload A/B
i exact-version trial confirm. Host z trwałym audytem Operations nagrywa pięć
zdrowych kanałów; A jest jawnie wyłączone lokalnym override, więc runtime jest
`healthy`, a nie pozornie `degraded`. Gate pięciu kanałów zakończył się wynikiem
`hostctl doctor`: 34 OK, zero ostrzeżeń i błędów. Każdy kanał miał zero timeoutów,
decode errors, retry i storage failures.

## Najbliższa procedura

Po jednorazowym restarcie zasilania A:

1. Nie otwieraj portów B/C/D/G/H. Odczytaj A dopiero podczas zaplanowanego okna
   serwisowego.
2. Sprawdź A tylko po RS485:

   ```bash
   host_v2/target/release/sensor-hil identify \
     /dev/sensor-system-line-a 1 115200
   ```

3. Zatrzymaj usługę przez `./hostctl stop`, przełącz aplikację do bootloadera
   przez Lifecycle V3, a następnie wgraj
   przygotowaną paczkę A/B po RS485:

   ```bash
   ./hostctl update \
     --port /dev/sensor-system-line-a \
     --node 1 \
     --hardware-id 8d626ee02dcea999 \
     --board-revision 2 \
     --expected-firmware 0.5.8 \
     --image node/build/sensor-system-node-update-package.json
   ```

   Polecenie samo wykonuje protocol-5 PrepareFirmwareUpdate i odmawia pracy,
   gdy usługa recordera jest aktywna.

4. `hostctl update` natychmiast potwierdza dokładny trial boot. Osobne polecenie
   poniżej służy wyłącznie do diagnostyki albo wznowienia ręcznej procedury:

   ```bash
   host_v2/target/release/sensor-hil confirm-trial \
     /dev/sensor-system-line-a 1 8d626ee02dcea999 2 0.5.8 115200
   ```

5. Po aktualizacji odczytaj diagnostykę bez zmiany lifecycle. Firmware 0.5.6
   nie reklamuje Diagnostics Service, więc przed aktualizacją polecenie ma
   prawo jawnie odmówić:

   ```bash
   host_v2/target/release/sensor-hil diagnostics \
     /dev/sensor-system-line-a 1 8d626ee02dcea999 2 115200
   ```

6. Zachowaj w dokumentacji wnioski i wartości potrzebne do diagnozy, ale nie
   dodawaj surowego transcriptu do Git.
7. Dopiero po poprawnym HIL wykonaj kontrolowany restart hosta, aby A mogła
   ponownie dołączyć do produkcyjnego runtime. Zweryfikuj `./hostctl doctor` i
   spójność cursorów wszystkich kanałów.

Jeżeli A po restarcie nadal raportuje `Faulted`, nie próbuj aktualizacji w
pętli. Najpierw odczytaj Identity i stan, zachowaj dowód i ustal, czy fault
powstał ponownie przy inicjalizacji sensora.

## Kolejność dalszej implementacji

Po odczytaniu dokładnego rekordu A:

1. napraw przyczynę `InvalidSample` na podstawie danych, bez zgadywania;
2. propaguj wynik regularnego `AcquisitionEngine::update()` do health/runtime,
   aby błąd nie czekał na `Stop`;
3. dodaj prostą bounded politykę przed resetem: próba drainu, a gdy odzyskanie
   jest niemożliwe, jawny gap albo continuity break; awaria jednego kanału nie
   może blokować innych;
4. domknij częściowy bootstrap i późniejsze dołączanie brakującego kanału bez
   restartu całego procesu;
5. kontynuuj roadmapę operatora: obserwacyjny dashboard, minimalne sterowanie,
   zabezpieczenia 24/7 i kwalifikację 1 h → 6 h → 24 h → 72 h;
6. Archive, uploader, M.2, wiele node na jednej linii i pełny gate ośmiu
   kanałów pozostają kolejnymi etapami, nie warunkami tej diagnozy.

Diagnostics Service pozostaje pasywny: odpowiada wyłącznie na jawne żądania,
nie publikuje okresowej telemetrii, nie zmienia lifecycle i nie ma własnej pracy
w tle. Production runtime nie odpytuje go jeszcze automatycznie.
