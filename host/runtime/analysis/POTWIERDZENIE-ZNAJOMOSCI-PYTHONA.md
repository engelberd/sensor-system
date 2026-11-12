# Odczyt okna Analysis w Pythonie

`load_analysis_window.py` odczytuje samodzielny plik HDF5 przez `h5py`.
Zwraca jedną tabelę próbek surowych, dokładne czasy uint64, metadane, zdarzenia
oraz temperaturę. Sprawdza długości kolumn, kolejność czasów, zakresy i liczniki.

Przykład: `window = load_window(Path("data/window.h5"))`.
Szczegóły formatu: [Analysis HDF5 v6](../../../docs/architecture/ANALYSIS-HDF5-V6.md).
Notebook pokazuje przebiegi i analizę częstotliwościową. Przed analizą należy
sprawdzić `is_continuous`, jawne luki i konfigurację ODR.
