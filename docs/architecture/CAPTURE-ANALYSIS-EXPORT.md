# Odczyt Capture w MATLAB-ie i Jupyterze

- Status: Analysis HDF5 v4
- Aktualizacja: 2026-11-11

Capture WAL jest źródłem danych recordera. Automatyczny eksporter tworzy
jeden samodzielny HDF5 dla zapieczętowanego okna, zawierający surowe XYZ w jednej tabeli, indywidualne czasy, konfigurację, zdarzenia jakości oraz temperaturę.
Kontrakt i ścieżki datasetów: [Analysis HDF5 v6](ANALYSIS-HDF5-V6.md).

```bash
./hostctl analysis export-new
host/.venv/bin/python host/runtime/analysis/load_analysis_window.py /sciezka/okno.h5
```

Pierwszy przebieg eksportuje skonfigurowany ogon każdej epoki. Pełną historię
można odtworzyć przez `./hostctl analysis export-new --backfill-all`.
Wyniki trafiają bezpośrednio do `analysis_v2.root_dir`, domyślnie `var/analysis`,
w układzie `YYYY-MM-DD/CHANNEL/`, np.
`2026-11-13/line-b/line-b_2026-11-13_16-38.h5`.
Eksport nie używa RS485 i nie zmienia WAL.

Notebook: [analysis_window.ipynb](../../host/runtime/analysis/analysis_window.ipynb).
MATLAB:

```matlab
path = "/sciezka/okno.h5";
samples = h5read(path, '/samples');
t = samples.czas_us;
x = samples.x;
odr_hz = h5readatt(path, '/', 'configured_odr_hz');
time_s = double(t - t(1)) / 1e6;
plot(time_s, x);
```

Przed analizą częstotliwościową sprawdź luki, przerwania ciągłości i ODR.
Czas node jest monotoniczny; kotwica UTC w metadanych ma jawną niepewność.

`capture-export` pozostaje technicznym narzędziem do diagnostyki WAL;
jego tabele CSV nie są produktem Analysis i nie są generowane przez timer.
Stare przykłady analiz oparte na companion CSV zostały wycofane.
