# Ręczny test pionu RS485

- Status: zaliczony na jednym node protocol 5 przez RS485
- Ostatnia aktualizacja: 2026-10-10

## Zakres

Test sprawdza jeden rzeczywisty pion:

```text
RS485 discovery -> fail-closed handshake -> Start -> Measurement Pull
-> trwały zapis i sync -> Release -> Stop -> Maintenance
```

Komunikacja odbywa się wyłącznie przez wskazany port konwertera RS485 przy
115200 bit/s. USB podłączone bezpośrednio do node może służyć do zasilania, ale
nie wolno przekazać jego portu jako `DEVICE`.

Skrypt używa polecenia `capture-cycle`, które zapisuje dane przez produkcyjny
`CaptureSegmentStream`: wersjonowany WAL, CRC32C, `sync_data`, rotację i atomowe
manifesty. Dla szybkiego testu mechaniki ustawia dwusekundowe okna. Nie zmienia
to konfiguracji node ani domyślnego, około dziesięciominutowego profilu systemu.

## Przygotowanie

Potrzebne są:

- ścieżka portu konwertera RS485, na przykład `/dev/cu.usbserial-0001`;
- adres node;
- 16-znakowy hardware ID z wcześniejszego `identify`;
- fizycznie potwierdzona rewizja płytki, obecnie `2`.

Port można znaleźć bez otwierania urządzenia:

```bash
ls -1 /dev/cu.usbserial* /dev/cu.usbserial-* 2>/dev/null
```

## Uruchomienie

Z katalogu głównego repozytorium:

```bash
./host_v2/tools/run_manual_rs485_hil.sh \
  /dev/cu.usbserial-0001 \
  1 \
  0011223344556677 \
  2 \
  2500 \
  48
```

Ostatnie dwa argumenty są opcjonalne: liczba próbek i maksymalny rozmiar chunku.
Skrypt buduje release `sensor-hil`, wykonuje discovery, handshake i pełny cykl
nagrania przez produkcyjny Capture. Przy każdym wyjściu próbuje sprowadzić node
do `Maintenance`.

Każde uruchomienie tworzy osobny katalog:

```text
var/tmp/manual-rs485-hil-YYYYMMDDTHHMMSSZ/
  transcript.log
  capture/
    window-00000000000000000000.wal
    window-00000000000000000000.manifest
    ...
```

## Kryterium zaliczenia

Końcowy raport musi zawierać:

- `gaps=0` dla danych należących do tej sesji;
- dokładnie jeden końcowy `ContinuityBreak(ProcessingReset)` wytworzony przez
  kontrolowane `Stop`;
- identyczne cursory `received`, `durable` i `released`;
- stan końcowy `Maintenance`;
- brak błędów decode i timeoutów w stabilnym teście lokalnym;
- co najmniej dwa zweryfikowane logicznie okna Capture;
- komunikat `PASS` skryptu.

Runner odczytuje `StreamState` przed `Start` i przyjmuje jego `produced` jako
cursor początkowy nowego nagrania. Dane pozostawione przez wcześniejszą,
zakończoną sesję nie są więc fałszywie raportowane jako luka bieżącego testu.

Każdy późniejszy gap pozostaje prawidłowo zapisanym wynikiem protokołu, ale oznacza
niezaliczenie testu stabilnego stanowiska i wymaga zachowania całego katalogu
wynikowego do analizy.

## Time Service

Firmware 0.5.6 można sprawdzić niezależną, fail-closed serią exchange:

```bash
host_v2/target/release/sensor-hil time-probe \
  /dev/cu.usbserial-0001 1 0011223344556677 2 8 1000 115200
```

Argumenty `8` i `1000` oznaczają liczbę obserwacji oraz odstęp w milisekundach.
Komenda wymaga reklamowanego Time Service, sprawdza identity i `boot_epoch`,
raportuje surowe `t1..t4`, path delay i uncertainty oraz kończy się błędem przy
timeoutie albo błędzie dekodowania. Kilkusekundowy wynik służy do sprawdzenia
pionu wire; nie jest pomiarem dryfu oscylatora.

## Wynik referencyjny

Fizyczny przebieg z 2026-10-09 na board revision 2 i firmware 0.5.5:

- 2505 nowych próbek i 10 zweryfikowanych okien;
- `gaps=0`;
- jeden oczekiwany `ProcessingReset` po `Stop`;
- `received=durable=released=5273`;
- `decode_errors=0`, `transaction_timeouts=0`;
- stan końcowy `Maintenance`.

Pierwsza wersja runnera błędnie zaczynała od cursora zero i raportowała dane
pozostawione przez wcześniejszą sesję jako `JournalOverflow`. Poprawiony runner
odczytuje `StreamState`, wiąże go z boot epoch potwierdzonym przez handshake i
zaczyna nowe Capture od aktualnego `produced`.

Pełny przebieg około 10 minut przy nominalnym output ODR 125 Hz, wykonany tego
samego dnia na firmware 0.5.5:

- 75 007 próbek w jednym około dziesięciominutowym oknie;
- zakres cursora `[93375, 168383)`, z jednym końcowym markerem
  `ProcessingReset`;
- `gaps=0`;
- `received=durable=released=168383`;
- `decode_errors=0`, `transaction_timeouts=0`;
- stan końcowy `Maintenance`;
- niezależna walidacja i eksport CSV zakończone powodzeniem.

Artefakt lokalny znajduje się w
`var/tmp/manual-rs485-hil-20261009T133720Z`. Nie jest częścią repozytorium.

Po aktualizacji A/B 0.5.5 → 0.5.6 przez RS485 wykonano również:

- osiem obserwacji Time bez decode errors i timeoutów, path delay
  12,7–17,8 ms;
- Capture 2505 próbek w czterech pięciosekundowych oknach, `gaps=0`;
- ciągłe granice cursorów `[0,635)`, `[635,1270)`, `[1270,1905)` i
  `[1905,2506)`;
- niezależny eksport 2505 próbek do CSV i jeden oczekiwany końcowy
  `ProcessingReset`.
