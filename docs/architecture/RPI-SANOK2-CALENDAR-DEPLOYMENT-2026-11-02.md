# rpi-sanok2 — wdrożenie wspólnych okien WAL/CSV

Data: 2026-11-02. Zakres: host, bez aktualizacji firmware i bez zmiany pinów.
Polityka: [ADR-0013](../adr/0013-shared-capture-calendar.md).

## Instalacja

Źródła skompilowano natywnie na ARM64 przed zatrzymaniem pomiaru. Zachowano
lokalny overlay, codzienne Capture/Analysis i istniejące epoki node.
Backup kodu/konfiguracji i narzędzi oraz surowe dowody testów pozostają poza
Git, w `var/tmp/shared-calendar-update` na Raspberry.

Release:
`sensor-host-0.5.0-dev-framing-3-application-6-ecab5c14b7fd-c7a28cdf8734-7e4337688a7e`.

Początek pracy nowego release: 21:23 CET. Zachowano A 125 Hz i B 7.8125 Hz,
range 2 g, balanced, HPF 0, watermark 30, offsety 0. `doctor` po aktualizacji:
21 OK, zero ostrzeżeń i błędów. Wcześniejsza rozbieżność fingerprintu zniknęła.

## Zmiana konfiguracji i restart

Na B wykonano Maintenance -> Apply -> Resume, zmieniając wyłącznie offset X
0 -> 1 -> 0 (rewizje 11 -> 12 -> 13). Zapis testowego offsetu pozostaje
jawnym osobnym oknem z konfiguracją, nie został usunięty z historii.
Przywrócenie offsetu potwierdzono odczytem konfiguracji z node.

Porównanie technicznego eksportu WAL z Analysis CSV potwierdziło dla każdego
nowego zamkniętego okna identyczne pary `(cursor, device_time_us)`, liczbę
próbek, pojedynczą rewizję, zakres manifestu i właściwy katalog dnia.
Kontrolowany restart hosta o 21:24:27 zachował epoki i przyległość cursorów.
Walidacja całego istniejącego drzewa po restarcie: 62 zamknięte okna,
2 182 958 próbek, zero brakujących próbek. Jawne ProcessingReset/zmiany
segmentu czasu podczas czynności serwisowych pozostają w ledgerze.

W oknach B związanych ze zmianą konfiguracji niepewność kotwicy ma wartość
`null`: zastosowano ograniczenie cofnięcia oszacowania czasu względem
poprzedniego okna. Nie jest to deklaracja dokładnego UTC. Indywidualne
monotoniczne timestampy Pico zachowano bez modyfikacji.

## Znane dane historyczne

Eksporter nadal odrzuca historyczny segment B ordinal 3, zawierający rewizje
10 i 11 w jednym WAL, zapisany przed tą aktualizacją. Raportuje
`MixedAnalysisConfiguration`; nie przepisano go i nie ukryto błędu. Nie jest
to uszkodzenie nowych okien ani utrata surowego WAL. Stanowi osobne ograniczenie
historycznego eksportu, także przy kolejnych uruchomieniach timera.

## Rzeczywista granica 21:30 i odbiór końcowy

Oba kanały automatycznie zamknęły częściowe okno i rozpoczęły następne na
pierwszej próbce po granicy 21:30. Potwierdzono rzeczywisty podział jednej
paczki transferowej pomiędzy plikami:

| Kanał | Sąsiednie okna | Podział paczki | Ostatnia próbka przed granicą | Pierwsza próbka po granicy |
| --- | --- | --- | --- | --- |
| A, 125 Hz | 28 -> 29 | 28 + 6 | 4693 us | 3251 us |
| B, 7.8125 Hz | 34 -> 35 | 1 + 4 | 83743 us | 43104 us |

Odległości odnoszą się do zamrożonej granicy device/UTC, nie są pomiarem
bezwzględnej dokładności synchronizacji. Pierwsza próbka kolejnych plików ma
oszacowany czas lokalny odpowiednio 21:30:00.003251 i 21:30:00.043104, a nowe
deadline wynoszą 21:40. Sufiksy i prefiksy mają identyczne pola źródłowego
`retained_from` i `produced`, przylegające cursory i nie zmieniają timestampów.

Po granicy wykonano kontrolowany Stop, eksport i ponowny Start. Sprawdzono
łącznie 9 nowych zamkniętych okien: 58 336 próbek, każdą porównano z CSV.
Całe historyczne drzewo: 66 zamkniętych okien, 2 229 855 próbek, zero gaps.
Jawne breaks po Stop/Maintenance nie są brakującymi próbkami.

O 21:30:51 oba node ponownie nagrywały, z przyrostem 3765 próbek A i 220 B
od ostatniego startu. Zero nowych gaps, missing samples, breaks, retry,
recovery, timeoutów, backpressure i błędów storage. Recorder, dashboard,
service-control i timer eksportu active; doctor 21 OK. Offsety B ponownie 0,
rewizja 13, pozostałe parametry zachowane.

To krótki HIL wdrożenia. Zmianę dnia i DST sprawdzono testami programowymi;
nie oczekiwano do fizycznej północy ani zmiany czasu. Nie wykonywano odcięcia
zasilania podczas zapisu fragmentu; recovery takiego prefiksu pokrywają testy
programowe. Nie przeprowadzono kalibracji czasu ani długotrwałego soak.

## Czytelne nazwy produktów

Po uzgodnieniu z użytkownikiem usunięto hardware/boot/ordinal z nazwy CSV.
Obowiązuje minuta pierwszej próbki i sufiks `_2`, `_3` przy kolizji. Te same
nazwy bazowe dotyczą JSON, Events i Telemetry. Tożsamość produktu pozostaje
w JSON; nie zmieniono WAL, granic okien ani timestampów.

Zaktualizowano wyłącznie eksporter, bez restartowania recordera. Nowy release:
`sensor-host-0.5.0-dev-framing-3-application-6-ecab5c14b7fd-0aed4c177d7e-7e4337688a7e`.
15 istniejących zestawów plików (266 018 próbek) otrzymało krótsze nazwy;
SHA-256 CSV, Events i Telemetry pozostały identyczne. Backup:
`var/tmp/shared-calendar-update/csv-before-short-names.tar.gz` na Raspberry.

Test ARM w osobnym katalogu wymusił kolizję istniejącego CSV na obu kanałach:
nowe produkty otrzymały `_2`, pliki zastane pozostały nienaruszone, a dane
były identyczne z codziennymi CSV. Ponowny codzienny eksport: created=0,
bez duplikatów po zmianie nazw; jedynym failed pozostaje opisany wyżej stary
mieszany segment. Doctor: 21 OK. Testy lokalne: 468 Rust, 186 Python, Clippy,
format i kompilacja release zakończone poprawnie.
