# Odczyt Capture w MATLAB-ie i Jupyterze

- Status: strumieniowy eksport CSV i przykłady analizy zaimplementowane
- Ostatnia aktualizacja: 2026-10-09

## Granica odpowiedzialności

Capture WAL jest trwałym formatem technicznym recordera. MATLAB ani notebook
nie powinny dekodować go samodzielnie. `capture-export` najpierw niezależnie
waliduje każdy WAL z jego manifestem, a następnie zapisuje analizowalną kopię
CSV ze stałym zużyciem pamięci.

Eksport nie zastępuje docelowego, skompresowanego Archive. Jest narzędziem do
inspekcji, testów i pracy badawczej.

## Nagranie około dziesięciu minut

Obecny fizyczny node pracuje przy wyjściowym ODR około 125 Hz. Z katalogu
repozytorium:

```bash
./host_v2/tools/run_manual_10min_capture.sh \
  /dev/cu.usbserial-B003C07K \
  1 \
  2f13349450fa4084 \
  2 \
  125
```

Skrypt zbierze co najmniej 75 000 próbek, ustawi logiczne okno Capture na
600 sekund, niezależnie zweryfikuje WAL, utworzy katalog `analysis` i pozostawi
node w `Maintenance`. Argument `125` opisuje aktualną konfigurację node; skrypt
jej nie zmienia. Dla 250 Hz należy podać `250`, a dla 62,5 Hz `62.5`.

## Ponowny lub ręczny eksport

Dedykowany skrypt 10-minutowy wykonuje eksport automatycznie. Aby ponowić
eksport albo przeanalizować Capture wykonany innym runnerem, użyj ścieżki
pokazanej po `PASS`:

```bash
cargo run --release \
  --manifest-path host_v2/Cargo.toml \
  -p sensor-host --bin capture-export -- \
  var/tmp/manual-rs485-hil-YYYYMMDDTHHMMSSZ/capture \
  var/tmp/manual-rs485-hil-YYYYMMDDTHHMMSSZ/analysis
```

Powstaną:

- `samples.csv` — cursor, surowe X/Y/Z, konfiguracja i dowody czasu;
- `events.csv` — gaps i continuity breaks;
- `windows.csv` — dokładne zakresy logicznych okien;
- `metadata.json` — identity, boot epoch i podsumowanie semantyki eksportu.

`device_time_us_estimate` jest interpolacją pomiędzy kotwicami czasu jednego
chunku. Należy zawsze zachować i sprawdzać `timing_quality_flags` oraz
`max_fit_residual_us`. Nie jest to jeszcze skorygowany czas UTC.

## MATLAB

```matlab
addpath("host_v2/analysis")
capture_quicklook("var/tmp/manual-rs485-hil-YYYYMMDDTHHMMSSZ/analysis")
```

Skrypt znajduje się w
[`capture_quicklook.m`](../../host_v2/analysis/capture_quicklook.m).

## Jupyter

Otwórz
[`capture_quicklook.ipynb`](../../host_v2/analysis/capture_quicklook.ipynb),
ustaw `analysis_dir` i uruchom komórki. Notebook korzysta z NumPy oraz
Matplotlib i nie wymaga Pandas.
