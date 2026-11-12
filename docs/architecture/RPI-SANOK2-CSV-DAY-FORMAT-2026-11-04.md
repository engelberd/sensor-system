# rpi-sanok2 — format daty katalogów Analysis

2026-11-04: katalogi Analysis używają `DD_MM_YYYY`, spójnie z datą w nazwach
produktów. Data nadal pochodzi z pierwszej próbki w strefie instalacji.
WAL, konfiguracja node i granice okien nie zmieniły się.

Zaktualizowano wyłącznie eksporter, bez restartowania recordera. Release:
`sensor-host-0.5.0-dev-framing-3-application-6-ecab5c14b7fd-c49bba95f9b9-7e4337688a7e`.

Istniejące katalogi przeniesiono w tym samym filesystemie:

- `2026-11-02` -> `02_11_2026`: 164 pliki;
- `2026-11-03` -> `03_11_2026`: 1152 pliki;
- `2026-11-04` -> `04_11_2026`: 608 plików.

Sprawdzono zachowanie względnych ścieżek, inode, rozmiarów i mtime wszystkich
1924 plików. Plan odwracalnej migracji i dowody znajdują się poza Git,
w `var/tmp/csv-day-format-20261104` na Raspberry.

Nowy codzienny eksport utworzył po jednym CSV w `04_11_2026` dla każdego
kanału. W osobnym katalogu testowym ponowiono eksport tych samych okien:
75 516 próbek A i 4729 B, identyczne SHA-256 CSV względem codziennych produktów.
Drugie uruchomienie nie utworzyło żadnych nowych produktów. Doctor: 21 OK.
Historyczny mieszany segment B ordinal 3 nadal jest jawnie odrzucany,
zgodnie z wcześniejszym raportem; nie jest skutkiem zmiany katalogów.

Testy lokalne: 468 Rust, 186 Python, Clippy, format i kompilacja release
zakończyły się poprawnie. Test przekroczenia północy sprawdza nowy format
nazw obu dni oraz dotychczasową ciągłość WAL/CSV.
