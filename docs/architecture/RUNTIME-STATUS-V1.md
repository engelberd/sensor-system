# Runtime Status v1

## Cel

Runtime Status jest wersjonowanym, tylko do odczytu obrazem bieżącego stanu
`sensor-host`. Stanowi wspólne źródło dla `hostctl doctor`, dashboardu i
monitoringu systemd. Nie przyjmuje poleceń, nie otwiera RS485 i nie jest źródłem
prawdy dla cursorów ani lifecycle.

Proces produkcyjny publikuje plik:

```text
/run/sensor-system/host-v2-status.json
```

Ścieżka jest zawsze przekazywana jawnie przez `sensor-host run --status-file`.
Testy ręczne mogą zapisywać status obok swoich artefaktów.

## Własności publikacji

- schemat ma pole `schema_version` równe `1`;
- każda udana publikacja zwiększa `sequence` w obrębie jednego procesu;
- `process.instance_id` rozróżnia ponowne uruchomienia;
- `process.host_boot_id` wiąże status z bootem Linuksa i Time WAL;
- zapis trafia najpierw do ograniczonego pliku tymczasowego, a potem jest
  publikowany atomowym `rename`;
- maksymalny rozmiar dokumentu wynosi 4 MiB;
- status jest odświeżany z częstotliwością istniejącego `metrics_interval_ms`;
- terminalny snapshot `stopped` albo `faulted` pozostaje po zakończeniu procesu;
- terminalne fazy kanałów i liczniki kroków pochodzą z raportów rzeczywistego
  join/shutdownu; cursory i świeżość node są ostatnią obserwacją z granicy Stop;
- po awarii procesu ostatni status może nadal mówić `recording`; konsument musi
  sprawdzić świeżość, PID, `instance_id` i stan usługi systemd.

Błąd publikacji po uruchomieniu runtime jest błędem operacyjnym. Host wykonuje
wtedy kontrolowany Stop, ponieważ pozostawienie pozornie świeżego, ale
nieaktualizowanego statusu byłoby niebezpieczne dla operatora.

## Stany procesu

| `process.phase` | Znaczenie |
| --- | --- |
| `starting` | konfiguracja jest poprawna, trwa bootstrap/recovery |
| `recording` | runtime działa, health jest healthy |
| `degraded` | runtime działa, co najmniej jeden izolowany problem jest widoczny |
| `stopping` | przyjęto Stop albo trwa obsługa błędu globalnego |
| `stopped` | kontrolowany shutdown zakończył się czysto |
| `faulted` | start, nadzór albo shutdown zakończył się błędem |

`health.level` pozostaje osobnym polem domenowym: `healthy`, `degraded` albo
`stop_required`. Dashboard nie wyprowadza health wyłącznie z koloru procesu.

## Struktura dokumentu

Najważniejsze pola najwyższego poziomu:

| Pole | Znaczenie |
| --- | --- |
| `schema_version` | wersja kontraktu JSON |
| `sequence` | kolejność publikacji jednego procesu |
| `system_name` | nazwa instalacji z profilu |
| `software` | wersja hosta i protokołów |
| `config` | algorytm oraz fingerprint scalonego profilu |
| `process` | PID, instancja, boot, faza i czasy |
| `health` | wynik czystej projekcji `HostHealthSnapshot` |
| `storage` | root, rezerwa, wolne miejsce albo typ awarii |
| `channels` | lifecycle workera i kumulacyjne liczniki kanału |
| `nodes` | identity, ODR, cursory i świeżość per node |
| `time_streams` | liczba trwałych obserwacji per Time WAL |
| `last_error` | terminalny lub bieżący błąd procesu, jeśli wystąpił |

Czas kalendarzowy jest przekazywany jako Unix milliseconds. Czas monotoniczny
jest przekazywany jako mikrosekundy od `CLOCK_BOOTTIME`. Wiek ostatniego pomiaru
i TimeSync jest obliczany tylko w tej samej osi monotonicznej.

Fingerprint `fnv1a64` służy do wykrywania zmiany scalonej konfiguracji. Nie jest
mechanizmem bezpieczeństwa ani podpisem konfiguracji.

## Cursory node

Każdy node ma atomowy snapshot:

- `received` — prefiks przyjęty z protokołu;
- `durable` — prefiks potwierdzony przez Capture storage;
- `released` — prefiks, którego zwolnienie przekazano node;
- `produced` — ostatni cursor produkcji zaobserwowany w odpowiedzi node.

Snapshot aktualizuje ten sam `ChannelWorkCoordinator`, który posiada
`NodeSession`. Atomiki są wyłącznie kopią obserwacyjną; dashboard i status nie
mogą nimi sterować ani przesuwać sesji. Czytelnik statusu nigdy nie blokuje
workera RS485.

## Reguły konsumenta

`doctor` i dashboard muszą:

1. odrzucić nieobsługiwaną wersję schematu;
2. sprawdzić, czy `system_name` i fingerprint pasują do aktywnego profilu;
3. porównać `host_boot_id` z aktualnym bootem;
4. sprawdzić świeżość `updated_monotonic_us` względem skonfigurowanego interwału;
5. potwierdzić PID i stan jednostki systemd;
6. traktować regresję `sequence` jako nową instancję tylko wtedy, gdy zmienił
   się `instance_id`;
7. traktować brak pola, `null` lub nieznany enum fail-closed dla funkcji
   sterujących.

Pola telemetrii node są opcjonalne. Po pierwszym poprawnym i przyjętym do
bounded persistence odczycie zawierają `temperature_c`, `temperature_raw`,
`telemetry_device_time_us`, `last_telemetry_monotonic_us` i wyliczony w tej
samej osi `last_telemetry_age_ms`. Brak tych pól oznacza brak capability
Telemetry albo oczekiwanie na pierwszy odczyt. Status czyta nieblokującą kopię;
nie odpytuje RS485 i nie zastępuje Telemetry WAL jako trwałego źródła.

Snapshot jest podstawą obserwacji. Historia zdarzeń pozostaje w journald, a
trwałe dane pomiarowe w Capture, Time WAL i Telemetry WAL.
