# ADR-0007: język runtime hosta i model procesu

- Status: accepted
- Date: 2026-09-28

## Kontekst

Host ma równolegle obsłużyć 1–16 kanałów, wiele node na half-duplex RS485,
trwały zapis i recovery. Docelowa platforma to 64-bit Raspberry Pi OS na
Raspberry Pi 4 lub 5, z 2–16 GB RAM oraz SD albo SSD. System ma być oszczędny,
ale nie projektujemy pod jedną sztywną wartość pamięci.

Dotychczasowy host w Pythonie łączy transport, politykę transferu i zapis. Proces
Pythona per kanał powiela interpreter i biblioteki oraz utrudnia jawne limity
pamięci. Jednocześnie Python i jego biblioteki pozostają wartościowe przy pracy
z zapisanymi danymi.

## Decyzja

1. Firmware node pozostaje w C++17 z Pico SDK.
2. Krytyczny runtime `host_v2` implementujemy w stabilnym Rust dla ARM64 Linux.
3. Python pozostaje językiem narzędzi offline: analizy, walidacji, migracji i
   diagnostyki zapisanych danych. Recorder nie uruchamia interpretera Python.
4. Pierwsza wersja używa jednego procesu `sensor-host`. Każdy channel ma własny
   logiczny `ChannelAgent`, ograniczone kolejki, jednego właściciela I/O i osobny
   stan zdrowia.
5. Jeśli test blokady sterownika, pomiary opóźnień lub wymagania izolacji to
   uzasadnią, osobny proces obejmie cały `TransportDevice`. Model domeny i
   komunikaty pozostaną bez zmian.
6. UI jest cienkim klientem lokalnego Operations API i nie uczestniczy w
   gwarancji odbioru ani trwałego commit.
7. Host i node współdzielą specyfikację oraz binarne golden vectors, a nie kod
   domenowy przez FFI.

## Początkowy podział kodu

Workspace zaczyna od trzech crate'ów:

- `sensor-core`: typy i reguły domeny bez I/O;
- `sensor-protocol`: kontrakt wire i kodeki bez polityki produktu;
- `sensor-host`: composition root, runtime i adaptery.

Nowy crate lub trait powstaje dopiero wtedy, gdy istnieje rzeczywista granica
zależności. Trait jest uzasadniony przez drugi adapter, fake w teście zachowania
albo konieczność odizolowania zewnętrznego I/O.

## Konsekwencje

- jedna binarka ogranicza narzut pamięci i upraszcza deployment;
- typy, ownership i jawne błędy pomagają kontrolować stan współbieżny bez garbage
  collectora;
- awaria procesu nadal może dotknąć wiele kanałów, dlatego runtime wymaga
  nadzoru zewnętrznego, crash recovery i trwałych cursorów;
- izolacja zadań nie zastępuje timeoutów dla operacji systemowych ani testów
  sterowników USB/serial;
- HDF5 nie jest z góry wymagany w gorącej ścieżce; adapter Capture zostanie
  wybrany po pomiarze trwałości, kosztu flush i recovery;
- na każdej klasie Raspberry Pi mierzymy CPU, RSS, I/O wait, najgorszy czas
  trwałego zapisu i backlog. Admission control używa wyników dla konkretnej
  instalacji, zamiast kodować jeden globalny budżet sprzętowy.
