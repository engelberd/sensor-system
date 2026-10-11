# Symulacja pionu Measurement host v2

- Status: deterministyczna bramka logiczna gotowa
- Ostatnia aktualizacja: 2026-10-08
- Zakres: jeden half-duplex channel, wiele node, Measurement protocol 5

## Cel

Symulacja sprawdza połączenie modułów, a nie tylko każdy moduł osobno:

```text
ChannelRecordingLoop
  -> MeasurementCoordinator
  -> ChannelRuntime / ChannelScheduler
  -> ChannelAgent / TransactionCore
  -> ChannelEndpoint / ChannelPort
  -> framing v3 bytes
  -> SimulatedRs485Bus
  -> SimulatedNodeLink
  -> response bytes
  -> DurableMeasurementSink
```

Wymiana przechodzi przez prawdziwy encoder, inkrementalny decoder, transaction
ID, timeout, quarantine, request/response Measurement i cursory trwałości.
Symulator nie wywołuje metod domenowych hosta bokiem.

## Model

`SimulatedClock` daje jeden kontrolowany czas monotoniczny. Każdy
`SimulatedWireNode` produkuje próbki z dokładnego `SampleRate`, przechowuje je w
bounded journalu i implementuje semantykę `from`, `release_before`, retencji
oraz boot epoch przez `SimulatedNodeLink`.

Magistrala nalicza idealny czas 8N1 osobno dla requestu i response przy
115200 bit/s oraz 1 ms turnaround. Czas requestu upływa przed uruchomieniem
deadline odpowiedzi, tak jak po zakończeniu `write_all`/`flush`. Odpowiedź staje
się czytelna dopiero po swoim czasie wire. Profil czterech node × 125 Hz z
chunkami docelowymi 32 zajmuje więc około 58% nominalnego pasma samym ruchem
Measurement, zamiast korzystać z bezczasowej atrapy portu.

`ChannelRuntime` używa dwóch rozmiarów:

- `max_sample_chunk_samples` ogranicza jedną odpowiedź wire;
- `target_chunk_samples` określa preferowane wypełnienie przed zwykłym pull.

Po odpowiedzi `ready_at` jest liczony z output ODR i zweryfikowanego backlogu.
Jeżeli backlog już istnieje, następny pull jest gotowy od razu. Jeżeli node nie
ma danych, runtime czeka na docelowe wypełnienie, lecz nigdy dłużej niż pozwala
`maximum_service_interval - measurement_slot`.

## Automatyczne scenariusze

Test `multi_node_recording_survives_one_lost_response_and_storage_retry` używa:

- czterech node po 125 Hz;
- journala 2048 próbek na node;
- chunku maksymalnego 48 i celu 32 próbki;
- dwóch prób wire na slot;
- dziesięciu sekund czasu symulowanego;
- jednej utraconej odpowiedzi;
- jednej odpowiedzi opóźnionej poza timeout, odebranej jako `LateResponse`
  podczas quarantine;
- jednego odrzuconego durable write.

Brama wymaga, aby każdy node dostarczył co najmniej 1150 próbek, różnica między
node nie przekroczyła jednego maksymalnego chunku, nie powstała luka ani
continuity break i żaden journal nie nadpisał próbki. Błąd storage ma wykonać
ponownie ten sam zapis bez powtarzania zakończonego transferu wire.
Spóźniona odpowiedź nie może zakończyć retry ani zostać przypisana do nowego
transaction ID.

Test `offline_node_enters_probe_mode_without_starving_healthy_nodes` odłącza
jeden z czterech node. Po wyczerpaniu małego budżetu prób runtime przechodzi na
`offline_probe_interval`. Każdy zdrowy node musi nadal dostarczać dane bez
overflow, a liczba prób do odłączonego urządzenia pozostaje ograniczona.

Test `empty_pull_waits_for_target_fill_without_missing_service_deadline`
sprawdza arytmetykę pacingu bez float. Dla 32 próbek przy 125 Hz następny zwykły
pull staje się gotowy po 256 ms.

Uruchomienie:

```bash
cargo test --workspace --manifest-path host_v2/Cargo.toml
```

## Czego ta bramka nie potwierdza

Symulacja nie modeluje długości przewodu, odbić, bias/termination, zakłóceń,
jitteru i dodatkowego turnaround konkretnego konwertera USB–RS485, opóźnień
systemu operacyjnego ani rzeczywistego czasu `fsync`. Nie zastępuje też
admission dla wybranego profilu instalacji. Te własności wymagają HIL oraz
pomiarów przy 115200 bit/s.

Nie są jeszcze częścią pełnej pętli wykonawcy Time, Health i Diagnostic ani
produkcyjny Capture z crash recovery. Ich kandydatury muszą wejść do tego
samego schedulera i zachować jednego właściciela transakcji na channel.

## Następna granica

Logika codziennego transferu Measurement jest gotowa do podłączenia
produkcyjnego Capture. Kolejny pion powinien dodać append-only zapis z recovery,
bounded handoff i konfigurowalne okna około 600 s liczone od czasu pierwszej
próbki. Następnie można zasilać scheduler kandydaturami Time/Health/Diagnostic i
powtórzyć macierz w HIL wielu node.
