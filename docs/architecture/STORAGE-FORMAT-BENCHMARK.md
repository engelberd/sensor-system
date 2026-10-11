# Benchmark formatów Capture i Archive

- Status: pierwszy benchmark syntetyczny; decyzja Archive pozostaje warunkowa
- Ostatnia aktualizacja: 2026-10-08
- Narzędzie: `host_v2/python/tools/storage_benchmark.py`

## Pytanie

Benchmark rozdziela dwa problemy:

1. Capture musi szybko i crash-safe przyjmować rekordy podczas nagrywania.
2. Archive ma minimalizować długoterminowy rozmiar i dać się walidować ze stałą
   pamięcią.

Każdy wariant zapisuje ten sam deterministyczny strumień signed-24 XYZ.
Walidator czyta ograniczone bloki, rekonstruuje dokładne próbki i porównuje
SHA-256 z wejściem. Każdy format jest uruchamiany w osobnym procesie, dzięki
czemu peak RSS nie kumuluje wcześniejszych bibliotek.

## Znaleziona przyczyna problemu legacy

Stary compactor HDF5 utrzymuje `seen: set[(boot_epoch, sample_seq)]` dla każdej
próbki całej doby. Tworzy też listy segmentów, quality overlays i kandydatów
timing przed ich zapisem. Zbiór jest O(liczba próbek), choć uporządkowany
strumień wymaga tylko oczekiwanego cursora O(1).

Przy 250 Hz jeden sensor produkuje 21,6 mln próbek na dobę. Pythonowy zbiór
milionów tuple może zużyć wielokrotnie więcej pamięci niż same wartości XYZ.
To jest główna wada algorytmu; samo HDF5 nie wyjaśnia wzrostu RAM wraz z liczbą
sensorów i długością dnia.

W v2 walidator utrzymuje tylko aktualny `(hardware_id, boot_epoch,
expected_cursor)`, stan bieżącego bloku i inkrementalny hash. Liczba sensorów nie
może automatycznie zwiększać liczby równoległych compactorów.

## Kandydaci

- `wal`: packed int24, nagłówek i CRC każdego bloku, okresowy `fsync`;
- `packed`: minimalny nieskompresowany payload 9 B/próbkę;
- `gzip`/`zstd`: kompresja interleaved packed int24;
- `gzip-shuffle`/`zstd-shuffle`: int32 z byte shuffle przed kompresją;
- `hdf5-gzip`: int32, HDF5 shuffle, gzip 4 i Fletcher32.

Czwarty bajt int32 jest rozszerzeniem znaku. Po byte shuffle kompresuje się
prawie całkowicie, dlatego format int32 może być mniejszy po kompresji niż
bezpośrednio skompresowane 9 B/próbkę.

## Wynik dzienny

Profil `vibration`, jeden sensor, 125 Hz, 10,8 mln próbek, bloki 16 384:

| Format | Rozmiar | B/próbkę | Czas zapisu | Walidacja | Peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| packed int24 | 97,20 MB | 9,000 | 0,75 s | 0,05 s | 49,5 MiB |
| int32 shuffle + gzip | 63,90 MB | 5,917 | 3,19 s | 0,45 s | 48,7 MiB |
| int32 shuffle + zstd 3 | **53,41 MB** | **4,946** | **0,70 s** | **0,29 s** | 49,4 MiB |
| HDF5 int32 shuffle + gzip 4 | 64,26 MB | 5,950 | 2,50 s | 0,43 s | 76,5 MiB |

W tym profilu prosty zstd+shuffle jest o około 17% mniejszy od HDF5+gzip,
zapisuje około 3,6× szybciej i zużywa około 27 MiB mniej peak RSS. Liczby
obejmują interpreter Python i NumPy; ważne są porównanie oraz brak wzrostu RAM
z długością nagrania.

## Wpływ rodzaju sygnału

Dziesięć minut, 125 Hz, bloki 16 384:

| Profil | zstd+shuffle | HDF5+gzip | packed int24 |
| --- | ---: | ---: | ---: |
| quiet | 3,188 B/próbkę | 3,460 B/próbkę | 9,000 B/próbkę |
| vibration | 5,677 B/próbkę | 6,076 B/próbkę | 9,000 B/próbkę |
| noise | 7,180 B/próbkę | 7,017 B/próbkę | 9,000 B/próbkę |

Zstd wygrywa dla sygnału spokojnego i typowego. Dla syntetycznego silnego szumu
gzip/HDF5 jest około 2,3% mniejszy. Decyzja wymaga powtórzenia na rzeczywistych
Capture; lokalny katalog nagrań był pusty podczas tego benchmarku.

## Rozmiar bloku i RAM

Godzina 250 Hz, profil `vibration`:

| Blok | zstd B/próbkę | zstd peak RSS | HDF5 B/próbkę | HDF5 peak RSS |
| ---: | ---: | ---: | ---: | ---: |
| 4 096 | 5,101 | 40,4 MiB | 6,191 | 63,2 MiB |
| 16 384 | 5,004 | 52,1 MiB | 5,956 | 69,0 MiB |
| 65 536 | 4,897 | 87,1 MiB | 5,851 | 93,7 MiB |

16 384 jest rozsądnym punktem startowym dla compactorów: daje większość zysku
kompresji bez skoku pamięci bloku 65 536. To profil wykonawczy, nie część formatu;
reader musi obsłużyć inne rozmiary.

## Capture WAL

Test 10 minut przy 250 Hz i rekordzie 48 próbek dał 9,583 B/próbkę, czyli około
6,5% narzutu względem packed int24. Ucięcie siedmiu bajtów końcowego rekordu
zostało wykryte przez długość/CRC, a recovery zatrzymało się na poprzednim
pełnym cursorze. WAL jest produktem tymczasowym, więc ten narzut jest akceptowalny
w zamian za prosty append i jednoznaczne obcięcie fizycznie niepełnego ogona.
Pełny rekord z błędnym CRC nie jest automatycznie usuwany: recovery zatrzymuje
się fail-closed, ponieważ rekord mógł wcześniej zostać uznany za durable.

Czas `fsync` zmierzony na komputerze deweloperskim nie reprezentuje Raspberry Pi
z SD/SSD. Group commit i liczba writerów muszą zostać dobrane osobnym benchmarkiem
na docelowym nośniku. `DurableReceipt` nie może zostać wystawiony przed fsync
obejmującym dany rekord.

## Wniosek roboczy

- Capture v2: prosty append-only WAL z packed int24, per-record CRC i recovery
  przez skan do pierwszego niepełnego rekordu; pełna korupcja jest błędem
  wymagającym jawnej interwencji.
- Archive v1 kandydat: blokowy int32 byte-shuffle + zstd, SHA-256 produktu oraz
  osobne małe strumienie segmentów, gapów, czasu, konfiguracji i provenance.
- Compactor: domyślnie jeden worker, maksymalnie konfigurowalne dwa; jeden blok
  na worker i brak per-sample set/list.
- HDF5: adapter eksportowy lub alternatywny Archive, jeżeli prawdziwe sygnały
  albo wymagania narzędziowe przeważą nad rozmiarem i kosztem.

Decyzji Archive nie zamrażamy przed benchmarkiem prawdziwych danych oraz testem
fault injection formatu blokowego. Podział Capture/Archive jest już uzasadniony:
WAL wygrywa prostotą recovery, a zstd+shuffle rozmiarem długoterminowym.

Jednostka produktu jest już decyzją niezależną od formatu: jedna godzina UTC na
sensor, domykana na podstawie durable watermarku i publikowana przez osobny,
trwały outbox. Szczegóły opisuje
[ARCHIVE-AND-PUBLICATION-POLICY.md](ARCHIVE-AND-PUBLICATION-POLICY.md).

## Uruchomienie

```bash
PYTHONDONTWRITEBYTECODE=1 host/.venv/bin/python \
  host_v2/python/tools/storage_benchmark.py \
  --duration-s 600 --odr 125 --profile vibration
```
