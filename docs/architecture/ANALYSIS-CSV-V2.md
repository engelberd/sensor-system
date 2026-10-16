# Analysis CSV v2

- Status: główny format analityczny dla pomiarów drgań
- Ostatnia aktualizacja: 2026-10-16

## Cel i źródło prawdy

`Analysis CSV v2` jest prostym widokiem dla MATLAB-a, notebooków i systemów
zewnętrznych. Nie zastępuje Capture WAL i nie uczestniczy w
`fsync -> Release`. Eksporter czyta wyłącznie zamknięte okna, waliduje WAL
względem trwałego manifestu i może być bezpiecznie ponawiany. Awaria eksportu
nie zatrzymuje ani nie opóźnia RS485.

Domyślne `storage.window_seconds = 600` daje około dziesięciu minut danych od
rzeczywistej pierwszej próbki. Chunk przekraczający próg nie jest dzielony,
więc okno może być nieznacznie dłuższe. Nie wyrównujemy go do pełnej minuty.

## Publikowane pliki

Dla każdego zamkniętego okna powstają trzy pliki o wspólnym rdzeniu:

```text
line_b-node_1-11:10-16.10.2026.csv
line_b-node_1-11:10-16.10.2026.events.csv
line_b-node_1-11:10-16.10.2026.json
```

Czas w nazwie jest lokalnym, minutowym przybliżeniem czasu pierwszej próbki.
Jeżeli restart lub krótki segment spowoduje kolizję nazwy w tej samej minucie,
eksporter dodaje wyłącznie techniczny suffix `-w<ordinal>`, zamiast nadpisywać
gotowy produkt.

Główny CSV zawiera tylko:

```text
x_m_s2,y_m_s2,z_m_s2
```

Kolejność wierszy jest kolejnością próbek. Kody signed ADXL355 są przeliczane
na `m/s²` przy użyciu zakresu z trwałej historii konfiguracji, standardowego
przyspieszenia `9.80665 m/s²` i typowej czułości cyfrowej producenta:

| Zakres | Czułość nominalna |
| --- | ---: |
| ±2 g | 256000 LSB/g |
| ±4 g | 128000 LSB/g |
| ±8 g | 64000 LSB/g |

Źródłem wartości jest tabela „Digital Output for the ADXL355” w
[ADXL354/ADXL355 Data Sheet Rev. D](https://www.analog.com/media/en/technical-documentation/data-sheets/adxl354_adxl355.pdf).
Eksporter odrzuca okno mieszające różne ODR lub zakresy, ponieważ taki plik bez
kolumny czasu nie byłby samowystarczalnym, jednoznacznym szeregiem.

Plik `.json` jest obowiązkowym opisem CSV i zawiera między innymi:

- nazwę pliku danych, kanał, node, hardware ID, `boot_epoch` i ordinal okna;
- dokładny `first_device_time_us` z monotonicznego zegara node;
- oszacowany czas pierwszej próbki w UTC i czasie lokalnym, metodę oraz
  niepewność, jeśli można ją policzyć;
- ustawiony `configured_odr_hz` oraz niezależnie wyznaczony
  `estimated_odr_hz`;
- zakres, nominalną czułość, stałą g i wzór konwersji;
- liczbę próbek, brakujących numerów i przerwań ciągłości.

Czas kalendarzowy jest rekonstruowany z Time WAL `node -> CLOCK_BOOTTIME`, a
następnie z bieżącej relacji `CLOCK_BOOTTIME -> UTC`. Jeżeli odpowiedni Time
WAL z tego samego bootu hosta nie jest dostępny, eksporter używa jawnie
oznaczonego fallbacku: czas publikacji manifestu minus dokładny interwał
monotoniczny okna. `first_device_time_us` pozostaje dokładnym dowodem źródłowym
niezależnie od jakości czasu kalendarzowego.

Plik `.events.csv` zachowuje luki i continuity breaks. Ponieważ główny CSV nie
ma numeru próbki, zdarzenie zawiera `row_index` wskazujący miejsce w tablicy
danych oraz `source_sample_number` służący do audytu Capture.

## Automatyzacja i konfiguracja

`analysis-export` skanuje tylko skonfigurowane, aktywne tożsamości. Najpierw
publikuje zdarzenia i JSON, a główny CSV przez atomowy rename publikuje jako
ostatni znacznik ukończenia produktu. Kursor eksportu jest odczytywany z
metadanych JSON, więc uproszczona nazwa nie usuwa idempotencji ani informacji o
`boot_epoch` i ordinalu.

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
interwał. Ręczne wykonanie tej samej operacji:

```bash
./hostctl analysis export-new
```

Moduł `host_v2/analysis/load_analysis_window.py` jest małym elementem używanym
przez notebooki analityczne. Znajduje trzy pliki na podstawie ścieżki głównego
CSV, sprawdza ich zgodność oraz tworzy wektor czasu z wyznaczonego ODR. Można
go również uruchomić bezpośrednio do szybkiej kontroli okna:

```bash
python host_v2/analysis/load_analysis_window.py \
  'var/analysis/line-b/line_b-node_1-11:36-16.10.2026.csv'
```

Minimalny notebook startowy znajduje się w
`host_v2/analysis/analysis_window.ipynb`. Własny notebook może użyć loadera
bez kopiowania logiki formatu plików:

```python
from host_v2.analysis.load_analysis_window import load_window

window = load_window("line_b-node_1-11:36-16.10.2026.csv")
acceleration = window["samples"]  # NumPy array: columns x, y, z [m/s²]
time_s = window["time_s"]
metadata = window["metadata"]
assert window["is_continuous"]
```

Skrypt wymaga tylko `numpy`. Kończy się kodem `0` dla ciągłego okna, `2` dla
jawnej luki lub continuity break oraz `1` dla niespójnych plików. Wektor czasu
uwzględnia brakujące próbki opisane przez zdarzenia `gap`.

Przy pierwszym uruchomieniu eksporter bierze tylko skonfigurowaną liczbę
najnowszych zamkniętych okien bieżącego `boot_epoch`. Capture z poprzednich
epok pozostaje dostępny do jawnego eksportu offline.

## Migracja z v1

Pliki v1 z surowymi osiami pozostają wyłącznie starym produktem pochodnym.
Capture WAL nadal zawiera pełny materiał źródłowy, więc katalog Analysis można
bezpiecznie odbudować w v2 bez zmiany lub usuwania Capture. Po wdrożeniu v2 na
Raspberry Pi stary katalog Analysis jest czyszczony i eksporter publikuje
najnowsze zamknięte okno każdej aktywnej linii w nowym formacie.

## Dowód wdrożenia

Wdrożenie na Raspberry Pi 2026-10-16 opublikowało po jednym najnowszym oknie
B/C/D/G/H, łącznie pięć produktów v2. Każde miało zero brakujących próbek.
Przykładowe okno B ma nazwę `line_b-node_1-11:36-16.10.2026.csv`, 75273
próbki, ustawione 125 Hz, wyznaczone 125.440668 Hz oraz czas pierwszej próbki
`2026-10-16T11:36:53.332153+02:00` z oszacowaną niepewnością 2593 µs.
Pierwsze wiersze miały fizycznie poprawny rząd wartości, w tym około
`9.90 m/s²` na osi obciążonej grawitacją. Powtórny eksport nie utworzył
duplikatów. Stary, odtwarzalny katalog v1 o rozmiarze 1,3 GB został usunięty;
Capture WAL pozostał bez zmian. Po operacji recorder, timer i pięć kanałów
pozostały aktywne, a Doctor zwrócił 34 OK, zero ostrzeżeń i błędów.
