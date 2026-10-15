# Analysis CSV v1

- Status: tymczasowy format analityczny zaimplementowany
- Ostatnia aktualizacja: 2026-10-16

## Cel i źródło prawdy

`Analysis CSV v1` jest prostym widokiem dla MATLAB-a, notebooków i ręcznej
kontroli nagrań. Nie zastępuje Capture WAL i nie uczestniczy w
`fsync -> Release`. Eksporter czyta wyłącznie zamknięte okna, najpierw
waliduje WAL względem trwałego manifestu i może być bezpiecznie ponawiany.
Awaria eksportu nie zatrzymuje ani nie opóźnia RS485.

Okna pochodzą bezpośrednio z polityki Capture. Domyślne
`storage.window_seconds = 600` oznacza około dziesięciu minut od rzeczywistej
pierwszej próbki. Chunk przekraczający próg nie jest dzielony, więc długość
pliku może być nieznacznie większa. Nie wyrównujemy okien do pełnych minut.

## Publikowane pliki

Dla każdego zamkniętego okna powstają trzy pliki o wspólnym rdzeniu nazwy:

```text
device-t00000000123456789012us_ch02_node001_<hardware>_boot<epoch>_w<ordinal>.csv
device-t00000000123456789012us_ch02_node001_<hardware>_boot<epoch>_w<ordinal>.events.csv
device-t00000000123456789012us_ch02_node001_<hardware>_boot<epoch>_w<ordinal>.json
```

Pierwsza wartość jest dokładnym oszacowaniem czasu pierwszej próbki w zegarze
monotonicznym node, w mikrosekundach od jego startu. Nie jest UTC. Obecny
system zachowuje dowody `node monotonic -> host monotonic`, lecz trwała warstwa
`host monotonic -> UTC` nie jest jeszcze gotowa. Nazwa celowo mówi
`device-t...us`, aby nie przedstawiać czasu pliku lub zaokrąglonej minuty jako
czasu pomiaru. Po dodaniu dowodu UTC można rozszerzyć nazwę bez zmiany Capture.

Główny CSV ma kolumny:

```text
sample_number,device_time_us,x_raw,y_raw,z_raw,output_odr_hz
```

- `sample_number` jest źródłowym numerem próbki, więc pozwala łączyć kolejne
  pliki i wykrywać przerwy;
- `device_time_us` pochodzi z timing evidence i interpolacji wewnątrz chunku;
- `x_raw`, `y_raw`, `z_raw` są surowymi kodami signed ADXL355;
- `output_odr_hz` pochodzi z trwałej historii `config_revision`, a nie z
  aktualnego pliku konfiguracyjnego hosta.

Plik `.events.csv` zachowuje luki i continuity breaks. Plik `.json` zawiera
tożsamość, liczniki jakości oraz `estimated_odr_hz` wyliczone ze średniego
odstępu między sąsiednimi, kolejnymi próbkami. Dzięki temu skonfigurowany ODR i
ODR obserwowany nie są mylone.

## Automatyzacja i konfiguracja

`analysis-export` skanuje tylko skonfigurowane, aktywne tożsamości. Publikacja
głównego CSV następuje jako ostatni atomowy rename; jego obecność jest
znacznikiem ukończonego produktu. Kolejny przebieg pomija taki plik.

Konfiguracja produktu znajduje się w `storage.analysis_v2`:

```json
{
  "enabled": true,
  "root_dir": "var/analysis",
  "scan_interval_seconds": 60,
  "initial_backfill_windows": 1
}
```

Timer `sensor-analysis-export-v2.timer` uruchamia proces jednokrotny co podany
interwał. Ręczne wykonanie tej samej, idempotentnej operacji:

```bash
./hostctl analysis export-new
```

Przy pierwszym uruchomieniu eksporter bierze tylko podaną liczbę najnowszych
zamkniętych okien bieżącego `boot_epoch`. Zapobiega to przypadkowemu
rozwinięciu całej historycznej bazy Capture do wielokrotnie większego tekstu.
Najwyższy opublikowany ordinal w nazwach CSV jest następnie kursorem: kolejne
przebiegi eksportują wszystkie późniejsze okna po kolei. Capture z poprzednich
epok pozostaje dostępny do jawnego eksportu offline.
