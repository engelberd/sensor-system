# rpi-sanok2 — indywidualne timestampy, wdrożenie 2026-11-02

## Zakres i tożsamość

Wdrożono application protocol 6 / framing 3, firmware 0.5.11 dla board
revision 2 oraz hosta z Capture WAL v2 i Analysis CSV v3 (ADR-0012).

| Kanał | Hardware ID | Przywrócony output ODR | Config revision |
| --- | --- | ---: | ---: |
| line-a | 5c2823c24731dcd8 | 125 Hz | 5 |
| line-b | 32d32d8f5b796a99 | 250 Hz | 9 |

Zakres ±2 g, HPF=0, filtr Balanced, FIFO watermark=30, offsety XYZ=0.
Zachowano telemetry_interval_seconds=60 dla obu node. Lokalny tracked profil
instalacji został uzgodniony z rzeczywistym profilem Raspberry Pi.

Host został zbudowany natywnie ARM64 przed zatrzymaniem recordera. Stary proces
zamknął oba kanały z clean=true. Oba update A/B wykonały handshake poprzednią
binarką, walidację hardware ID i board revision, kontrolę pakietu 0.5.11 oraz
potwierdzenie trial boot nową binarką. Nie użyto obejścia board revision.

Release hosta: `sensor-host-0.5.0-dev-framing-3-application-6-384cf05b0ea4-86bdf6cf55f8-7e4337688a7e`.

## Dane i konfiguracja lokalna

Stare `var/recordings` i `var/analysis` pozostały zachowane. Nowy codzienny
Capture: `var/recordings-protocol6`, Time: podkatalog `time`, checkpointy:
podkatalog `.retention`, Analysis: `var/analysis-protocol6`.
Ścieżki są lokalnym overlay, nie nowymi domyślnymi produktu.

Backupy źródeł, konfiguracji, poprzednich narzędzi oraz dowody operacyjne są
na Raspberry Pi w ignorowanym `var/tmp/update-timestamps-0.5.11`.
Nagrania HIL i próby granicy nowego Capture zostały odłożone osobno do
`var/recordings-protocol6-hil-20261102` oraz
`var/recordings-protocol6-hil-boundary-20261102`.

## Potwierdzony tor sprzętowy

Eksport techniczny po zamknięciu WAL i replay zachował indywidualne,
nieregularne czasy. Odrębne konfiguracje przeanalizowano tylko dla sąsiednich
cursorów w tym samym segmencie czasu.

| Node / nominalne ODR | Liczba analizowanych odstępów | Mediana us | Min–max us |
| --- | ---: | ---: | ---: |
| A / 125 Hz | 4114 | 7945 | 7930–7960 |
| A / 31.25 Hz | 3644 | 31780 | 31768–31791 |
| B / 250 Hz | 7119 | 3964 | 3951–3980 |
| B / 7.8125 Hz | 378 | 126852 | 126838–126865 |

Wolny ODR B potwierdza transfer odstępów większych niż 65535 us. Nie są to
czasy `index / nominalny ODR`. Nie wyznaczono dokładności bezwzględnej względem
DRDY ani wzorca zegara; median nie należy traktować jako kalibracji.

## Ujawnione ograniczenia

1. Jedna próba Apply z Recording na B zakończyła się restartem jeszcze przed
   zapisaniem nowej konfiguracji. Diagnostyka zachowała InvalidSample z toru
   akwizycji; nie rozstrzygnięto przyczyny restartu. Ponowne przejęcie hosta
   odzyskało strumień. Jawne Maintenance -> Apply -> Resume pozwoliło poprawnie
   sprawdzić 7.8125 Hz i przywrócić 250 Hz. Wymaga osobnego fault-injection/HIL.
2. Nowy pusty katalog Capture przy niepustym journalu pozostałym po HIL
   wywołał liczne recovery i jawne gaps; sam status running/healthy nie
   potwierdza poprawnego zapisu. Codzienny Capture rozpoczęto dopiero po
   kontrolowanym restarcie obu node i potwierdzeniu nowych boot_epoch.
   Granica new_capture_boundary / Release wymaga osobnego testu i naprawy.

Tych prób nie włączono do codziennego zbioru. Brak błędów UART nie jest
wystarczającym kryterium: sprawdzamy przyrost persisted_sample_count,
missing_sample_count, gaps, breaks, recovery, backlog i storage.

## Odbiór końcowy

Po potwierdzonym restarcie node (nowe epoki: A `8d2ca4535a55803a`,
B `4ee98c6f6d5d1783`) obserwowano 30 s wzrostu trwałego zapisu bez luk,
missing samples, breaks, recovery ani błędów storage. Następnie wykonano
kontrolowany Stop, walidację całego małego nowego drzewa i eksport Analysis:

- A: 9671 próbek, zero gaps/missing; B: 19374 próbki, zero gaps/missing;
- każde zamknięcie zawiera jeden jawny ProcessingReset związany z kontrolowanym
  Stop — nie jest to utrata jednej próbki;
- oba CSV mają schema v3, sześć wymaganych kolumn i ściśle rosnące czasy;
- exporter: created=2, failed=0;
- ponowne uruchomienie w tym samym katalogu zachowało recovery cursorów:
  A 2099 i B 4246 nowych próbek, zero gaps/missing, recovery, timeoutów,
  backpressure i błędów storage w końcowym snapshotcie;
- doctor: 21 OK, zero ostrzeżeń i błędów;
- recorder, dashboard, service-control i timer eksportu są active.

To krótki odbiór wdrożenia, nie długotrwały soak, kalibracja ani potwierdzenie
usunięcia dwóch problemów opisanych wyżej. Powtarzalna bezpieczna procedura
zmiany ODR: Maintenance -> Apply -> Resume. Przy zmianie na zupełnie pusty
Capture należy na razie przed startem potwierdzić nową epokę node.

## Uproszczenie CSV — 2026-11-02

Na życzenie użytkownika usunięto pochodną kolumnę `time_from_start_s`.
Bieżący CSV v3 ma pięć kolumn: sample_sequence, device_time_us, XYZ.
Loader wylicza względne sekundy z różnicy integer timestampów.
Eksporter zbudowano na ARM64 i podmieniono bez zatrzymywania recordera.
Cztery istniejące produkty ujednolicono po backupie w
`var/tmp/csv-one-time-update/csv-products.before.tar.gz`; nowy eksport A
potwierdził pięć kolumn. Numery próbek, timestampy i XYZ pozostały zachowane.

Podczas tej operacji zaobserwowano konfigurację B 7.8125 Hz i rewizje 10/11
w jednym oknie. Ustawień pomiaru nie zmieniano. Eksporter odrzucił ten jeden
segment jako MixedAnalysisConfiguration; pozostałe produkty zostały
zweryfikowane. Doctor wykazał różnicę fingerprintu konfiguracji względem
bieżącego procesu, po zmianie konfiguracji instalacji. Nie restartowano
recordera w ramach zmiany samego CSV.
