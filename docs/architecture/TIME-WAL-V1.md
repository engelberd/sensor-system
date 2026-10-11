# Time WAL v1

- Status: format, reader/writer, router wielu strumieni, bounded worker,
  threaded executor, recovery niepewnego appendu, startup, konfiguracja i
  health completion zaimplementowane; symulacja Time Service i podstawowy HIL
  jednego node są gotowe
- Ostatnia aktualizacja: 2026-10-10

## Cel

Time WAL zachowuje surowe obserwacje `t1..t4` potrzebne do późniejszego
odtworzenia relacji node monotonic → host monotonic. Nie przechowuje wyliczonego
offsetu, dryfu ani UTC. Są to wyniki modelu, które można ponownie policzyć.

Jeden plik należy dokładnie do jednego strumienia:

```text
(hardware_id, node_address, node_boot_epoch, host_clock_id)
```

`host_clock_id` identyfikuje uruchomienie zegara monotonicznego hosta. Na
docelowym Linuxie powinien pochodzić z `/proc/sys/kernel/random/boot_id`.
Zapobiega to łączeniu liczników monotonicznych sprzed i po restarcie Raspberry
Pi. Ścieżka pliku nie jest źródłem tej tożsamości.

## Format

Nagłówek ma 64 bajty i zawiera magic, wersję, rozmiar rekordu, pełną tożsamość
strumienia oraz CRC32C. Rekord ma stałe 36 bajtów:

| Pole | Rozmiar |
| --- | ---: |
| `t1` host send, µs | 8 B |
| `t2` node receive, µs | 8 B |
| `t3` node send, µs | 8 B |
| `t4` host receive, µs | 8 B |
| CRC32C poprzednich pól | 4 B |

Identyfikatory nie powtarzają się w rekordach. Numer rekordu wynika z jego
pozycji. Sto obserwacji zajmuje 3664 bajty wraz z nagłówkiem. Nawet częsta
synchronizacja daje pomijalny strumień względem próbek, dlatego CRC każdego
rekordu jest lepszym wyborem niż bardziej złożone bloki kontrolne.

## Trwałość i recovery

`TimeWalSink::append_batch` waliduje cały batch przed zapisem i wykonuje jedno
`sync_data`. Po niepewnym błędzie zapisu uchwyt jest poisoned i wymaga ponownego
otwarcia. Recovery:

1. waliduje CRC oraz pełną tożsamość nagłówka;
2. czyta rekordy stałej długości i waliduje CRC, `boot_epoch`, składnię
   `ClockObservation` oraz monotoniczność wszystkich czterech liczników;
3. obcina wyłącznie niepełny ogon krótszy niż 36 bajtów;
4. pełny rekord z błędnym CRC zatrzymuje recovery zamiast uznać go za ogon.

Czytnik jest read-only i nigdy nie naprawia pliku. `TimeRepository` utrzymuje
stały zestaw strumieni jednego `host_clock_id`, odrzuca duplikaty, obcą epokę i
nieznaną tożsamość zamiast tworzyć plik podczas recording.

`TimePersistenceWorker` odbiera rekordy z bounded queue poza pętlą kanału i
emituje osobne completion trwałości. Po pierwszym błędzie zatrzymuje konsumpcję
i zachowuje dokładnie jeden niepewny rekord. Ponownie otwarte repozytorium
rozstrzyga go przez licznik stałych rekordów:

- poprzedni licznik: rekord jest nieobecny i zostaje ponowiony;
- licznik większy o jeden: rekord jest już trwały i nie wolno go dopisać drugi
  raz;
- każdy inny wynik: recovery zatrzymuje się fail-closed.

Samo przyjęcie przez bounded queue nadal oznacza handoff, a nie zapis na dysku.
`TimePersistenceExecutor` uruchamia worker na nazwanym wątku, udostępnia
klonowalne submittery dla wielu kanałów, przekazuje bounded completion i kończy
pracę kontrolowanie nawet wtedy, gdy klon submittera pozostaje żywy.
`TimeRuntime` otwiera i odzyskuje komplet WAL-i przed uruchomieniem executora.
Zachowuje snapshot liczników durable dla health oraz specyfikacje potrzebne do
jawnego reopen/recovery po błędzie persistence. `host_clock_id` pochodzi z
`/proc/sys/kernel/random/boot_id`; ścieżka pliku nie jest źródłem tożsamości.
`TimePersistenceHealth` zachowuje niepewny rekord po błędzie, wymaga jawnego
recovery i uznaje usługę za zdrową dopiero po kolejnym poprawnym durable
receipt. Niespójność strumienia lub licznika jest fail-closed.

## Granice odpowiedzialności

- pętla kanału wykonuje tylko nieblokujące przekazanie obserwacji przez klon
  submittera;
- `TimeRepository` wybiera skonfigurowany strumień, a worker wykonuje I/O;
- Time WAL odpowiada za bajty, CRC, barierę trwałości i recovery;
- `ClockModel` odpowiada za offset, dryf i niepewność;
- osobna warstwa powiąże host monotonic z UTC.
