# Tymczasowy profil pojemności node v2

- Status: profil integracyjny, pierwszy HIL jednej linii zaliczony
- Ostatnia aktualizacja: 2026-10-06

## Dlaczego profil istnieje już teraz

Szablony firmware wymagają pojemności znanych podczas kompilacji. Jedno miejsce
konfiguracji pozwala zbudować rzeczywistą kompozycję i policzyć RAM bez
rozrzucania liczb po klasach. Nie oznacza to zamrożenia wartości produktowych.

Profil startowy:

| Element | Pojemność |
|---|---:|
| acquisition buffer | 256 próbek |
| ingress core 0 → core 1 | 128 zdarzeń |
| measurement journal | 2048 próbek |
| gap/continuity storage | 64 + 64 wpisy |
| sample chunk | 48 próbek |
| command/result mailbox | 4 + 4 wpisy |
| lifecycle result cache | 8 wyników |
| response outbox | 2 ramki |
| ingress drain na jeden poll | 16 zdarzeń |
| stos core 1 | 8 KiB |

Na obecnym ABI ingress zajmuje około 7 KiB, a storage journala około 69 KiB.
Limity są sprawdzane przez `static_assert`; nie obejmują stosu, sterowników ani
pozostałych obiektów obrazu.

Aktualny build 0.5.5 `sensor-system-node-v2-integration.elf` i obrazy slotowe
mają 128404 B BSS. Dla porównania obecny `sensor-system-node-direct.elf` ma
289772 B BSS.
Pomiar obejmuje pełne statyczne obiekty obrazu. Pierwszy HIL trial wykazał, że
domyślne 2 KiB stosu core 1 z Pico SDK nie daje wiarygodnej granicy dla nowego
endpointu. Profil jawnie rezerwuje 8 KiB i wymaga pomiaru high-water mark pod
najgorszym obciążeniem.

2048 próbek oznacza około 8,2 s retencji przy 250 próbkach/s, 16,4 s przy 125
próbkach/s i 32,8 s przy 62,5 próbkach/s. To wartości do integracji, bez
gwarancji dla docelowej rotacji wielu node.

Fizyczny test jednej linii przy 115200 bit/s potwierdził handshake, 254 próbki,
durable `fsync`, release dokładnie do cursora 255 oraz przejście do
`Maintenance` bez gap. Ten wynik sprawdza poprawność granic, lecz nie wyznacza
jeszcze pojemności dla wielu node, długiego przewodu ani opóźnień storage.

## Dane wymagane z HIL

Przed oznaczeniem profilu jako produkcyjny trzeba zmierzyć:

- maksymalne oraz percentylowe zajęcie ingress;
- maksymalną przerwę pomiędzy skutecznymi pull/release dla każdego node;
- czas obsługi transportu przy błędach CRC, timeoutach i retransmisjach;
- wpływ USB observera i trybu Diagnostic na opóźnienie core 1;
- zapas stosu i całkowite SRAM dla rzeczywistego obrazu;
- warianty jednego szybkiego node oraz wielu wolniejszych node na kanale.

Strojenie zmienia wyłącznie `RuntimeCapacityProfile`. Nie może zmieniać formatu
protokołu, właścicieli stanu ani polityki ciągłości danych.
