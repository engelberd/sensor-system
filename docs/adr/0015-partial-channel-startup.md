# ADR-0015: Niesprawny kanał nie blokuje startu pozostałych

- Data: 2026-11-11
- Status: zaakceptowane na żądanie użytkownika

## Problem

Bootstrap produkcyjny wymagał kompletnego inventory całej instalacji.
Brak node A powodował przerwanie procesu przed uruchomieniem zdrowej linii B.
Runtime izolował już błędy kanałów podczas pracy, ale start nie spełniał tej zasady.

## Decyzja

Każdy kanał przechodzi niezależny, ograniczony czasowo bootstrap. Brak portu,
timeout node, niezgodna tożsamość lub niekompletne dowody wykluczają ten kanał,
a jego ID, nazwa i przyczyna trafiają do jawnego zbioru startup faults.
Pozostałe kanały przechodzą zwykłe admission, recovery, durable Capture i start.
Host pracuje jako `degraded`; wadliwy kanał jest `faulted` i nie otrzymuje
workerów ani fałszywych cursorów lub dowodów czasu. Profil nadal go zawiera.

Collector pozostaje ścisły: kompletny wynik jest wymagany dla wszystkich node
każdego dopuszczonego kanału. Polecenie read-only `discover` nadal wymaga
pełnego inventory zadanej topologii; nie udaje pełnego sukcesu przy brakach.
Jeżeli nie da się dopuścić żadnego kanału, proces nie startuje i raportuje
przyczyny wszystkich awarii. Błędy wspólnego storage, integralności czasu,
konfiguracji hosta i paniki workerów pozostają globalnymi błędami.

## Granice

Izolacja przy bootstrapie dotyczy całego channel. Brak jednego node na wspólnej
magistrali nie uruchamia pozostałych node tej samej magistrali; niezależne
kanały pracują dalej. Kanał nieobecny przy starcie wymaga ponownego bootstrapu
przez restart hosta po usunięciu przyczyny. Automatyczne dołączanie takiego
kanału pozostaje otwartą bramką; istniejące odzyskiwanie node w działającym
workerze i wolne próby offline nie zmieniają się.

Wdrożenie ograniczamy do rpi-sanok2. Nie wyłączamy A w profilu i nie zmieniamy
firmware ani zasad board revision.
